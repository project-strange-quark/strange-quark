// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::fmt;

use squark_types::journal::{
    AtomKind, BATCH_HEADER_SIZE, BATCH_VERSION, BatchHeader, MAX_RECORD_BYTES, Opcode, RECORD_SIZE,
    Record,
};

/// A numeric word or a string whose index the writer assigns in the same batch.
#[derive(Clone, Copy, Debug)]
pub enum Operand<'a> {
    Word(u32),
    Utf8(&'a str),
    Utf16(&'a [u16]),
}

impl From<u32> for Operand<'_> {
    fn from(word: u32) -> Self {
        Self::Word(word)
    }
}

impl<'a> From<&'a str> for Operand<'a> {
    fn from(text: &'a str) -> Self {
        Self::Utf8(text)
    }
}

impl<'a> From<&'a [u16]> for Operand<'a> {
    fn from(text: &'a [u16]) -> Self {
        Self::Utf16(text)
    }
}

impl Operand<'_> {
    fn string_len(self) -> Option<usize> {
        match self {
            Self::Word(_) => None,
            Self::Utf8(text) => Some(text.encode_utf16().count()),
            Self::Utf16(text) => Some(text.len()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    StringTableTooLarge,
    AtomIdsExhausted,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StringTableTooLarge => "journal string table exceeds the wire size",
            Self::AtomIdsExhausted => "journal atom ids exhausted",
        })
    }
}

impl std::error::Error for WriteError {}

/// Writes complete version 1 batches in journal order for a document.bin
/// directory. The caller supplies the opcode operands and allocates node IDs.
#[derive(Debug, Default)]
pub struct BatchWriter {
    batches: Vec<Vec<u8>>,
    records: Vec<[u8; RECORD_SIZE]>,
    offsets: Vec<u32>,
    units: Vec<u16>,
    atoms: HashMap<AtomKind, HashMap<Vec<u16>, u32>>,
    highest_atom: u32,
}

impl BatchWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns exact UTF-16 units and writes DEFINE_ATOM on the first use.
    /// Writers using this method let it allocate all of their atom IDs.
    pub fn intern(&mut self, kind: AtomKind, text: &[u16]) -> Result<u32, WriteError> {
        if let Some(id) = self.atoms.get(&kind).and_then(|atoms| atoms.get(text)) {
            return Ok(*id);
        }
        let id = self
            .highest_atom
            .checked_add(1)
            .ok_or(WriteError::AtomIdsExhausted)?;
        self.record(Opcode::DefineAtom, 0, id, text, kind as u32)?;
        self.atoms
            .entry(kind)
            .or_default()
            .insert(text.to_vec(), id);
        self.highest_atom = id;
        Ok(id)
    }

    /// Appends a record, rolling over at 64 KiB. Supply a string operand
    /// wherever the opcode needs a string index to keep it with the record.
    pub fn record<'b, 'c>(
        &mut self,
        op: Opcode,
        flags: u8,
        a: u32,
        b: impl Into<Operand<'b>>,
        c: impl Into<Operand<'c>>,
    ) -> Result<(), WriteError> {
        if self.records.len() == MAX_RECORD_BYTES / RECORD_SIZE {
            self.end_batch();
        }
        let (b, c) = (b.into(), c.into());
        let lengths = [b.string_len(), c.string_len()];
        lengths
            .into_iter()
            .flatten()
            .try_fold(self.units.len() as u32, |total, length| {
                total.checked_add(u32::try_from(length).ok()?)
            })
            .ok_or(WriteError::StringTableTooLarge)?;

        let b = self.write_operand(b);
        let c = self.write_operand(c);
        self.records.push(Record { op, flags, a, b, c }.encode());
        Ok(())
    }

    fn write_operand(&mut self, operand: Operand<'_>) -> u32 {
        match operand {
            Operand::Word(word) => return word,
            Operand::Utf8(text) => self.units.extend(text.encode_utf16()),
            Operand::Utf16(text) => self.units.extend_from_slice(text),
        }
        let index = self.offsets.len() as u32;
        self.offsets.push(self.units.len() as u32);
        index
    }

    /// Ends the current batch early. Atom interning continues across batches.
    pub fn end_batch(&mut self) {
        if self.records.is_empty() {
            return;
        }
        let record_bytes = self.records.len() * RECORD_SIZE;
        let header = BatchHeader {
            record_bytes: record_bytes as u32,
            string_count: self.offsets.len() as u32,
            string_code_units: self.units.len() as u32,
            version: BATCH_VERSION,
        };
        let mut bytes = Vec::with_capacity(
            BATCH_HEADER_SIZE + record_bytes + (self.offsets.len() + 1) * 4 + self.units.len() * 2,
        );
        bytes.extend(header.encode());
        bytes.extend(self.records.drain(..).flatten());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(self.offsets.drain(..).flat_map(u32::to_le_bytes));
        bytes.extend(self.units.drain(..).flat_map(u16::to_le_bytes));
        self.batches.push(bytes);
    }

    pub fn finish(mut self) -> Vec<Vec<u8>> {
        self.end_batch();
        self.batches
    }
}
