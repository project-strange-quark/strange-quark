// SPDX-License-Identifier: MIT
//! Version 1 journal validation and encoding for runtime flushes and packs.

#![forbid(unsafe_code)]

mod batch;
mod decoder;
mod writer;

pub use batch::{BatchError, StringTable, StringTableError, Utf16};
pub use decoder::{Atom, BatchDecoder, DecodedBatch, JournalError, RecordError};
pub use writer::{BatchWriter, Operand, WriteError};
