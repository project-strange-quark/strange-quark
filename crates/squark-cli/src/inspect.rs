// SPDX-License-Identifier: MIT
//! Validated reader and inspector for `.sqpak` archives, appended runtime
//! binaries, and `document.bin` files (`squark inspect --dump-pack`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::Serialize;
use squark_journal::BatchDecoder;
use squark_types::NodeId;
use squark_types::journal::{AtomKind, Opcode, Record};
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DOCUMENT_PATH, DocumentHeader,
    MIME_CUSTOM, PACK_ENTRY_SIZE, PACK_HEADER_SIZE, PackEntry, PackHeader, PackTrailer, QuirksMode,
    SCRIPT_ENTRY_SIZE, SQDOC_MAGIC, SQPK_MAGIC, ScriptEntry, ScriptFlags, ScriptKind,
    TRAILER_MAGIC, TRAILER_SIZE, decode_utf16_string_table, fnv1a_64, is_valid_pack_path,
    mime_from_id,
};

use crate::error::CliError;

/// Full inspection report for a `.sqpak`, appended runtime executable, or standalone `document.bin`.
#[derive(Clone, Debug, Serialize)]
pub struct PackInspection {
    pub container: ContainerKind,
    pub file_bytes: usize,
    pub trailer: Option<TrailerSummary>,
    pub pack: Option<PackTableSummary>,
    pub document: Option<DocumentSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerKind {
    StandaloneSqpak,
    AppendedBinary,
    StandaloneDocumentBin,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrailerSummary {
    pub pack_offset: u64,
    pub pack_len: u32,
    pub pack_crc32: u32,
    pub crc_valid: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackTableSummary {
    pub entry_count: u32,
    pub strings_offset: u32,
    pub strings_len: u32,
    pub data_offset: u32,
    pub total_bytes: usize,
    pub crc32: u32,
    pub entries: Vec<PackEntrySummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackEntrySummary {
    pub path: String,
    pub path_hash: u64,
    pub mime: String,
    pub mime_id: u16,
    pub data_off: u32,
    pub data_len: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocumentSummary {
    pub quirks_mode: &'static str,
    pub document_node_id: u32,
    pub batch_count: u32,
    pub total_records: usize,
    pub atom_count: usize,
    pub inline_styles: Vec<InlineStyleSummary>,
    pub scripts: Vec<ScriptSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InlineStyleSummary {
    pub node_id: u32,
    pub css: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScriptSummary {
    pub kind: &'static str,
    pub defer: bool,
    pub r#async: bool,
    pub node_id: u32,
    pub payload: String,
    pub base_url: String,
}

/// Owned snapshot of one validated journal batch for tests and tooling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedBatch {
    pub records: Vec<Record>,
    pub strings: Vec<Vec<u16>>,
}

impl ValidatedBatch {
    pub fn string_lossy(&self, idx: u32) -> Option<String> {
        self.strings
            .get(idx as usize)
            .map(|units| String::from_utf16_lossy(units))
    }
}

/// Decoded in-memory view of a validated `document.bin` for tests and tooling.
#[derive(Clone, Debug)]
pub struct ValidatedDocument {
    pub header: DocumentHeader,
    pub batches: Vec<ValidatedBatch>,
    pub script_strings: Vec<Vec<u16>>,
    pub scripts: Vec<ScriptEntry>,
    pub atoms: HashMap<u32, (AtomKind, Vec<u16>)>,
}

/// Reads and validates a `.sqpak`, appended runtime binary, `document.bin`, or
/// the pack of a macOS app bundle (given the bundle or its executable).
pub fn inspect_path(path: &Path) -> Result<PackInspection, CliError> {
    let bundled = crate::bundle::pack_in_bundle(path);
    let path = bundled.as_deref().unwrap_or(path);
    let bytes = fs::read(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    inspect_bytes(&bytes)
}

/// Validates and inspects raw bytes of a `.sqpak`, appended binary, or `document.bin`.
pub fn inspect_bytes(bytes: &[u8]) -> Result<PackInspection, CliError> {
    if bytes.starts_with(&SQPK_MAGIC) {
        let (pack, doc_bytes) = validate_sqpak(bytes)?;
        let document = match doc_bytes {
            Some(doc) => Some(summarize_document(&validate_document_bin(doc)?)),
            None => None,
        };
        return Ok(PackInspection {
            container: ContainerKind::StandaloneSqpak,
            file_bytes: bytes.len(),
            trailer: None,
            pack: Some(pack),
            document,
        });
    }

    if bytes.starts_with(&SQDOC_MAGIC) {
        let doc = validate_document_bin(bytes)?;
        return Ok(PackInspection {
            container: ContainerKind::StandaloneDocumentBin,
            file_bytes: bytes.len(),
            trailer: None,
            pack: None,
            document: Some(summarize_document(&doc)),
        });
    }

    if bytes.len() >= TRAILER_SIZE
        && bytes[bytes.len() - TRAILER_SIZE..].starts_with(&TRAILER_MAGIC)
    {
        let trailer_slice: &[u8; TRAILER_SIZE] = bytes[bytes.len() - TRAILER_SIZE..]
            .try_into()
            .expect("trailer slice length checked");
        let trailer = PackTrailer::decode(trailer_slice)?;
        let expected_end = (bytes.len() - TRAILER_SIZE) as u64;
        let pack_end = trailer
            .pack_offset
            .checked_add(u64::from(trailer.pack_len))
            .ok_or_else(|| CliError::PackValidation("trailer pack range overflow".to_owned()))?;
        if pack_end != expected_end {
            return Err(CliError::PackValidation(format!(
                "trailer pack range {}..{} does not end at trailer offset {}",
                trailer.pack_offset, pack_end, expected_end
            )));
        }
        let pack_slice = &bytes[trailer.pack_offset as usize..pack_end as usize];
        let actual_crc = crc32fast::hash(pack_slice);
        if actual_crc != trailer.pack_crc32 {
            return Err(CliError::PackValidation(format!(
                "pack CRC32 mismatch: trailer has {:#010x}, computed {:#010x}",
                trailer.pack_crc32, actual_crc
            )));
        }
        let (pack, doc_bytes) = validate_sqpak(pack_slice)?;
        let document = match doc_bytes {
            Some(doc) => Some(summarize_document(&validate_document_bin(doc)?)),
            None => None,
        };
        return Ok(PackInspection {
            container: ContainerKind::AppendedBinary,
            file_bytes: bytes.len(),
            trailer: Some(TrailerSummary {
                pack_offset: trailer.pack_offset,
                pack_len: trailer.pack_len,
                pack_crc32: trailer.pack_crc32,
                crc_valid: true,
            }),
            pack: Some(pack),
            document,
        });
    }

    Err(CliError::PackValidation(
        "unrecognized file: expected SQPK header, SQDOC header, or SQPACK EOF trailer".to_owned(),
    ))
}

/// Validates a `.sqpak` slice completely and returns its summary plus `/__squark/document.bin` if present.
pub fn validate_sqpak(bytes: &[u8]) -> Result<(PackTableSummary, Option<&[u8]>), CliError> {
    if bytes.len() < PACK_HEADER_SIZE {
        return Err(CliError::PackValidation(format!(
            "pack shorter than 32-byte header ({} bytes)",
            bytes.len()
        )));
    }
    let header_arr: &[u8; PACK_HEADER_SIZE] = bytes[..PACK_HEADER_SIZE]
        .try_into()
        .expect("header slice length checked");
    let header = PackHeader::decode(header_arr)?;

    let index_start = header.index_offset as usize;
    let index_len = (header.entry_count as usize)
        .checked_mul(PACK_ENTRY_SIZE)
        .ok_or_else(|| CliError::PackValidation("index size overflow".to_owned()))?;
    let index_end = index_start
        .checked_add(index_len)
        .ok_or_else(|| CliError::PackValidation("index end overflow".to_owned()))?;

    let strings_start = header.strings_offset as usize;
    let strings_end = strings_start
        .checked_add(header.strings_len as usize)
        .ok_or_else(|| CliError::PackValidation("string pool end overflow".to_owned()))?;

    let data_start = header.data_offset as usize;

    if index_end > strings_start || strings_end > data_start || data_start > bytes.len() {
        return Err(CliError::PackValidation(format!(
            "pack sections overlap or exceed file bounds (index {index_start}..{index_end}, strings {strings_start}..{strings_end}, data {data_start}..{})",
            bytes.len()
        )));
    }

    let string_pool = &bytes[strings_start..strings_end];
    let data_section = &bytes[data_start..];

    let mut entries = Vec::with_capacity(header.entry_count as usize);
    let mut prev_path: Option<String> = None;
    let mut doc_slice: Option<&[u8]> = None;

    for i in 0..(header.entry_count as usize) {
        let off = index_start + i * PACK_ENTRY_SIZE;
        let entry_arr: &[u8; PACK_ENTRY_SIZE] = bytes[off..off + PACK_ENTRY_SIZE]
            .try_into()
            .expect("index entry length checked");
        let entry = PackEntry::decode(entry_arr)?;

        let path_start = entry.path_off as usize;
        let path_end = path_start
            .checked_add(usize::from(entry.path_len))
            .ok_or_else(|| CliError::PackValidation("entry path range overflow".to_owned()))?;
        let path_bytes = string_pool.get(path_start..path_end).ok_or_else(|| {
            CliError::PackValidation(format!("entry {i} path range out of string pool bounds"))
        })?;
        let path = std::str::from_utf8(path_bytes)
            .map_err(|_| CliError::PackValidation(format!("entry {i} path is not valid UTF-8")))?
            .to_owned();
        if !is_valid_pack_path(&path) {
            return Err(CliError::InvalidPackPath { path });
        }
        if let Some(prev) = &prev_path {
            match path.as_bytes().cmp(prev.as_bytes()) {
                std::cmp::Ordering::Equal => {
                    return Err(CliError::DuplicatePackPath { path });
                }
                std::cmp::Ordering::Less => {
                    return Err(CliError::PackValidation(format!(
                        "pack index is not sorted: {path:?} precedes {prev:?}"
                    )));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        let expected_hash = fnv1a_64(path.as_bytes());
        if entry.path_hash != expected_hash {
            return Err(CliError::PackValidation(format!(
                "entry {path:?} hash mismatch: stored {:#018x}, computed {:#018x}",
                entry.path_hash, expected_hash
            )));
        }

        let mime = if entry.mime_id == MIME_CUSTOM {
            let m_start = entry.mime_off as usize;
            let m_end = m_start
                .checked_add(usize::from(entry.mime_len))
                .ok_or_else(|| CliError::PackValidation("custom MIME range overflow".to_owned()))?;
            let m_bytes = string_pool.get(m_start..m_end).ok_or_else(|| {
                CliError::PackValidation(format!("entry {path:?} custom MIME out of pool bounds"))
            })?;
            std::str::from_utf8(m_bytes)
                .map_err(|_| {
                    CliError::PackValidation(format!("entry {path:?} custom MIME is not UTF-8"))
                })?
                .to_owned()
        } else {
            mime_from_id(entry.mime_id)
                .expect("PackEntry::decode already validated mime_id")
                .to_owned()
        };

        let d_start = entry.data_off as usize;
        let d_end = d_start
            .checked_add(entry.data_len as usize)
            .ok_or_else(|| CliError::PackValidation("entry payload range overflow".to_owned()))?;
        let payload = data_section.get(d_start..d_end).ok_or_else(|| {
            CliError::PackValidation(format!("entry {path:?} payload out of data section bounds"))
        })?;

        if path == DOCUMENT_PATH {
            doc_slice = Some(payload);
        }

        entries.push(PackEntrySummary {
            path: path.clone(),
            path_hash: entry.path_hash,
            mime,
            mime_id: entry.mime_id,
            data_off: entry.data_off,
            data_len: entry.data_len,
        });
        prev_path = Some(path);
    }

    Ok((
        PackTableSummary {
            entry_count: header.entry_count,
            strings_offset: header.strings_offset,
            strings_len: header.strings_len,
            data_offset: header.data_offset,
            total_bytes: bytes.len(),
            crc32: crc32fast::hash(bytes),
            entries,
        },
        doc_slice,
    ))
}

/// Validates a `document.bin` slice completely using `squark-journal::BatchDecoder`
/// for every journal batch in the directory.
pub fn validate_document_bin(bytes: &[u8]) -> Result<ValidatedDocument, CliError> {
    if bytes.len() < DOCUMENT_HEADER_SIZE {
        return Err(CliError::PackValidation(format!(
            "document.bin shorter than 64-byte header ({} bytes)",
            bytes.len()
        )));
    }
    let header_arr: &[u8; DOCUMENT_HEADER_SIZE] = bytes[..DOCUMENT_HEADER_SIZE]
        .try_into()
        .expect("header slice length checked");
    let header = DocumentHeader::decode(header_arr)?;

    let batch_dir_start = usize::try_from(header.batch_dir_offset)
        .map_err(|_| CliError::PackValidation("batch_dir_offset exceeds usize".to_owned()))?;
    let batch_dir_bytes = (header.batch_count as usize)
        .checked_mul(BATCH_DIR_ENTRY_SIZE)
        .ok_or_else(|| CliError::PackValidation("batch directory length overflow".to_owned()))?;
    let batch_dir_end = batch_dir_start
        .checked_add(batch_dir_bytes)
        .ok_or_else(|| CliError::PackValidation("batch directory end overflow".to_owned()))?;
    if batch_dir_start < DOCUMENT_HEADER_SIZE || batch_dir_end > bytes.len() {
        return Err(CliError::PackValidation(
            "batch directory out of document.bin bounds".to_owned(),
        ));
    }

    let mut decoder =
        BatchDecoder::new(header.document_node_id).map_err(|source| CliError::JournalBatch {
            batch_index: 0,
            source,
        })?;
    let mut batches = Vec::with_capacity(header.batch_count as usize);
    let mut atoms: HashMap<u32, (AtomKind, Vec<u16>)> = HashMap::new();

    for i in 0..(header.batch_count as usize) {
        let entry_off = batch_dir_start + i * BATCH_DIR_ENTRY_SIZE;
        let entry_arr: &[u8; BATCH_DIR_ENTRY_SIZE] = bytes
            [entry_off..entry_off + BATCH_DIR_ENTRY_SIZE]
            .try_into()
            .expect("batch dir entry length checked");
        let dir = BatchDirEntry::decode(entry_arr);
        let b_start = usize::try_from(dir.offset)
            .map_err(|_| CliError::PackValidation("batch offset exceeds usize".to_owned()))?;
        let b_len = usize::try_from(dir.len)
            .map_err(|_| CliError::PackValidation("batch length exceeds usize".to_owned()))?;
        let b_end = b_start
            .checked_add(b_len)
            .ok_or_else(|| CliError::PackValidation("batch end overflow".to_owned()))?;
        let slice = bytes.get(b_start..b_end).ok_or_else(|| {
            CliError::PackValidation(format!("batch {i} range {b_start}..{b_end} out of bounds"))
        })?;

        let decoded = decoder
            .validate(slice)
            .map_err(|source| CliError::JournalBatch {
                batch_index: i,
                source,
            })?;
        let records: Vec<Record> = decoded.records().collect();
        let table = decoded.strings();
        let strings: Vec<Vec<u16>> = (0..table.len() as u32)
            .map(|idx| table.get(idx).expect("index in bounds").units().collect())
            .collect();
        for rec in &records {
            if rec.op == Opcode::DefineAtom
                && let Some(atom) = decoded.atom(rec.a)
            {
                atoms.insert(rec.a, (atom.kind, atom.text.to_vec()));
            }
        }
        decoded.commit();
        batches.push(ValidatedBatch { records, strings });
    }

    let ss_start = usize::try_from(header.script_strings_offset)
        .map_err(|_| CliError::PackValidation("script_strings_offset exceeds usize".to_owned()))?;
    let ss_len = usize::try_from(header.script_strings_len)
        .map_err(|_| CliError::PackValidation("script_strings_len exceeds usize".to_owned()))?;
    let ss_end = ss_start
        .checked_add(ss_len)
        .ok_or_else(|| CliError::PackValidation("script string table end overflow".to_owned()))?;
    let ss_slice = bytes.get(ss_start..ss_end).ok_or_else(|| {
        CliError::PackValidation("script string table out of document.bin bounds".to_owned())
    })?;
    let script_strings = decode_utf16_string_table(ss_slice)?;

    let st_start = usize::try_from(header.script_table_offset)
        .map_err(|_| CliError::PackValidation("script_table_offset exceeds usize".to_owned()))?;
    let st_len = (header.script_count as usize)
        .checked_mul(SCRIPT_ENTRY_SIZE)
        .ok_or_else(|| CliError::PackValidation("script table length overflow".to_owned()))?;
    let st_end = st_start
        .checked_add(st_len)
        .ok_or_else(|| CliError::PackValidation("script table end overflow".to_owned()))?;
    if st_end > bytes.len() {
        return Err(CliError::PackValidation(
            "script table out of document.bin bounds".to_owned(),
        ));
    }

    let mut scripts = Vec::with_capacity(header.script_count as usize);
    for i in 0..(header.script_count as usize) {
        let off = st_start + i * SCRIPT_ENTRY_SIZE;
        let arr: &[u8; SCRIPT_ENTRY_SIZE] = bytes[off..off + SCRIPT_ENTRY_SIZE]
            .try_into()
            .expect("script entry length checked");
        let entry = ScriptEntry::decode(arr)?;
        if (entry.payload_str as usize) >= script_strings.len()
            || (entry.base_url_str as usize) >= script_strings.len()
        {
            return Err(CliError::PackValidation(format!(
                "script entry {i} string index out of bounds"
            )));
        }
        scripts.push(entry);
    }

    Ok(ValidatedDocument {
        header,
        batches,
        script_strings,
        scripts,
        atoms,
    })
}

fn summarize_document(doc: &ValidatedDocument) -> DocumentSummary {
    let total_records = doc.batches.iter().map(|b| b.records.len()).sum();
    let mut inline_styles = Vec::new();
    for batch in &doc.batches {
        for rec in &batch.records {
            if rec.op == Opcode::SheetInsert {
                inline_styles.push(InlineStyleSummary {
                    node_id: rec.a,
                    css: batch.string_lossy(rec.c).unwrap_or_default(),
                });
            }
        }
    }
    let scripts = doc
        .scripts
        .iter()
        .map(|s| ScriptSummary {
            kind: script_kind_name(s.kind),
            defer: s.flags.contains(ScriptFlags::DEFER),
            r#async: s.flags.contains(ScriptFlags::ASYNC),
            node_id: s.node_id.raw(),
            payload: String::from_utf16_lossy(&doc.script_strings[s.payload_str as usize]),
            base_url: String::from_utf16_lossy(&doc.script_strings[s.base_url_str as usize]),
        })
        .collect();
    DocumentSummary {
        quirks_mode: quirks_mode_name(doc.header.quirks_mode),
        document_node_id: doc.header.document_node_id.raw(),
        batch_count: doc.header.batch_count,
        total_records,
        atom_count: doc.atoms.len(),
        inline_styles,
        scripts,
    }
}

/// Formats a [`PackInspection`] as a human-readable dump for `squark inspect --dump-pack`.
pub fn format_inspection(info: &PackInspection) -> String {
    let mut out = String::new();
    let kind_label = match info.container {
        ContainerKind::StandaloneSqpak => "standalone .sqpak",
        ContainerKind::AppendedBinary => "appended runtime binary",
        ContainerKind::StandaloneDocumentBin => "standalone document.bin",
    };
    let _ = writeln!(out, "Container: {kind_label} ({} bytes)", info.file_bytes);

    if let Some(t) = &info.trailer {
        let _ = writeln!(
            out,
            "Trailer:   offset={} len={} crc32={:#010x} (valid={})",
            t.pack_offset, t.pack_len, t.pack_crc32, t.crc_valid
        );
    }

    if let Some(p) = &info.pack {
        let _ = writeln!(
            out,
            "Pack:      entries={} strings={}..{} data_offset={} total={} bytes crc32={:#010x}",
            p.entry_count,
            p.strings_offset,
            p.strings_offset + p.strings_len,
            p.data_offset,
            p.total_bytes,
            p.crc32
        );
        let _ = writeln!(out, "Entries:");
        for e in &p.entries {
            let _ = writeln!(
                out,
                "  {:<40} {:>8} B  @{:<8}  mime={} (id={})",
                e.path, e.data_len, e.data_off, e.mime, e.mime_id
            );
        }
    }

    if let Some(d) = &info.document {
        let _ = writeln!(
            out,
            "Document:  quirks={} doc_node={} (slot {}, gen {}) batches={} records={} atoms={}",
            d.quirks_mode,
            d.document_node_id,
            NodeId::from_raw(d.document_node_id).slot(),
            NodeId::from_raw(d.document_node_id).generation(),
            d.batch_count,
            d.total_records,
            d.atom_count
        );
        if !d.inline_styles.is_empty() {
            let _ = writeln!(out, "Inline Styles ({}):", d.inline_styles.len());
            for (i, s) in d.inline_styles.iter().enumerate() {
                let preview = if s.css.len() > 60 {
                    format!("{}...", &s.css[..57])
                } else {
                    s.css.replace('\n', "\\n")
                };
                let _ = writeln!(out, "  [{i}] node={:<4} css={:?}", s.node_id, preview);
            }
        }
        if !d.scripts.is_empty() {
            let _ = writeln!(out, "Scripts & Stylesheets ({}):", d.scripts.len());
            for (i, s) in d.scripts.iter().enumerate() {
                let preview = if s.payload.len() > 60 {
                    format!("{}...", &s.payload[..57])
                } else {
                    s.payload.replace('\n', "\\n")
                };
                let _ = writeln!(
                    out,
                    "  [{i}] {:<19} node={:<4} defer={:<5} async={:<5} base={:?} payload={:?}",
                    s.kind, s.node_id, s.defer, s.r#async, s.base_url, preview
                );
            }
        }
    }

    out
}

fn quirks_mode_name(mode: QuirksMode) -> &'static str {
    match mode {
        QuirksMode::Standards => "standards",
        QuirksMode::LimitedQuirks => "limited-quirks",
        QuirksMode::Quirks => "quirks",
    }
}

fn script_kind_name(kind: ScriptKind) -> &'static str {
    match kind {
        ScriptKind::InlineClassic => "inline-classic",
        ScriptKind::ExternalClassic => "external-classic",
        ScriptKind::InlineModule => "inline-module",
        ScriptKind::ExternalModule => "external-module",
        ScriptKind::ExternalStylesheet => "external-stylesheet",
    }
}
