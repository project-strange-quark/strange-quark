// SPDX-License-Identifier: MIT
//! `@font-face` rules. Their sources are fetched when the rule set changes;
//! a face is decoded and registered the first time CSS font matching picks
//! it for a character, as browsers load only the faces text uses.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, OnceLock};

use parley::{FontContext, fontique};
use skrifa::{FontRef, raw::types::Tag};
use squark_assets::Resources;
use style::font_face::{Descriptors, Source};
use url::Url;

use crate::FaceAttributes;
use crate::document_fonts::{Coverage, FaceSet};

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

/// The font with its cmap cut to `coverage`, or the font itself when it
/// maps nothing outside it.
pub(crate) fn restricted(
    blob: fontique::Blob<u8>,
    coverage: &[RangeInclusive<u32>],
) -> fontique::Blob<u8> {
    match restrict_cmap(blob.data(), coverage) {
        Some(font) => fontique::Blob::new(Arc::new(font)),
        None => blob,
    }
}

pub(crate) fn register(
    context: &mut FontContext,
    blob: fontique::Blob<u8>,
    family: &str,
    attributes: &FaceAttributes,
) -> bool {
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
    use skrifa::raw::FileRef;
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
    repack_font(&font, Some((CMAP, &cmap)))
}

/// Write one standalone sfnt, optionally substituting a table.
fn repack_font(font: &FontRef<'_>, replacement: Option<(Tag, &[u8])>) -> Option<Vec<u8>> {
    let directory = &font.table_directory;
    let records = directory.table_records();
    let count = u16::try_from(records.len()).ok()?;
    let selector = 15 - count.max(1).leading_zeros() as u16;
    let range = 16_u16 << selector;
    let mut offset = 12 + 16 * records.len();
    let mut out = Vec::with_capacity(offset);
    out.extend(directory.sfnt_version().to_be_bytes());
    for word in [count, range, selector, count * 16 - range] {
        out.extend(word.to_be_bytes());
    }
    let mut tables = Vec::with_capacity(records.len());
    for record in records {
        let tag = record.tag();
        let (bytes, checksum) =
            if let Some((_, data)) = replacement.filter(|(table, _)| *table == tag) {
                (data, checksum(data))
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
    out.reserve_exact(offset - out.len());
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

/// A `src` URL's bytes. A font is decoded the first time it is used, and
/// clones share the decoded font, so text layout and the font metrics
/// provider decode a face once.
#[derive(Clone)]
pub(crate) enum Fetched {
    /// Bytes that look like a font.
    Font(Arc<LazyFont>),
    Invalid,
}

pub(crate) struct LazyFont {
    bytes: Arc<[u8]>,
    decoded: OnceLock<Option<fontique::Blob<u8>>>,
}

impl Fetched {
    fn new(bytes: &[u8]) -> Self {
        const MAGIC: [&[u8; 4]; 5] = [b"wOF2", b"\0\x01\0\0", b"OTTO", b"true", b"ttcf"];
        if MAGIC.iter().any(|magic| bytes.starts_with(*magic)) {
            Self::Font(Arc::new(LazyFont {
                bytes: Arc::from(bytes),
                decoded: OnceLock::new(),
            }))
        } else {
            Self::Invalid
        }
    }

    pub(crate) fn blob(&self) -> Option<fontique::Blob<u8>> {
        match self {
            Self::Font(font) => font
                .decoded
                .get_or_init(|| {
                    crate::decode_font(&font.bytes).map(|font| fontique::Blob::new(Arc::new(font)))
                })
                .clone(),
            Self::Invalid => None,
        }
    }

    /// Whether the bytes may still turn out to be a font.
    fn usable(&self) -> bool {
        match self {
            Self::Font(font) => font.decoded.get().is_none_or(Option::is_some),
            Self::Invalid => false,
        }
    }

    fn decoded_id(&self) -> Option<u64> {
        match self {
            Self::Font(font) => font.decoded.get()?.as_ref().map(fontique::Blob::id),
            Self::Invalid => None,
        }
    }
}

/// Fetches each face's sources up to the first one that looks like a font.
/// Bytes already fetched or decoded for a URL carry over.
pub(crate) fn fetch(
    resources: Resources<'_>,
    faces: &[WebFont],
    previous: &HashMap<Url, Fetched>,
) -> HashMap<Url, Fetched> {
    let mut fetched = HashMap::new();
    for face in faces {
        for source in &face.sources {
            let FaceSource::Url(url) = source else {
                continue;
            };
            if let Some(entry) = fetched.get(url) {
                if Fetched::usable(entry) {
                    break;
                }
                continue;
            }
            let entry = match previous.get(url) {
                Some(entry) => entry.clone(),
                None => match resources.get(url) {
                    Some(resource) => Fetched::new(&resource.bytes),
                    None => continue,
                },
            };
            let usable = entry.usable();
            fetched.insert(url.clone(), entry);
            if usable {
                break;
            }
        }
    }
    fetched
}

#[derive(Clone, Copy, PartialEq)]
enum Load {
    Unloaded,
    /// Registered from the font with this blob ID.
    Loaded(u64),
    Failed,
}

/// Load state for the rule faces of a `FaceSet`, in the same order.
#[derive(Default)]
pub(crate) struct WebFaces {
    loads: Vec<Load>,
    installed: HashMap<String, Option<fontique::Blob<u8>>>,
    /// Fonts with restricted cmaps, by source font and coverage. Reusing
    /// them across rule set changes keeps font instances, and the glyphs
    /// rasterized for them, alive.
    restricted: HashMap<(u64, Coverage), fontique::Blob<u8>>,
}

impl WebFaces {
    /// Starts over for a new rule set, keeping the restricted fonts whose
    /// sources it still uses.
    pub(crate) fn sync(&mut self, set: &FaceSet) {
        self.loads = vec![Load::Unloaded; set.rules.len()];
        let sources: std::collections::HashSet<u64> = set
            .fetched
            .values()
            .filter_map(Fetched::decoded_id)
            .chain(self.installed.values().flatten().map(fontique::Blob::id))
            .collect();
        self.restricted
            .retain(|(source, _), _| sources.contains(source));
    }

    /// Registers the face from its first source that decodes, once.
    pub(crate) fn load(&mut self, index: usize, set: &FaceSet, context: &mut FontContext) -> bool {
        match self.loads[index] {
            Load::Loaded(_) => return true,
            Load::Failed => return false,
            Load::Unloaded => {}
        }
        let face = &set.rules[index];
        let family = alias(index);
        let coverage = &set.rule_coverage[index];
        let loaded = face.sources.iter().find_map(|source| {
            let blob = match source {
                FaceSource::Url(url) => set.fetched.get(url).and_then(Fetched::blob),
                FaceSource::Local(name) => self
                    .installed
                    .entry(name.to_lowercase())
                    .or_insert_with(|| installed(context, name))
                    .clone(),
            }?;
            let blob = self
                .restricted
                .entry((blob.id(), coverage.clone()))
                .or_insert_with(|| restricted(blob, coverage))
                .clone();
            register(context, blob.clone(), &family, &face.attributes).then(|| blob.id())
        });
        self.loads[index] = loaded.map_or(Load::Failed, Load::Loaded);
        loaded.is_some()
    }

    /// The ID of the font a loaded face registered.
    pub(crate) fn registered(&self, index: usize) -> Option<u64> {
        match self.loads.get(index)? {
            Load::Loaded(id) => Some(*id),
            _ => None,
        }
    }
}

/// An installed font whose full or PostScript name is `name`, as `local()`
/// matches it. Only families whose name begins the requested one are read,
/// which holds for the names fonts give themselves. Extract a selected
/// collection face before aliasing it so the alias covers only that face.
pub(crate) fn installed(context: &mut FontContext, name: &str) -> Option<fontique::Blob<u8>> {
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
            let (file, collection_face) = match FileRef::new(blob.data()) {
                Ok(FileRef::Font(file)) => (file, false),
                Ok(FileRef::Collection(collection)) => {
                    let Ok(file) = collection.get(font.index()) else {
                        continue;
                    };
                    (file, true)
                }
                Err(_) => continue,
            };
            let matches = [StringId::FULL_NAME, StringId::POSTSCRIPT_NAME]
                .into_iter()
                .flat_map(|id| file.localized_strings(id))
                .any(|string| string.chars().collect::<String>().to_lowercase() == wanted);
            if matches {
                if collection_face {
                    return repack_font(&file, None)
                        .map(|data| fontique::Blob::new(Arc::new(data)));
                }
                return Some(blob);
            }
        }
    }
    None
}
