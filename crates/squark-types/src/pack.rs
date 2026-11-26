// SPDX-License-Identifier: MIT
//! Binary formats for `.sqpak` asset archives, appended runtime trailers, and
//! `document.bin` initial documents.
//!
//! All multi-byte integers are little-endian. All pack paths and custom MIME
//! strings are UTF-8. The `document.bin` script string table uses the same
//! UTF-16LE offset-table encoding as journal batches so lone surrogates round
//! trip identically across `squark-cli`, `squark-assets`, and `squark-js`.

use std::fmt;

use crate::NodeId;

/// Magic bytes for the 32-byte EOF trailer appended to a packed runtime binary.
pub const TRAILER_MAGIC: [u8; 8] = *b"SQPACK\0\0";
/// Byte size of [`PackTrailer`].
pub const TRAILER_SIZE: usize = 32;
/// Version 1 of the binary trailer format.
pub const TRAILER_VERSION: u32 = 1;
/// Required page alignment of `pack_offset` when a `.sqpak` is appended to a runtime binary.
pub const TRAILER_ALIGN: u64 = 4096;

/// File name of the pack in a macOS app bundle's `Contents/Resources`. A
/// squark-runtime executable in `Contents/MacOS` with no appended pack runs it.
pub const BUNDLE_PACK_NAME: &str = "app.sqpak";

/// Magic bytes for a version 1 `.sqpak` archive header.
pub const SQPK_MAGIC: [u8; 8] = *b"SQPK\x00\x01\x00\x00";
/// Byte size of [`PackHeader`].
pub const PACK_HEADER_SIZE: usize = 32;
/// Byte size of [`PackEntry`].
pub const PACK_ENTRY_SIZE: usize = 32;
/// Required alignment of the `.sqpak` data section offset.
pub const PACK_DATA_ALIGN: usize = 64;
/// Required alignment of each entry's payload offset relative to `data_offset`.
pub const PACK_ENTRY_DATA_ALIGN: usize = 8;
/// Sentinel `mime_id` indicating a custom UTF-8 MIME string in the string pool.
pub const MIME_CUSTOM: u16 = 0xFFFF;

/// Magic bytes for a `document.bin` header.
pub const SQDOC_MAGIC: [u8; 8] = *b"SQDOC\0\0\0";
/// Version 1 of the `document.bin` format.
pub const DOCUMENT_VERSION: u16 = 1;
/// Byte size of [`DocumentHeader`].
pub const DOCUMENT_HEADER_SIZE: usize = 64;
/// Byte size of [`BatchDirEntry`].
pub const BATCH_DIR_ENTRY_SIZE: usize = 16;
/// Byte size of [`ScriptEntry`].
pub const SCRIPT_ENTRY_SIZE: usize = 16;

/// Reserved canonical pack path for the compiled initial document.
pub const DOCUMENT_PATH: &str = "/__squark/document.bin";
/// Reserved canonical pack path for pack build metadata.
pub const MANIFEST_PATH: &str = "/__squark/manifest.json";

/// Built-in MIME table indexed by `PackEntry::mime_id` (`0..=14`).
pub const BUILTIN_MIMES: [&str; 15] = [
    "application/octet-stream",
    "text/html; charset=utf-8",
    "text/javascript; charset=utf-8",
    "text/css; charset=utf-8",
    "application/json",
    "font/woff2",
    "font/ttf",
    "font/otf",
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/svg+xml",
    "image/x-icon",
    "application/wasm",
    "text/plain; charset=utf-8",
];

/// Looks up a built-in MIME string by its `mime_id`.
pub fn mime_from_id(id: u16) -> Option<&'static str> {
    BUILTIN_MIMES.get(usize::from(id)).copied()
}

/// Returns the built-in `mime_id` for an exact MIME string, if present.
pub fn mime_id_from_str(mime: &str) -> Option<u16> {
    BUILTIN_MIMES
        .iter()
        .position(|&candidate| candidate == mime)
        .map(|idx| idx as u16)
}

/// Infers a built-in `mime_id` from a file extension (without leading dot, ASCII case-insensitive).
///
/// Unknown extensions map to `0` (`application/octet-stream`).
pub fn mime_id_for_extension(ext: &str) -> u16 {
    if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
        1
    } else if ext.eq_ignore_ascii_case("js")
        || ext.eq_ignore_ascii_case("mjs")
        || ext.eq_ignore_ascii_case("cjs")
    {
        2
    } else if ext.eq_ignore_ascii_case("css") {
        3
    } else if ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("webmanifest") {
        4
    } else if ext.eq_ignore_ascii_case("woff2") {
        5
    } else if ext.eq_ignore_ascii_case("ttf") {
        6
    } else if ext.eq_ignore_ascii_case("otf") {
        7
    } else if ext.eq_ignore_ascii_case("png") {
        8
    } else if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        9
    } else if ext.eq_ignore_ascii_case("webp") {
        10
    } else if ext.eq_ignore_ascii_case("svg") {
        11
    } else if ext.eq_ignore_ascii_case("ico") {
        12
    } else if ext.eq_ignore_ascii_case("wasm") {
        13
    } else if ext.eq_ignore_ascii_case("txt") {
        14
    } else {
        0
    }
}

/// Computes the 64-bit FNV-1a hash used for `PackEntry::path_hash`.
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(PRIME);
        i += 1;
    }
    hash
}

/// Validates a canonical `.sqpak` path.
///
/// Paths must be UTF-8, start with a single `'/'`, contain at least one
/// non-empty path segment, and contain no empty segments, `"."`, `".."`,
/// `'\\'` or NUL bytes.
pub fn is_valid_pack_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty() || path.contains('\\') || path.contains('\0') {
        return false;
    }
    rest.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Why a pack or `document.bin` structure failed validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackFormatError {
    /// Magic bytes did not match the expected header or trailer.
    BadMagic,
    /// Format version is not supported by version 1 readers.
    UnsupportedVersion { version: u32 },
    /// Header length field did not match the expected fixed size.
    BadHeaderLength { len: u16 },
    /// Reserved or padding bytes were non-zero.
    NonZeroReserved,
    /// Alignment constraint on an offset was violated.
    UnalignedOffset { offset: u64, align: u64 },
    /// `index_offset` did not equal [`PACK_HEADER_SIZE`].
    BadIndexOffset { offset: u32 },
    /// `QuirksMode` bits in [`DocumentHeader::flags`] were invalid.
    InvalidQuirksMode { bits: u32 },
    /// A node id had slot 0 (invalid).
    InvalidNodeId { raw: u32 },
    /// `mime_id` was neither a built-in MIME nor [`MIME_CUSTOM`].
    InvalidMimeId { id: u16 },
    /// Custom MIME offset/length was non-zero for a built-in MIME id.
    UnexpectedCustomMime,
    /// `ScriptEntry::kind` was outside `0..=4`.
    InvalidScriptKind { kind: u8 },
    /// `ScriptEntry::flags` had reserved bits set or flags on a stylesheet.
    InvalidScriptFlags { kind: u8, flags: u8 },
    /// Input slice was too short or had trailing bytes.
    InvalidLength { expected: usize, actual: usize },
    /// UTF-16 string table offsets were out of bounds or not monotonic.
    InvalidUtf16StringTable,
}

impl fmt::Display for PackFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BadMagic => write!(f, "bad pack magic"),
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported pack format version {version}")
            }
            Self::BadHeaderLength { len } => write!(f, "unexpected header length {len}"),
            Self::NonZeroReserved => write!(f, "non-zero reserved or flag bits"),
            Self::UnalignedOffset { offset, align } => {
                write!(f, "offset {offset} is not aligned to {align} bytes")
            }
            Self::BadIndexOffset { offset } => {
                write!(
                    f,
                    "pack index_offset {offset} does not equal {PACK_HEADER_SIZE}"
                )
            }
            Self::InvalidQuirksMode { bits } => write!(f, "invalid quirks mode bits {bits:#x}"),
            Self::InvalidNodeId { raw } => write!(f, "invalid node id {raw:#x} (slot 0)"),
            Self::InvalidMimeId { id } => write!(f, "invalid built-in mime_id {id}"),
            Self::UnexpectedCustomMime => {
                write!(f, "non-zero custom MIME fields for built-in mime_id")
            }
            Self::InvalidScriptKind { kind } => write!(f, "invalid script entry kind {kind}"),
            Self::InvalidScriptFlags { kind, flags } => {
                write!(f, "invalid script flags {flags:#x} for kind {kind}")
            }
            Self::InvalidLength { expected, actual } => {
                write!(f, "invalid length: expected {expected} bytes, got {actual}")
            }
            Self::InvalidUtf16StringTable => write!(f, "malformed UTF-16 string table"),
        }
    }
}

impl std::error::Error for PackFormatError {}

/// 32-byte trailer at `EOF - 32..EOF` of a packed runtime executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackTrailer {
    /// Absolute byte offset of the `.sqpak` header from the start of the file (4096-byte aligned).
    pub pack_offset: u64,
    /// Byte length of the `.sqpak` payload.
    pub pack_len: u32,
    /// IEEE CRC32 of `file[pack_offset..pack_offset + pack_len]`.
    pub pack_crc32: u32,
}

impl PackTrailer {
    pub fn encode(&self) -> [u8; TRAILER_SIZE] {
        let mut out = [0u8; TRAILER_SIZE];
        out[0..8].copy_from_slice(&TRAILER_MAGIC);
        out[8..12].copy_from_slice(&TRAILER_VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&0u32.to_le_bytes());
        out[16..24].copy_from_slice(&self.pack_offset.to_le_bytes());
        out[24..28].copy_from_slice(&self.pack_len.to_le_bytes());
        out[28..32].copy_from_slice(&self.pack_crc32.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; TRAILER_SIZE]) -> Result<Self, PackFormatError> {
        if bytes[0..8] != TRAILER_MAGIC {
            return Err(PackFormatError::BadMagic);
        }
        let version = read_u32(bytes, 8);
        if version != TRAILER_VERSION {
            return Err(PackFormatError::UnsupportedVersion { version });
        }
        let flags = read_u32(bytes, 12);
        if flags != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        let pack_offset = read_u64(bytes, 16);
        if !pack_offset.is_multiple_of(TRAILER_ALIGN) {
            return Err(PackFormatError::UnalignedOffset {
                offset: pack_offset,
                align: TRAILER_ALIGN,
            });
        }
        Ok(Self {
            pack_offset,
            pack_len: read_u32(bytes, 24),
            pack_crc32: read_u32(bytes, 28),
        })
    }
}

/// 32-byte header at offset 0 of a `.sqpak` archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackHeader {
    pub entry_count: u32,
    pub index_offset: u32,
    pub strings_offset: u32,
    pub strings_len: u32,
    pub data_offset: u32,
}

impl PackHeader {
    pub fn encode(&self) -> [u8; PACK_HEADER_SIZE] {
        let mut out = [0u8; PACK_HEADER_SIZE];
        out[0..8].copy_from_slice(&SQPK_MAGIC);
        out[8..12].copy_from_slice(&self.entry_count.to_le_bytes());
        out[12..16].copy_from_slice(&self.index_offset.to_le_bytes());
        out[16..20].copy_from_slice(&self.strings_offset.to_le_bytes());
        out[20..24].copy_from_slice(&self.strings_len.to_le_bytes());
        out[24..28].copy_from_slice(&self.data_offset.to_le_bytes());
        out[28..32].copy_from_slice(&0u32.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; PACK_HEADER_SIZE]) -> Result<Self, PackFormatError> {
        if bytes[0..8] != SQPK_MAGIC {
            return Err(PackFormatError::BadMagic);
        }
        let index_offset = read_u32(bytes, 12);
        if index_offset != PACK_HEADER_SIZE as u32 {
            return Err(PackFormatError::BadIndexOffset {
                offset: index_offset,
            });
        }
        let data_offset = read_u32(bytes, 24);
        if !(data_offset as usize).is_multiple_of(PACK_DATA_ALIGN) {
            return Err(PackFormatError::UnalignedOffset {
                offset: u64::from(data_offset),
                align: PACK_DATA_ALIGN as u64,
            });
        }
        let flags = read_u32(bytes, 28);
        if flags != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        Ok(Self {
            entry_count: read_u32(bytes, 8),
            index_offset,
            strings_offset: read_u32(bytes, 16),
            strings_len: read_u32(bytes, 20),
            data_offset,
        })
    }
}

/// 32-byte index entry in a `.sqpak` archive, sorted lexicographically by path bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackEntry {
    pub path_hash: u64,
    pub path_off: u32,
    pub path_len: u16,
    pub mime_id: u16,
    pub data_off: u32,
    pub data_len: u32,
    pub mime_off: u32,
    pub mime_len: u16,
    pub entry_flags: u16,
}

impl PackEntry {
    pub fn encode(&self) -> [u8; PACK_ENTRY_SIZE] {
        let mut out = [0u8; PACK_ENTRY_SIZE];
        out[0..8].copy_from_slice(&self.path_hash.to_le_bytes());
        out[8..12].copy_from_slice(&self.path_off.to_le_bytes());
        out[12..14].copy_from_slice(&self.path_len.to_le_bytes());
        out[14..16].copy_from_slice(&self.mime_id.to_le_bytes());
        out[16..20].copy_from_slice(&self.data_off.to_le_bytes());
        out[20..24].copy_from_slice(&self.data_len.to_le_bytes());
        out[24..28].copy_from_slice(&self.mime_off.to_le_bytes());
        out[28..30].copy_from_slice(&self.mime_len.to_le_bytes());
        out[30..32].copy_from_slice(&self.entry_flags.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; PACK_ENTRY_SIZE]) -> Result<Self, PackFormatError> {
        let mime_id = read_u16(bytes, 14);
        let is_builtin = usize::from(mime_id) < BUILTIN_MIMES.len();
        if !is_builtin && mime_id != MIME_CUSTOM {
            return Err(PackFormatError::InvalidMimeId { id: mime_id });
        }
        let data_off = read_u32(bytes, 16);
        if !(data_off as usize).is_multiple_of(PACK_ENTRY_DATA_ALIGN) {
            return Err(PackFormatError::UnalignedOffset {
                offset: u64::from(data_off),
                align: PACK_ENTRY_DATA_ALIGN as u64,
            });
        }
        let mime_off = read_u32(bytes, 24);
        let mime_len = read_u16(bytes, 28);
        if is_builtin && (mime_off != 0 || mime_len != 0) {
            return Err(PackFormatError::UnexpectedCustomMime);
        }
        let entry_flags = read_u16(bytes, 30);
        if entry_flags != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        Ok(Self {
            path_hash: read_u64(bytes, 0),
            path_off: read_u32(bytes, 8),
            path_len: read_u16(bytes, 12),
            mime_id,
            data_off,
            data_len: read_u32(bytes, 20),
            mime_off,
            mime_len,
            entry_flags,
        })
    }
}

/// HTML document quirks mode stored in the low two bits of [`DocumentHeader::flags`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum QuirksMode {
    Standards = 0,
    LimitedQuirks = 1,
    Quirks = 2,
}

impl QuirksMode {
    pub fn from_flags(flags: u32) -> Result<Self, PackFormatError> {
        if flags & !0b11 != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        match flags & 0b11 {
            0 => Ok(Self::Standards),
            1 => Ok(Self::LimitedQuirks),
            2 => Ok(Self::Quirks),
            bits => Err(PackFormatError::InvalidQuirksMode { bits }),
        }
    }

    pub const fn to_flags(self) -> u32 {
        self as u32
    }
}

/// 64-byte header at offset 0 of `/__squark/document.bin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentHeader {
    pub quirks_mode: QuirksMode,
    pub document_node_id: NodeId,
    pub batch_count: u32,
    pub batch_dir_offset: u64,
    pub script_strings_offset: u64,
    pub script_strings_len: u64,
    pub script_count: u32,
    pub script_table_offset: u64,
}

impl DocumentHeader {
    pub fn encode(&self) -> [u8; DOCUMENT_HEADER_SIZE] {
        let mut out = [0u8; DOCUMENT_HEADER_SIZE];
        out[0..8].copy_from_slice(&SQDOC_MAGIC);
        out[8..10].copy_from_slice(&DOCUMENT_VERSION.to_le_bytes());
        out[10..12].copy_from_slice(&(DOCUMENT_HEADER_SIZE as u16).to_le_bytes());
        out[12..16].copy_from_slice(&self.quirks_mode.to_flags().to_le_bytes());
        out[16..20].copy_from_slice(&self.document_node_id.raw().to_le_bytes());
        out[20..24].copy_from_slice(&self.batch_count.to_le_bytes());
        out[24..32].copy_from_slice(&self.batch_dir_offset.to_le_bytes());
        out[32..40].copy_from_slice(&self.script_strings_offset.to_le_bytes());
        out[40..48].copy_from_slice(&self.script_strings_len.to_le_bytes());
        out[48..52].copy_from_slice(&self.script_count.to_le_bytes());
        out[52..56].copy_from_slice(&0u32.to_le_bytes());
        out[56..64].copy_from_slice(&self.script_table_offset.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; DOCUMENT_HEADER_SIZE]) -> Result<Self, PackFormatError> {
        if bytes[0..8] != SQDOC_MAGIC {
            return Err(PackFormatError::BadMagic);
        }
        let version = read_u16(bytes, 8);
        if version != DOCUMENT_VERSION {
            return Err(PackFormatError::UnsupportedVersion {
                version: u32::from(version),
            });
        }
        let header_len = read_u16(bytes, 10);
        if header_len != DOCUMENT_HEADER_SIZE as u16 {
            return Err(PackFormatError::BadHeaderLength { len: header_len });
        }
        let quirks_mode = QuirksMode::from_flags(read_u32(bytes, 12))?;
        let raw_doc_id = read_u32(bytes, 16);
        let document_node_id = NodeId::from_raw(raw_doc_id);
        if document_node_id.slot() == 0 {
            return Err(PackFormatError::InvalidNodeId { raw: raw_doc_id });
        }
        let reserved = read_u32(bytes, 52);
        if reserved != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        Ok(Self {
            quirks_mode,
            document_node_id,
            batch_count: read_u32(bytes, 20),
            batch_dir_offset: read_u64(bytes, 24),
            script_strings_offset: read_u64(bytes, 32),
            script_strings_len: read_u64(bytes, 40),
            script_count: read_u32(bytes, 48),
            script_table_offset: read_u64(bytes, 56),
        })
    }
}

/// 16-byte directory entry pointing to one complete version 1 journal batch in `document.bin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchDirEntry {
    pub offset: u64,
    pub len: u64,
}

impl BatchDirEntry {
    pub fn encode(&self) -> [u8; BATCH_DIR_ENTRY_SIZE] {
        let mut out = [0u8; BATCH_DIR_ENTRY_SIZE];
        out[0..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..16].copy_from_slice(&self.len.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; BATCH_DIR_ENTRY_SIZE]) -> Self {
        Self {
            offset: read_u64(bytes, 0),
            len: read_u64(bytes, 8),
        }
    }
}

/// Kind of executable `<script>` or external `<link rel="stylesheet">` entry in `document.bin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ScriptKind {
    InlineClassic = 0,
    ExternalClassic = 1,
    InlineModule = 2,
    ExternalModule = 3,
    ExternalStylesheet = 4,
}

impl ScriptKind {
    pub fn from_u8(kind: u8) -> Result<Self, PackFormatError> {
        match kind {
            0 => Ok(Self::InlineClassic),
            1 => Ok(Self::ExternalClassic),
            2 => Ok(Self::InlineModule),
            3 => Ok(Self::ExternalModule),
            4 => Ok(Self::ExternalStylesheet),
            _ => Err(PackFormatError::InvalidScriptKind { kind }),
        }
    }
}

/// Attribute flags on a [`ScriptEntry`] (`defer` and `async` for scripts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScriptFlags(u8);

impl ScriptFlags {
    pub const NONE: Self = Self(0);
    pub const DEFER: Self = Self(1 << 0);
    pub const ASYNC: Self = Self(1 << 1);
    const VALID_MASK: u8 = Self::DEFER.0 | Self::ASYNC.0;

    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !Self::VALID_MASK != 0 {
            None
        } else {
            Some(Self(bits))
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// 16-byte script or external stylesheet entry in `document.bin`, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptEntry {
    pub kind: ScriptKind,
    pub flags: ScriptFlags,
    pub node_id: NodeId,
    pub payload_str: u32,
    pub base_url_str: u32,
}

impl ScriptEntry {
    pub fn encode(&self) -> [u8; SCRIPT_ENTRY_SIZE] {
        let mut out = [0u8; SCRIPT_ENTRY_SIZE];
        out[0] = self.kind as u8;
        out[1] = self.flags.bits();
        out[2..4].copy_from_slice(&0u16.to_le_bytes());
        out[4..8].copy_from_slice(&self.node_id.raw().to_le_bytes());
        out[8..12].copy_from_slice(&self.payload_str.to_le_bytes());
        out[12..16].copy_from_slice(&self.base_url_str.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; SCRIPT_ENTRY_SIZE]) -> Result<Self, PackFormatError> {
        let kind = ScriptKind::from_u8(bytes[0])?;
        let raw_flags = bytes[1];
        let flags =
            ScriptFlags::from_bits(raw_flags).ok_or(PackFormatError::InvalidScriptFlags {
                kind: bytes[0],
                flags: raw_flags,
            })?;
        if kind == ScriptKind::ExternalStylesheet && flags != ScriptFlags::NONE {
            return Err(PackFormatError::InvalidScriptFlags {
                kind: bytes[0],
                flags: raw_flags,
            });
        }
        if bytes[2] != 0 || bytes[3] != 0 {
            return Err(PackFormatError::NonZeroReserved);
        }
        let raw_node_id = read_u32(bytes, 4);
        let node_id = NodeId::from_raw(raw_node_id);
        if node_id.slot() == 0 {
            return Err(PackFormatError::InvalidNodeId { raw: raw_node_id });
        }
        Ok(Self {
            kind,
            flags,
            node_id,
            payload_str: read_u32(bytes, 8),
            base_url_str: read_u32(bytes, 12),
        })
    }
}

/// Encodes a slice of UTF-16 strings into the `document.bin` script string table format:
/// `u32 count`, `u32 total_code_units`, `(count + 1) * u32` offsets, then UTF-16LE units.
pub fn encode_utf16_string_table<S: AsRef<[u16]>>(
    strings: &[S],
) -> Result<Vec<u8>, PackFormatError> {
    let count =
        u32::try_from(strings.len()).map_err(|_| PackFormatError::InvalidUtf16StringTable)?;
    let mut total_units: u32 = 0;
    for s in strings {
        let len = u32::try_from(s.as_ref().len())
            .map_err(|_| PackFormatError::InvalidUtf16StringTable)?;
        total_units = total_units
            .checked_add(len)
            .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    }

    let offsets_bytes = (strings.len() + 1)
        .checked_mul(4)
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    let units_bytes = (total_units as usize)
        .checked_mul(2)
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    let total_bytes = 8usize
        .checked_add(offsets_bytes)
        .and_then(|n| n.checked_add(units_bytes))
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;

    let mut out = Vec::with_capacity(total_bytes);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&total_units.to_le_bytes());

    let mut cursor: u32 = 0;
    out.extend_from_slice(&cursor.to_le_bytes());
    for s in strings {
        cursor += s.as_ref().len() as u32;
        out.extend_from_slice(&cursor.to_le_bytes());
    }
    for s in strings {
        for &unit in s.as_ref() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    Ok(out)
}

/// Decodes and validates a `document.bin` UTF-16 script string table.
pub fn decode_utf16_string_table(bytes: &[u8]) -> Result<Vec<Vec<u16>>, PackFormatError> {
    if bytes.len() < 8 {
        return Err(PackFormatError::InvalidUtf16StringTable);
    }
    let count = read_u32(bytes, 0) as usize;
    let total_units = read_u32(bytes, 4) as usize;

    let offsets_len = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(4))
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    let units_offset = 8usize
        .checked_add(offsets_len)
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    let units_bytes = total_units
        .checked_mul(2)
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    let expected_len = units_offset
        .checked_add(units_bytes)
        .ok_or(PackFormatError::InvalidUtf16StringTable)?;
    if bytes.len() != expected_len {
        return Err(PackFormatError::InvalidLength {
            expected: expected_len,
            actual: bytes.len(),
        });
    }

    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        offsets.push(read_u32(bytes, 8 + i * 4) as usize);
    }
    if offsets[0] != 0 || offsets[count] != total_units {
        return Err(PackFormatError::InvalidUtf16StringTable);
    }
    for pair in offsets.windows(2) {
        if pair[0] > pair[1] {
            return Err(PackFormatError::InvalidUtf16StringTable);
        }
    }

    let payload = &bytes[units_offset..];
    let mut strings = Vec::with_capacity(count);
    for pair in offsets.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let mut units = Vec::with_capacity(end - start);
        for idx in start..end {
            units.push(read_u16(payload, idx * 2));
        }
        strings.push(units);
    }
    Ok(strings)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailer_round_trips_and_validates() {
        let trailer = PackTrailer {
            pack_offset: 8192,
            pack_len: 12_345,
            pack_crc32: 0x1234_5678,
        };
        let encoded = trailer.encode();
        assert_eq!(encoded.len(), TRAILER_SIZE);
        assert_eq!(&encoded[0..8], b"SQPACK\0\0");
        assert_eq!(PackTrailer::decode(&encoded), Ok(trailer));

        let mut bad_align = encoded;
        bad_align[16..24].copy_from_slice(&4097u64.to_le_bytes());
        assert_eq!(
            PackTrailer::decode(&bad_align),
            Err(PackFormatError::UnalignedOffset {
                offset: 4097,
                align: 4096
            })
        );

        let mut bad_ver = encoded;
        bad_ver[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            PackTrailer::decode(&bad_ver),
            Err(PackFormatError::UnsupportedVersion { version: 2 })
        );
    }

    #[test]
    fn pack_header_and_entry_round_trip() {
        let header = PackHeader {
            entry_count: 3,
            index_offset: 32,
            strings_offset: 128,
            strings_len: 42,
            data_offset: 192,
        };
        let encoded = header.encode();
        assert_eq!(&encoded[0..8], b"SQPK\x00\x01\x00\x00");
        assert_eq!(PackHeader::decode(&encoded), Ok(header));

        let mut unaligned_data = encoded;
        unaligned_data[24..28].copy_from_slice(&130u32.to_le_bytes());
        assert_eq!(
            PackHeader::decode(&unaligned_data),
            Err(PackFormatError::UnalignedOffset {
                offset: 130,
                align: 64
            })
        );

        let entry = PackEntry {
            path_hash: fnv1a_64(b"/index.html"),
            path_off: 0,
            path_len: 11,
            mime_id: 1,
            data_off: 64,
            data_len: 512,
            mime_off: 0,
            mime_len: 0,
            entry_flags: 0,
        };
        let encoded_entry = entry.encode();
        assert_eq!(PackEntry::decode(&encoded_entry), Ok(entry));

        let mut bad_custom = encoded_entry;
        bad_custom[24..28].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(
            PackEntry::decode(&bad_custom),
            Err(PackFormatError::UnexpectedCustomMime)
        );

        let custom_entry = PackEntry {
            mime_id: MIME_CUSTOM,
            mime_off: 12,
            mime_len: 16,
            ..entry
        };
        assert_eq!(PackEntry::decode(&custom_entry.encode()), Ok(custom_entry));
    }

    #[test]
    fn document_header_batch_dir_and_script_entry_round_trip() {
        let header = DocumentHeader {
            quirks_mode: QuirksMode::LimitedQuirks,
            document_node_id: NodeId::new(1, 0).unwrap(),
            batch_count: 2,
            batch_dir_offset: 64,
            script_strings_offset: 512,
            script_strings_len: 128,
            script_count: 3,
            script_table_offset: 640,
        };
        let encoded = header.encode();
        assert_eq!(&encoded[0..8], b"SQDOC\0\0\0");
        assert_eq!(DocumentHeader::decode(&encoded), Ok(header));

        let mut bad_doc_id = encoded;
        bad_doc_id[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            DocumentHeader::decode(&bad_doc_id),
            Err(PackFormatError::InvalidNodeId { raw: 0 })
        );

        let mut bad_quirks = encoded;
        bad_quirks[12..16].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(
            DocumentHeader::decode(&bad_quirks),
            Err(PackFormatError::InvalidQuirksMode { bits: 3 })
        );

        let dir = BatchDirEntry {
            offset: 96,
            len: 416,
        };
        assert_eq!(BatchDirEntry::decode(&dir.encode()), dir);

        let script = ScriptEntry {
            kind: ScriptKind::ExternalModule,
            flags: ScriptFlags::ASYNC,
            node_id: NodeId::new(7, 0).unwrap(),
            payload_str: 0,
            base_url_str: 1,
        };
        assert_eq!(ScriptEntry::decode(&script.encode()), Ok(script));

        let bad_sheet = ScriptEntry {
            kind: ScriptKind::ExternalStylesheet,
            flags: ScriptFlags::DEFER,
            ..script
        };
        assert_eq!(
            ScriptEntry::decode(&bad_sheet.encode()),
            Err(PackFormatError::InvalidScriptFlags { kind: 4, flags: 1 })
        );
    }

    #[test]
    fn utf16_string_table_round_trips_including_empty_and_lone_surrogate() {
        let empty = encode_utf16_string_table::<&[u16]>(&[]).unwrap();
        assert_eq!(empty, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode_utf16_string_table(&empty), Ok(Vec::new()));

        let strings: Vec<Vec<u16>> = vec![
            "".encode_utf16().collect(),
            "/assets/app.js".encode_utf16().collect(),
            vec![0xD800, 0x0041],
        ];
        let encoded = encode_utf16_string_table(&strings).unwrap();
        assert_eq!(decode_utf16_string_table(&encoded), Ok(strings));
    }

    #[test]
    fn canonical_pack_paths_and_mimes() {
        assert!(is_valid_pack_path("/index.html"));
        assert!(is_valid_pack_path("/__squark/document.bin"));
        assert!(is_valid_pack_path("/assets/index-DivIg4U3.js"));
        assert!(!is_valid_pack_path(""));
        assert!(!is_valid_pack_path("/"));
        assert!(!is_valid_pack_path("index.html"));
        assert!(!is_valid_pack_path("//index.html"));
        assert!(!is_valid_pack_path("/a/"));
        assert!(!is_valid_pack_path("/a/./b"));
        assert!(!is_valid_pack_path("/a/../b"));
        assert!(!is_valid_pack_path("/a\\b"));
        assert!(!is_valid_pack_path("/a\0b"));

        assert_eq!(
            mime_from_id(mime_id_for_extension("WOFF2")),
            Some("font/woff2")
        );
        assert_eq!(
            mime_from_id(mime_id_for_extension("mjs")),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(mime_id_from_str("image/svg+xml"), Some(11));
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
