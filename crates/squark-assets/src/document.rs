// SPDX-License-Identifier: MIT
use squark_journal::{StringTable, Utf16};
use squark_types::NodeId;
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DocumentHeader, QuirksMode,
    SCRIPT_ENTRY_SIZE, ScriptEntry, ScriptFlags, ScriptKind,
};

use crate::{PackError, range, u32_at};

#[derive(Debug)]
pub struct InitialDocument<'a> {
    pub id: NodeId,
    pub quirks: QuirksMode,
    bytes: &'a [u8],
    directory: &'a [[u8; BATCH_DIR_ENTRY_SIZE]],
    scripts: &'a [[u8; SCRIPT_ENTRY_SIZE]],
    strings: StringTable<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct Script<'a> {
    pub kind: ScriptKind,
    pub node: NodeId,
    pub defer: bool,
    pub async_: bool,
    pub payload: Utf16<'a>,
    pub base_url: Utf16<'a>,
}

impl<'a> InitialDocument<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PackError> {
        let header = DocumentHeader::decode(
            bytes
                .first_chunk()
                .ok_or(PackError::Invalid("document header"))?,
        )?;
        let section = |offset, size| {
            range(offset, size, bytes.len())
                .filter(|slice| slice.start >= DOCUMENT_HEADER_SIZE)
                .ok_or(PackError::Invalid("document section range"))
        };
        let directory = section(
            header.batch_dir_offset,
            u64::from(header.batch_count) * BATCH_DIR_ENTRY_SIZE as u64,
        )?;
        let strings = section(header.script_strings_offset, header.script_strings_len)?;
        let scripts = section(
            header.script_table_offset,
            u64::from(header.script_count) * SCRIPT_ENTRY_SIZE as u64,
        )?;
        let doc = Self {
            id: header.document_node_id,
            quirks: header.quirks_mode,
            bytes,
            directory: bytes[directory.clone()].as_chunks().0,
            scripts: bytes[scripts.clone()].as_chunks().0,
            strings: parse_strings(&bytes[strings.clone()])?,
        };
        let mut sections = Vec::with_capacity(doc.directory.len() + 3);
        sections.extend([directory, strings, scripts]);
        for entry in doc.directory {
            let entry = BatchDirEntry::decode(entry);
            let batch = section(entry.offset, entry.len)?;
            // The journal decoder checks the record and string tables during replay.
            if batch.len() < 20 {
                return Err(PackError::Invalid("journal batch length"));
            }
            sections.push(batch);
        }
        sections.retain(|section| !section.is_empty());
        sections.sort_unstable_by_key(|section| section.start);
        if sections.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(PackError::Invalid("overlapping document sections"));
        }
        for raw in doc.scripts {
            doc.parse_script(raw)?;
        }
        Ok(doc)
    }

    pub fn batches(&self) -> impl ExactSizeIterator<Item = &'a [u8]> + '_ {
        self.directory.iter().map(|entry| {
            let entry = BatchDirEntry::decode(entry);
            let bounds =
                range(entry.offset, entry.len, self.bytes.len()).expect("validated batch range");
            &self.bytes[bounds]
        })
    }

    pub fn scripts(&self) -> impl ExactSizeIterator<Item = Script<'a>> + '_ {
        self.scripts
            .iter()
            .map(|entry| self.parse_script(entry).expect("validated script"))
    }

    fn parse_script(&self, raw: &[u8; SCRIPT_ENTRY_SIZE]) -> Result<Script<'a>, PackError> {
        let entry = ScriptEntry::decode(raw)?;
        let string = |index| {
            self.strings
                .get(index)
                .ok_or(PackError::Invalid("script string index"))
        };
        Ok(Script {
            kind: entry.kind,
            node: entry.node_id,
            defer: entry.flags.contains(ScriptFlags::DEFER),
            async_: entry.flags.contains(ScriptFlags::ASYNC),
            payload: string(entry.payload_str)?,
            base_url: string(entry.base_url_str)?,
        })
    }
}

fn parse_strings(bytes: &[u8]) -> Result<StringTable<'_>, PackError> {
    let error = PackError::Invalid("script string table");
    let (header, data) = bytes.split_first_chunk::<8>().ok_or(error)?;
    StringTable::new(data, u32_at(header, 0), u32_at(header, 4))
        .map_err(|_| PackError::Invalid("script string table"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = b"SQDOC\0\0\0".to_vec();
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(64u16.to_le_bytes());
        bytes.extend([2u32, 0x1000001, 1].into_iter().flat_map(u32::to_le_bytes));
        bytes.extend([64u64, 80, 22].into_iter().flat_map(u64::to_le_bytes));
        bytes.extend([1u32, 0].into_iter().flat_map(u32::to_le_bytes));
        bytes.extend([102u64, 118, 20].into_iter().flat_map(u64::to_le_bytes));
        bytes.extend([2u32, 1, 0, 1, 1].into_iter().flat_map(u32::to_le_bytes));
        bytes.extend(0xd800u16.to_le_bytes());
        bytes.extend([0x100u32, 2, 0, 1].into_iter().flat_map(u32::to_le_bytes));
        bytes.extend([0u32, 0, 0, 1, 0].into_iter().flat_map(u32::to_le_bytes));
        bytes
    }

    #[test]
    fn reads_quirks_node_batches_and_utf16_without_transcoding() {
        let bytes = fixture();
        let doc = InitialDocument::parse(&bytes).unwrap();
        assert_eq!(doc.quirks, QuirksMode::Quirks);
        assert_eq!(doc.id, NodeId::new(1, 1).unwrap());
        assert_eq!(doc.batches().collect::<Vec<_>>(), vec![&bytes[118..]]);
        let script = doc.scripts().next().unwrap();
        assert!(script.defer);
        assert_eq!(script.payload.units().collect::<Vec<_>>(), vec![0xd800]);
        assert_eq!(script.base_url.units().len(), 0);
    }

    #[test]
    fn rejects_truncation_overflow_overlap_reserved_bits_and_bad_string_offsets() {
        let valid = fixture();
        for len in 0..valid.len() {
            assert!(
                InitialDocument::parse(&valid[..len]).is_err(),
                "length {len}"
            );
        }
        for (offset, value) in [
            (8, 2),
            (12, 4),
            (16, 0),
            (64, 90),
            (68, 255),
            (88, 2),
            (103, 4),
            (114, 2),
        ] {
            let mut bytes = valid.clone();
            bytes[offset] = value;
            assert!(InitialDocument::parse(&bytes).is_err(), "offset {offset}");
        }
    }
}
