// SPDX-License-Identifier: MIT
//! `@font-face` rules. Their sources are fetched when the rule set changes;
//! a face is decoded and registered the first time CSS font matching picks
//! it for a character, as browsers load only the faces text uses.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use parley::{FontContext, fontique};
use squark_assets::Resources;
use style::font_face::{Descriptors, Source};
use url::Url;

use crate::FaceAttributes;
use crate::document_fonts::Coverage;

/// Where an `@font-face` rule's `src` finds a font.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FaceSource {
    Url(Url),
    /// An installed font, by its full or PostScript name.
    Local(String),
}

/// An `@font-face` rule in effect.
#[derive(Clone, Debug, PartialEq)]
pub struct WebFont {
    pub family: String,
    pub attributes: FaceAttributes,
    pub sources: Vec<FaceSource>,
}

impl WebFont {
    /// The face an `@font-face` rule describes, or `None` when it lacks a
    /// family or a source, or a descriptor does not compute.
    pub fn from_descriptors(descriptors: &Descriptors) -> Option<Self> {
        let family = descriptors.font_family.as_ref()?.name.to_string();
        let sources = descriptors
            .src
            .as_ref()?
            .0
            .iter()
            .filter_map(|source| match source {
                Source::Url(source) => source
                    .url
                    .url()
                    .map(|url| FaceSource::Url(url.as_ref().clone())),
                Source::Local(name) => Some(FaceSource::Local(name.name.to_string())),
            })
            .collect();
        Some(Self {
            family,
            attributes: FaceAttributes::from_descriptors(descriptors)?,
            sources,
        })
    }
}

/// The family name a rule's face is registered under. Each face gets its
/// own, so matching, not the font collection, chooses between faces.
pub(crate) fn alias(index: usize) -> String {
    format!("StrangeQuark CSS Face {index}")
}

pub(crate) fn register(
    context: &mut FontContext,
    blob: fontique::Blob<u8>,
    family: &str,
    attributes: &FaceAttributes,
    coverage: &[RangeInclusive<u32>],
) -> bool {
    let blob = match restrict_cmap(blob.data(), coverage) {
        Some(font) => fontique::Blob::new(Arc::new(font)),
        None => blob,
    };
    let overrides = fontique::FontInfoOverride {
        family_name: Some(family),
        style: Some(crate::css::font_style(attributes.style[0])),
        weight: Some(fontique::FontWeight::new(attributes.weight[0])),
        width: Some(fontique::FontWidth::from_percentage(attributes.width[0])),
        ..Default::default()
    };
    !context
        .collection
        .register_fonts(blob, Some(overrides))
        .is_empty()
}

/// A copy of the font whose cmap maps only the codepoints in `coverage`, so
/// that font fallback leaves the rest to other faces, as `unicode-range`
/// does in browsers. `None` when the font maps nothing outside `coverage`,
/// or is a collection or a symbol font.
fn restrict_cmap(data: &[u8], coverage: &[RangeInclusive<u32>]) -> Option<Vec<u8>> {
    use skrifa::MetadataProvider;
    use skrifa::raw::{FileRef, types::Tag};
    const CMAP: Tag = Tag::new(b"cmap");
    if crate::document_fonts::is_full(coverage) {
        return None;
    }
    let Ok(FileRef::Font(font)) = FileRef::new(data) else {
        return None;
    };
    let charmap = font.charmap();
    if charmap.is_symbol() {
        return None;
    }
    let mut outside = false;
    let mut groups: Vec<[u32; 3]> = Vec::new();
    for (codepoint, glyph) in charmap.mappings() {
        if !crate::document_fonts::covers(coverage, codepoint) {
            outside = true;
            continue;
        }
        let glyph = glyph.to_u32();
        match groups.last_mut() {
            Some([start, end, first])
                if *end + 1 == codepoint && *first + (codepoint - *start) == glyph =>
            {
                *end = codepoint;
            }
            _ => groups.push([codepoint, codepoint, glyph]),
        }
    }
    if !outside {
        return None;
    }
    // One format 12 subtable for the Windows Unicode full repertoire.
    let mut cmap = Vec::with_capacity(28 + 12 * groups.len());
    for word in [0_u16, 1, 3, 10] {
        cmap.extend(word.to_be_bytes());
    }
    cmap.extend(12_u32.to_be_bytes());
    cmap.extend([0, 12, 0, 0]);
    cmap.extend(u32::try_from(16 + 12 * groups.len()).ok()?.to_be_bytes());
    cmap.extend(0_u32.to_be_bytes());
    cmap.extend(u32::try_from(groups.len()).ok()?.to_be_bytes());
    for group in groups {
        for value in group {
            cmap.extend(value.to_be_bytes());
        }
    }

    let directory = &font.table_directory;
    let records = directory.table_records();
    let count = u16::try_from(records.len()).ok()?;
    let selector = 15 - count.max(1).leading_zeros() as u16;
    let range = 16_u16 << selector;
    let mut out = Vec::with_capacity(data.len());
    out.extend(directory.sfnt_version().to_be_bytes());
    for word in [count, range, selector, count * 16 - range] {
        out.extend(word.to_be_bytes());
    }
    let mut offset = 12 + 16 * records.len();
    let mut tables = Vec::with_capacity(records.len());
    for record in records {
        let tag = record.tag();
        let (bytes, checksum) = if tag == CMAP {
            (cmap.as_slice(), checksum(&cmap))
        } else {
            (font.table_data(tag)?.as_bytes(), record.checksum())
        };
        out.extend(tag.to_be_bytes());
        out.extend(checksum.to_be_bytes());
        out.extend(u32::try_from(offset).ok()?.to_be_bytes());
        out.extend(u32::try_from(bytes.len()).ok()?.to_be_bytes());
        offset += bytes.len().next_multiple_of(4);
        tables.push(bytes);
    }
    for bytes in tables {
        out.extend(bytes);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    Some(out)
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

enum Fetched {
    /// Bytes that look like a font, not yet decoded.
    Raw(Arc<[u8]>),
    Decoded(fontique::Blob<u8>),
    Invalid,
}

impl Fetched {
    fn new(bytes: &[u8]) -> Self {
        const MAGIC: [&[u8; 4]; 5] = [b"wOF2", b"\0\x01\0\0", b"OTTO", b"true", b"ttcf"];
        if MAGIC.iter().any(|magic| bytes.starts_with(*magic)) {
            Self::Raw(Arc::from(bytes))
        } else {
            Self::Invalid
        }
    }

    fn blob(&mut self) -> Option<fontique::Blob<u8>> {
        if let Self::Raw(bytes) = self {
            *self = match crate::decode_font(bytes) {
                Some(font) => Self::Decoded(fontique::Blob::new(Arc::new(font))),
                None => Self::Invalid,
            };
        }
        match self {
            Self::Decoded(blob) => Some(blob.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Load {
    Unloaded,
    Loaded,
    Failed,
}

/// Load state for the faces in `Fonts::faces`, in the same order.
#[derive(Default)]
pub(crate) struct WebFaces {
    loads: Vec<Load>,
    coverage: Vec<Coverage>,
    fetched: HashMap<Url, Fetched>,
    installed: HashMap<String, Option<fontique::Blob<u8>>>,
}

impl WebFaces {
    /// Fetches each face's sources up to the first one that looks like a
    /// font. Bytes already fetched or decoded for a URL carry over.
    pub(crate) fn sync(
        &mut self,
        resources: Resources<'_>,
        faces: &[WebFont],
        coverage: Vec<Coverage>,
    ) {
        let mut previous = std::mem::take(&mut self.fetched);
        for face in faces {
            for source in &face.sources {
                let FaceSource::Url(url) = source else {
                    continue;
                };
                if let Some(fetched) = self.fetched.get(url) {
                    if !matches!(fetched, Fetched::Invalid) {
                        break;
                    }
                    continue;
                }
                let fetched = match previous.remove(url) {
                    Some(fetched) => fetched,
                    None => match resources.get(url) {
                        Some(resource) => Fetched::new(&resource.bytes),
                        None => continue,
                    },
                };
                let usable = !matches!(fetched, Fetched::Invalid);
                self.fetched.insert(url.clone(), fetched);
                if usable {
                    break;
                }
            }
        }
        self.loads = vec![Load::Unloaded; faces.len()];
        self.coverage = coverage;
    }

    /// The characters the face renders.
    pub(crate) fn coverage(&self, index: usize) -> &[RangeInclusive<u32>] {
        &self.coverage[index]
    }

    /// Registers the face from its first source that decodes, once.
    pub(crate) fn load(
        &mut self,
        index: usize,
        faces: &[WebFont],
        context: &mut FontContext,
    ) -> bool {
        match self.loads[index] {
            Load::Loaded => return true,
            Load::Failed => return false,
            Load::Unloaded => {}
        }
        let face = &faces[index];
        let family = alias(index);
        let loaded = face.sources.iter().any(|source| {
            let blob = match source {
                FaceSource::Url(url) => self.fetched.get_mut(url).and_then(Fetched::blob),
                FaceSource::Local(name) => self
                    .installed
                    .entry(name.to_lowercase())
                    .or_insert_with(|| installed(context, name))
                    .clone(),
            };
            blob.is_some_and(|blob| {
                register(
                    context,
                    blob,
                    &family,
                    &face.attributes,
                    &self.coverage[index],
                )
            })
        });
        self.loads[index] = if loaded { Load::Loaded } else { Load::Failed };
        loaded
    }
}

/// An installed font whose full or PostScript name is `name`, as `local()`
/// matches it. Only families whose name begins the requested one are read,
/// which holds for the names fonts give themselves. A font inside a
/// collection file is skipped: its family alias would cover every font in
/// the file.
fn installed(context: &mut FontContext, name: &str) -> Option<fontique::Blob<u8>> {
    use skrifa::{MetadataProvider, raw::FileRef, string::StringId};
    let wanted = name.trim().to_lowercase();
    let compact = |name: &str| {
        name.chars()
            .filter(|ch| !ch.is_whitespace() && *ch != '-')
            .collect::<String>()
            .to_lowercase()
    };
    let wanted_compact = compact(&wanted);
    let families: Vec<_> = context
        .collection
        .family_names()
        .filter(|family| {
            let family = family.to_lowercase();
            !family.is_empty()
                && (wanted.starts_with(&family) || wanted_compact.starts_with(&compact(&family)))
        })
        .map(str::to_owned)
        .collect();
    for family in families {
        let Some(info) = context.collection.family_by_name(&family) else {
            continue;
        };
        for font in info.fonts() {
            let Some(blob) = font.load(Some(&mut context.source_cache)) else {
                continue;
            };
            let Ok(FileRef::Font(file)) = FileRef::new(blob.data()) else {
                continue;
            };
            let matches = [StringId::FULL_NAME, StringId::POSTSCRIPT_NAME]
                .into_iter()
                .flat_map(|id| file.localized_strings(id))
                .any(|string| string.chars().collect::<String>().to_lowercase() == wanted);
            if matches {
                return Some(blob);
            }
        }
    }
    None
}
