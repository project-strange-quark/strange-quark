// SPDX-License-Identifier: MIT

use squark_types::journal::{
    BATCH_HEADER_SIZE, BATCH_VERSION, BatchHeader, MAX_RECORD_BYTES, RECORD_SIZE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchError {
    MissingHeader,
    Version(u32),
    RecordBytes(u32),
    Strings(StringTableError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringTableError {
    Length,
    Offset { index: usize, value: u32 },
}

/// A string in the original batch bytes. Each code unit is read little-endian
/// so a batch can start at any byte alignment.
#[derive(Clone, Copy, Debug)]
pub struct Utf16<'a>(&'a [[u8; 2]]);

impl<'a> Utf16<'a> {
    pub fn units(self) -> impl ExactSizeIterator<Item = u16> + 'a {
        self.0.iter().copied().map(u16::from_le_bytes)
    }

    /// Code units in their original little-endian representation.
    pub fn le_units(self) -> &'a [[u8; 2]] {
        self.0
    }

    pub fn to_string_lossy(self) -> String {
        char::decode_utf16(self.units())
            .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

/// Validated, borrowed UTF-16LE string table for a batch or a pack script table.
#[derive(Clone, Copy, Debug)]
pub struct StringTable<'a> {
    offsets: &'a [[u8; 4]],
    units: &'a [[u8; 2]],
}

impl<'a> StringTable<'a> {
    /// Reads the offset and code-unit regions, using the counts from the header.
    pub fn new(bytes: &'a [u8], count: u32, code_units: u32) -> Result<Self, StringTableError> {
        let offsets_bytes = (count as usize)
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .ok_or(StringTableError::Length)?;
        let units_bytes = (code_units as usize)
            .checked_mul(2)
            .ok_or(StringTableError::Length)?;
        if offsets_bytes.checked_add(units_bytes) != Some(bytes.len()) {
            return Err(StringTableError::Length);
        }
        let (offsets, units) = bytes.split_at(offsets_bytes);
        let (offsets, _) = offsets.as_chunks::<4>();
        let (units, _) = units.as_chunks::<2>();
        let mut previous = 0;
        for (index, offset) in offsets.iter().enumerate() {
            let value = u32::from_le_bytes(*offset);
            if value < previous
                || value > code_units
                || (index == 0 && value != 0)
                || (index + 1 == offsets.len() && value != code_units)
            {
                return Err(StringTableError::Offset { index, value });
            }
            previous = value;
        }
        Ok(Self { offsets, units })
    }

    pub fn get(self, index: u32) -> Option<Utf16<'a>> {
        let index = index as usize;
        if index >= self.len() {
            return None;
        }
        let start = u32::from_le_bytes(self.offsets[index]) as usize;
        let end = u32::from_le_bytes(self.offsets[index + 1]) as usize;
        Some(Utf16(&self.units[start..end]))
    }

    pub fn len(self) -> usize {
        self.offsets.len() - 1
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub(crate) struct Batch<'a> {
    pub records: &'a [[u8; RECORD_SIZE]],
    pub strings: StringTable<'a>,
}

impl<'a> Batch<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, BatchError> {
        let (header, bytes) = bytes
            .split_first_chunk::<BATCH_HEADER_SIZE>()
            .ok_or(BatchError::MissingHeader)?;
        let header = BatchHeader::decode(header);
        if header.version != BATCH_VERSION {
            return Err(BatchError::Version(header.version));
        }
        let record_bytes = header.record_bytes as usize;
        if record_bytes > MAX_RECORD_BYTES || !record_bytes.is_multiple_of(RECORD_SIZE) {
            return Err(BatchError::RecordBytes(header.record_bytes));
        }
        let (records, strings) = bytes
            .split_at_checked(record_bytes)
            .ok_or(BatchError::RecordBytes(header.record_bytes))?;
        let strings = StringTable::new(strings, header.string_count, header.string_code_units)
            .map_err(BatchError::Strings)?;
        Ok(Self {
            records: records.as_chunks::<RECORD_SIZE>().0,
            strings,
        })
    }
}
