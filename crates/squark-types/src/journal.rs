// SPDX-License-Identifier: MIT
//! The journal record format (docs/architecture.md §4, "Journal records").
//!
//! Each record is 16 bytes, little-endian: opcode (u8), flags (u8), two bytes
//! of padding (zero), then three u32 operands. Strings live in a side table
//! and records carry their index; tag, attribute and property names are atoms.
//! squark-dom's encoder (JavaScript) and squark-journal's decoder (Rust) must agree
//! with this file byte for byte.

/// The size of one record in bytes.
pub const RECORD_SIZE: usize = 16;
pub const BATCH_HEADER_SIZE: usize = 16;
pub const MAX_RECORD_BYTES: usize = 65_536;
pub const BATCH_VERSION: u32 = 1;
pub const NO_STRING: u32 = u32::MAX;
/// Flag bit 0 on SET_STYLE. The string operand contains only the CSS value.
pub const STYLE_IMPORTANT: u8 = 1 << 0;
/// SET_SELECTION flags bit for a selection whose focus is at its start.
pub const SELECTION_BACKWARD: u8 = 1 << 0;
pub const FOCUS_BLUR: u32 = 1 << 0;
pub const FOCUS_PREVENT_SCROLL: u32 = 1 << 1;

/// A version 1 batch starts with four little-endian words. The record region,
/// `string_count + 1` offsets, and UTF-16LE code units follow it in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchHeader {
    pub record_bytes: u32,
    pub string_count: u32,
    pub string_code_units: u32,
    pub version: u32,
}

impl BatchHeader {
    pub fn encode(self) -> [u8; BATCH_HEADER_SIZE] {
        let mut bytes = [0; BATCH_HEADER_SIZE];
        for (word, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip([
            self.record_bytes,
            self.string_count,
            self.string_code_units,
            self.version,
        ]) {
            *word = value.to_le_bytes();
        }
        bytes
    }

    pub fn decode(bytes: &[u8; BATCH_HEADER_SIZE]) -> Self {
        let [record_bytes, string_count, string_code_units, version] =
            <[[u8; 4]; 4]>::try_from(bytes.as_chunks::<4>().0)
                .expect("the header is four words")
                .map(u32::from_le_bytes);
        Self {
            record_bytes,
            string_count,
            string_code_units,
            version,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum AtomKind {
    Tag = 1,
    Attribute = 2,
    CssProperty = 3,
    Namespace = 4,
    /// Namespace URI, U+0000, qualified name.
    QualifiedAttribute = 5,
}

impl AtomKind {
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::Tag,
            2 => Self::Attribute,
            3 => Self::CssProperty,
            4 => Self::Namespace,
            5 => Self::QualifiedAttribute,
            _ => return None,
        })
    }
}

/// CREATE_NODE's supported DOM nodeType values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum NodeType {
    Comment = 8,
    Document = 9,
    DocumentType = 10,
    DocumentFragment = 11,
}

impl NodeType {
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            8 => Self::Comment,
            9 => Self::Document,
            10 => Self::DocumentType,
            11 => Self::DocumentFragment,
            _ => return None,
        })
    }

    pub const fn has_string(self) -> bool {
        matches!(self, Self::Comment | Self::DocumentType)
    }
}

/// FONT_FACE's flag byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FontFaceAction {
    Define = 0,
    Add = 1,
    Remove = 2,
}

impl FontFaceAction {
    pub const fn from_raw(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Define,
            1 => Self::Add,
            2 => Self::Remove,
            _ => return None,
        })
    }
}

/// SET_STATE uses this bit set in both its state and its mask operand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElementState(u32);

impl ElementState {
    pub const HOVER: Self = Self(1 << 0);
    pub const ACTIVE: Self = Self(1 << 1);
    pub const FOCUS: Self = Self(1 << 2);
    pub const FOCUS_VISIBLE: Self = Self(1 << 3);
    pub const FOCUS_WITHIN: Self = Self(1 << 4);
    pub const CHECKED: Self = Self(1 << 5);
    pub const INDETERMINATE: Self = Self(1 << 6);
    pub const DISABLED: Self = Self(1 << 7);
    pub const SELECTED: Self = Self(1 << 8);
    pub const READ_ONLY: Self = Self(1 << 9);
    pub const PLACEHOLDER_SHOWN: Self = Self(1 << 10);
    pub const INVALID: Self = Self(1 << 11);

    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits >> 12 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// Non-passive listener bits. LISTENERS widens this set to a u32 operand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenerFlags(u8);

impl ListenerFlags {
    pub const NON_PASSIVE_WHEEL: Self = Self(1 << 0);
    pub const NON_PASSIVE_TOUCHMOVE: Self = Self(1 << 1);

    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits >> 2 == 0 {
            Some(Self(bits as u8))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u32 {
        self.0 as u32
    }
}

/// Opcodes. 0x01–0x0F are listed in docs/architecture.md §4; the comments on
/// 0x10–0x1B give their operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    CreateElement = 0x01,
    CreateText = 0x02,
    Insert = 0x03,
    Remove = 0x04,
    SetAttr = 0x05,
    RemoveAttr = 0x06,
    SetText = 0x07,
    SetStyle = 0x08,
    SetState = 0x09,
    SetValue = 0x0A,
    SheetInsert = 0x0B,
    SheetDelete = 0x0C,
    ScrollTo = 0x0D,
    /// NodeId, focus bits (FOCUS_BLUR and FOCUS_PREVENT_SCROLL), zero.
    Focus = 0x0E,
    Release = 0x0F,
    /// Atom id, string index, AtomKind.
    DefineAtom = 0x10,
    /// Canvas NodeId, staged buffer id, byte length.
    CanvasSubmit = 0x11,
    /// Face id, family string index, descriptor JSON string index. Flags are
    /// FontFaceAction.
    FontFace = 0x12,
    /// Canvas NodeId, bitmap width, bitmap height.
    CanvasSize = 0x13,
    /// NodeId, NodeType, and a string index for a comment or doctype.
    /// A template content fragment carries its owner's NodeId. An ordinary
    /// fragment or inert document carries NO_STRING.
    CreateNode = 0x14,
    /// Target NodeId (0 for Window), ListenerFlags bits, zero.
    Listeners = 0x15,
    /// Text control NodeId, UTF-16 start, UTF-16 end.
    SetSelection = 0x16,
    /// Text control NodeId, preedit string index, UTF-16 cursor within preedit.
    SetPreedit = 0x17,
    /// New shadow root NodeId, host element NodeId, zero. The shadow root is
    /// a fragment that no node contains: its host renders it in place of its
    /// children, and the host stays its host until both are released.
    AttachShadow = 0x18,
    /// Slot element NodeId, then a slottable NodeId to append to the slot's
    /// assigned nodes, or zero to clear them, and zero. squark-dom runs slot
    /// assignment and sends a slot's whole list whenever it changes.
    AssignSlot = 0x19,
    /// New constructed stylesheet id (from the NodeId space, and released
    /// like a node), media string index or NO_STRING, zero. SHEET_INSERT and
    /// SHEET_DELETE edit its rules like an owner element's.
    CreateSheet = 0x1A,
    /// Document or shadow root NodeId, then a constructed stylesheet id to
    /// append to its adopted stylesheets, or zero to clear them, and zero.
    AdoptSheet = 0x1B,
}

impl Opcode {
    pub fn from_u8(b: u8) -> Option<Opcode> {
        use Opcode::*;
        Some(match b {
            0x01 => CreateElement,
            0x02 => CreateText,
            0x03 => Insert,
            0x04 => Remove,
            0x05 => SetAttr,
            0x06 => RemoveAttr,
            0x07 => SetText,
            0x08 => SetStyle,
            0x09 => SetState,
            0x0A => SetValue,
            0x0B => SheetInsert,
            0x0C => SheetDelete,
            0x0D => ScrollTo,
            0x0E => Focus,
            0x0F => Release,
            0x10 => DefineAtom,
            0x11 => CanvasSubmit,
            0x12 => FontFace,
            0x13 => CanvasSize,
            0x14 => CreateNode,
            0x15 => Listeners,
            0x16 => SetSelection,
            0x17 => SetPreedit,
            0x18 => AttachShadow,
            0x19 => AssignSlot,
            0x1A => CreateSheet,
            0x1B => AdoptSheet,
            _ => return None,
        })
    }
}

/// One decoded record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub op: Opcode,
    pub flags: u8,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

/// Why a record could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The buffer length is not a multiple of [`RECORD_SIZE`].
    Truncated { len: usize },
    /// An opcode this version does not know.
    UnknownOpcode { offset: usize, opcode: u8 },
    /// Padding bytes were not zero.
    BadPadding { offset: usize },
}

impl Record {
    pub fn encode(&self) -> [u8; RECORD_SIZE] {
        let mut out = [0u8; RECORD_SIZE];
        out[0] = self.op as u8;
        out[1] = self.flags;
        out[4..8].copy_from_slice(&self.a.to_le_bytes());
        out[8..12].copy_from_slice(&self.b.to_le_bytes());
        out[12..16].copy_from_slice(&self.c.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; RECORD_SIZE], offset: usize) -> Result<Record, DecodeError> {
        let op = Opcode::from_u8(bytes[0]).ok_or(DecodeError::UnknownOpcode {
            offset,
            opcode: bytes[0],
        })?;
        if bytes[2] != 0 || bytes[3] != 0 {
            return Err(DecodeError::BadPadding { offset });
        }
        let u = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        Ok(Record {
            op,
            flags: bytes[1],
            a: u(4),
            b: u(8),
            c: u(12),
        })
    }
}

/// Decodes a whole buffer, stopping at the first bad record.
pub fn decode_all(buf: &[u8]) -> Result<Vec<Record>, DecodeError> {
    if !buf.len().is_multiple_of(RECORD_SIZE) {
        return Err(DecodeError::Truncated { len: buf.len() });
    }
    let (records, _) = buf.as_chunks::<RECORD_SIZE>();
    records
        .iter()
        .enumerate()
        .map(|(i, arr)| Record::decode(arr, i * RECORD_SIZE))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_opcode_round_trips() {
        for b in 0x01..=0x1Bu8 {
            let op = Opcode::from_u8(b).unwrap();
            let r = Record {
                op,
                flags: 0x5A,
                a: 1,
                b: u32::MAX,
                c: 0xDEAD_BEEF,
            };
            assert_eq!(Record::decode(&r.encode(), 0), Ok(r));
        }
    }

    #[test]
    fn layout_is_little_endian_with_zero_padding() {
        let r = Record {
            op: Opcode::Insert,
            flags: 0,
            a: 0x0102_0304,
            b: 0,
            c: 7,
        };
        let bytes = r.encode();
        assert_eq!(&bytes[..8], &[0x03, 0, 0, 0, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(&bytes[12..], &[7, 0, 0, 0]);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert_eq!(
            decode_all(&[0u8; 15]),
            Err(DecodeError::Truncated { len: 15 })
        );
        let mut b = Record {
            op: Opcode::Focus,
            flags: 0,
            a: 0,
            b: 0,
            c: 0,
        }
        .encode();
        b[2] = 1;
        assert_eq!(
            Record::decode(&b, 32),
            Err(DecodeError::BadPadding { offset: 32 })
        );
        b[0] = 0xFF;
        assert!(matches!(
            Record::decode(&b, 0),
            Err(DecodeError::UnknownOpcode { opcode: 0xFF, .. })
        ));
    }
}
