// SPDX-License-Identifier: MIT

//! Replay the packed static document into the squark-dom realm before app startup.

use std::fmt;
use std::ops::Range;

use squark_journal::{BatchDecoder, DecodedBatch, JournalError, StringTable, Utf16};
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DocumentHeader, PackFormatError,
    SCRIPT_ENTRY_SIZE, ScriptEntry,
};

#[derive(Debug)]
pub enum BootstrapError {
    Document(&'static str),
    Header(PackFormatError),
    Journal { batch: usize, error: JournalError },
    JavaScript(String),
    V8,
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document(reason) => write!(f, "invalid document.bin: {reason}"),
            Self::Header(error) => write!(f, "invalid document.bin header: {error}"),
            Self::Journal { batch, error } => write!(f, "document.bin batch {batch}: {error}"),
            Self::JavaScript(message) => write!(f, "squark-dom bootstrap: {message}"),
            Self::V8 => write!(f, "could not build the squark-dom bootstrap in V8"),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// Validated regions of document.bin used for shared startup replay.
#[derive(Debug)]
pub(crate) struct PackedDocument<'a> {
    pub header: DocumentHeader,
    pub batches: Vec<&'a [u8]>,
    scripts: Vec<ScriptEntry>,
    strings: StringTable<'a>,
}

impl<'a> PackedDocument<'a> {
    pub fn read(bytes: &'a [u8]) -> Result<Self, BootstrapError> {
        use BootstrapError::Document;

        let (header, _) = bytes
            .split_first_chunk::<DOCUMENT_HEADER_SIZE>()
            .ok_or(Document("truncated header"))?;
        let header = DocumentHeader::decode(header).map_err(BootstrapError::Header)?;

        let table_range = |offset, count: u32, entry_size| {
            let length = u64::from(count) * entry_size as u64;
            region(bytes.len(), offset, length)
        };
        let directory = table_range(
            header.batch_dir_offset,
            header.batch_count,
            BATCH_DIR_ENTRY_SIZE,
        )?;
        let strings = region(
            bytes.len(),
            header.script_strings_offset,
            header.script_strings_len,
        )?;
        let scripts = table_range(
            header.script_table_offset,
            header.script_count,
            SCRIPT_ENTRY_SIZE,
        )?;

        let string_bytes = &bytes[strings.clone()];
        let string_header = string_bytes
            .get(..8)
            .ok_or(Document("truncated script string table"))?;
        let table = StringTable::new(
            &string_bytes[8..],
            word(string_header, 0),
            word(string_header, 4),
        )
        .map_err(|_| Document("invalid script string table"))?;

        let mut ranges = vec![
            0..DOCUMENT_HEADER_SIZE,
            directory.clone(),
            strings,
            scripts.clone(),
        ];
        let mut batches = Vec::with_capacity(header.batch_count as usize);
        for entry in bytes[directory].as_chunks::<BATCH_DIR_ENTRY_SIZE>().0 {
            let entry = BatchDirEntry::decode(entry);
            let range = region(bytes.len(), entry.offset, entry.len)?;
            batches.push(&bytes[range.clone()]);
            ranges.push(range);
        }
        ranges.retain(|range| !range.is_empty());
        ranges.sort_unstable_by_key(|range| range.start);
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(Document("overlapping document regions"));
        }

        let scripts = bytes[scripts]
            .as_chunks::<SCRIPT_ENTRY_SIZE>()
            .0
            .iter()
            .map(|bytes| {
                let entry =
                    ScriptEntry::decode(bytes).map_err(|_| Document("invalid script entry"))?;
                if table.get(entry.payload_str).is_none() || table.get(entry.base_url_str).is_none()
                {
                    return Err(Document("invalid script string index"));
                }
                Ok(entry)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            header,
            batches,
            scripts,
            strings: table,
        })
    }

    pub fn scripts(&self) -> impl Iterator<Item = (ScriptEntry, Utf16<'a>, Utf16<'a>)> + '_ {
        self.scripts.iter().map(|&entry| {
            (
                entry,
                self.strings.get(entry.payload_str).unwrap(),
                self.strings.get(entry.base_url_str).unwrap(),
            )
        })
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn region(len: usize, offset: u64, length: u64) -> Result<Range<usize>, BootstrapError> {
    if let Some(end) = offset.checked_add(length)
        && end <= len as u64
    {
        return Ok(offset as usize..end as usize);
    }
    Err(BootstrapError::Document("region outside document.bin"))
}

/// Calls the squark-dom ESM export `bootstrapDocument` with a fresh Journal from
/// the same context. The caller installs the returned document in its realm.
pub fn bootstrap_document<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bytes: &[u8],
    replay: v8::Local<'s, v8::Function>,
    journal: v8::Local<'s, v8::Object>,
) -> Result<v8::Local<'s, v8::Object>, BootstrapError> {
    let document = PackedDocument::read(bytes)?;
    replay_document(scope, &document, replay, journal).map(|(document, _)| document)
}

pub(crate) fn replay_document<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    document: &PackedDocument<'_>,
    replay: v8::Local<'s, v8::Function>,
    journal: v8::Local<'s, v8::Object>,
) -> Result<(v8::Local<'s, v8::Object>, BatchDecoder), BootstrapError> {
    let mut decoder = BatchDecoder::new(document.header.document_node_id)
        .map_err(|_| BootstrapError::Document("invalid document node id"))?;
    let mut batches = Vec::with_capacity(document.batches.len());
    for (index, bytes) in document.batches.iter().enumerate() {
        let batch = decoder
            .validate(bytes)
            .map_err(|error| BootstrapError::Journal {
                batch: index,
                error,
            })?;
        batches.push(batch_value(scope, &batch)?.into());
        batch.commit();
    }

    let id = v8::Integer::new_from_unsigned(scope, document.header.document_node_id.raw());
    let quirks = v8::Integer::new_from_unsigned(scope, document.header.quirks_mode.to_flags());
    let batches = v8::Array::new_with_elements(scope, &batches);
    let receiver = v8::undefined(scope);
    v8::tc_scope!(let scope, scope);
    let Some(result) = replay.call(
        scope,
        receiver.into(),
        &[journal.into(), id.into(), quirks.into(), batches.into()],
    ) else {
        return Err(match scope.exception() {
            Some(exception) => BootstrapError::JavaScript(exception.to_rust_string_lossy(scope)),
            None => BootstrapError::V8,
        });
    };
    let document = v8::Local::try_from(result).map_err(|_| BootstrapError::V8)?;
    Ok((document, decoder))
}

fn batch_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    batch: &DecodedBatch<'_, '_>,
) -> Result<v8::Local<'s, v8::Array>, BootstrapError> {
    let record_bytes =
        v8::ArrayBuffer::new_backing_store_from_boxed_slice(batch.record_bytes().into())
            .make_shared();
    let records = v8::ArrayBuffer::with_backing_store(scope, &record_bytes);
    let table = batch.strings();
    let mut strings = Vec::with_capacity(table.len());
    let mut units = Vec::new();
    for index in 0..table.len() {
        units.clear();
        units.extend(table.get(index as u32).expect("table index").units());
        let value = v8::String::new_from_two_byte(scope, &units, v8::NewStringType::Normal)
            .ok_or(BootstrapError::V8)?;
        strings.push(value.into());
    }
    let strings = v8::Array::new_with_elements(scope, &strings);
    Ok(v8::Array::new_with_elements(
        scope,
        &[records.into(), strings.into()],
    ))
}
