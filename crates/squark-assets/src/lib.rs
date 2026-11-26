// SPDX-License-Identifier: MIT
//! Read-only views of version 1 packs and their initial documents.

mod document;
mod pack;

pub use document::{InitialDocument, Script};
pub use pack::{Asset, Pack};
pub use squark_types::pack::{QuirksMode, ScriptKind};

use std::{fmt, io, ops::Range};

#[derive(Debug)]
pub enum PackError {
    Io(io::Error),
    Map(memmap2::MapIfSafeError),
    Format(squark_types::pack::PackFormatError),
    Invalid(&'static str),
    MissingDocument,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Map(error) => error.fmt(f),
            Self::Format(error) => error.fmt(f),
            Self::Invalid(field) => write!(f, "invalid pack or document: {field}"),
            Self::MissingDocument => f.write_str("pack is missing /__squark/document.bin"),
        }
    }
}

impl std::error::Error for PackError {}

impl From<io::Error> for PackError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<squark_types::pack::PackFormatError> for PackError {
    fn from(error: squark_types::pack::PackFormatError) -> Self {
        Self::Format(error)
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..][..4].try_into().unwrap())
}

fn range(offset: u64, size: u64, total: usize) -> Option<Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = usize::try_from(offset.checked_add(size)?).ok()?;
    (end <= total).then_some(start..end)
}

mod images;
mod remote_images;
mod svg;
pub use images::{
    BlobResolver, DecodedImage, ElementImage, Image, ImageCatalog, IntrinsicSize, Resource,
    Resources, premultiply_rgba, svg_intrinsic,
};
pub use remote_images::RemoteResources;
pub use svg::{svg_bounds, svg_geometry};
