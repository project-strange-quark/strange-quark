// SPDX-License-Identifier: MIT
//! Font discovery, packed web fonts and the Parley shaping context.

mod css;
mod document_fonts;
mod editing;
mod measure;
mod outline;
mod raster;
mod web_fonts;
pub use css::{
    base_direction, style, text_align, text_align_last, vertical_align, white_space_collapse,
};
pub use document_fonts::{DocumentFont, FaceAttributes, FontRequest, decode_font, matching_faces};
pub use editing::{EditText, TextPositions};
pub use measure::{CanvasAlign, CanvasBaseline, CanvasRun, CanvasText, ControlMetrics};
pub use raster::GlyphRasterizer;
pub use web_fonts::{FaceSource, WebFont};

use std::collections::HashMap;
use std::sync::OnceLock;

use parley::{FontContext, FontData, LayoutContext, fontique};
use squark_assets::Resources;
use squark_types::{NodeId, render::FontInstanceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextBrush(pub NodeId);

impl Default for TextBrush {
    fn default() -> Self {
        Self(NodeId::NONE)
    }
}

#[derive(Clone, Debug)]
pub struct FontInstance {
    pub font: FontData,
    pub size: f32,
    pub coords: Vec<i16>,
}

#[derive(Eq, Hash, PartialEq)]
struct InstanceKey {
    blob: u64,
    index: u32,
    size: u32,
    coords: Vec<i16>,
}

pub struct Fonts {
    pub context: FontContext,
    pub layout: LayoutContext<TextBrush>,
    faces: Vec<WebFont>,
    web: web_fonts::WebFaces,
    document_fonts: Vec<DocumentFont>,
    document_coverage: Vec<document_fonts::Coverage>,
    families: HashMap<String, Vec<document_fonts::Face>>,
    keys: HashMap<InstanceKey, FontInstanceId>,
    instances: Vec<FontInstance>,
    measured_sources: HashMap<u64, fontique::Blob<u8>>,
    canvas_ink: outline::CanvasInkBounds,
}

impl Default for Fonts {
    fn default() -> Self {
        static SOURCES: OnceLock<fontique::SourceCache> = OnceLock::new();
        let mut context = FontContext::new();
        // Parley prunes its local cache during shaping. Fontique can still
        // recover the original mapping while a font instance holds its blob.
        context.source_cache = SOURCES
            .get_or_init(fontique::SourceCache::new_shared)
            .clone();
        Self {
            context,
            layout: LayoutContext::new(),
            faces: Vec::new(),
            web: Default::default(),
            document_fonts: Vec::new(),
            document_coverage: Vec::new(),
            families: HashMap::new(),
            keys: HashMap::new(),
            instances: Vec::new(),
            measured_sources: HashMap::new(),
            canvas_ink: outline::CanvasInkBounds::default(),
        }
    }
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fonts")
            .field("faces", &self.faces)
            .field("instances", &self.instances.len())
            .finish_non_exhaustive()
    }
}

impl Fonts {
    /// Refresh web font registrations when the effective @font-face rules
    /// or the loaded `document.fonts` faces change, including when rules
    /// cross a media-query boundary. Rule faces register when text first
    /// matches them.
    pub fn sync_web_fonts(
        &mut self,
        resources: Resources<'_>,
        faces: Vec<WebFont>,
        document_fonts: &[DocumentFont],
    ) -> bool {
        if self.faces == faces && self.document_fonts == document_fonts {
            return false;
        }
        // Rebuild registrations when active faces change. Keep the source
        // cache and its backing mappings across the collection replacement.
        self.context.collection = FontContext::new().collection;
        let mut coverage = document_fonts::exclusive_coverage(
            &faces
                .iter()
                .map(|face| (face.family.as_str(), &face.attributes))
                .chain(
                    document_fonts
                        .iter()
                        .map(|face| (face.family.as_str(), &face.attributes)),
                )
                .collect::<Vec<_>>(),
        );
        self.document_coverage = coverage.split_off(faces.len());
        self.families = document_fonts::family_index(&faces, document_fonts);
        self.web.sync(resources, &faces, coverage);
        for (face, coverage) in document_fonts.iter().zip(&self.document_coverage) {
            face.register(&mut self.context, coverage);
        }
        self.document_fonts = document_fonts.to_vec();
        self.faces = faces;
        true
    }

    pub fn intern(&mut self, font: &FontData, size: f32, coords: &[i16]) -> FontInstanceId {
        let key = InstanceKey {
            blob: font.data.id(),
            index: font.index,
            size: size.to_bits(),
            coords: coords.to_vec(),
        };
        *self.keys.entry(key).or_insert_with(|| {
            self.instances.push(FontInstance {
                font: font.clone(),
                size,
                coords: coords.to_vec(),
            });
            FontInstanceId(self.instances.len() as u64)
        })
    }

    pub fn instance(&self, id: FontInstanceId) -> Option<&FontInstance> {
        self.instances
            .get(usize::try_from(id.0.checked_sub(1)?).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use parley::{FontFamily, FontFamilyName, GenericFamily, TextStyle};
    use skrifa::{MetadataProvider, string::StringId};
    use std::borrow::Cow;
    use url::Url;

    use super::*;

    fn data_url(bytes: &[u8]) -> Url {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Url::parse(&format!("data:font/woff2;base64,{encoded}")).unwrap()
    }

    fn families(style: &TextStyle<'_, '_, TextBrush>) -> Vec<String> {
        match &style.font_family {
            FontFamily::List(names) => names
                .iter()
                .map(|name| match name {
                    FontFamilyName::Named(name) => name.to_string(),
                    FontFamilyName::Generic(generic) => format!("{generic:?}"),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The registered font's PostScript name, weight, and whether its cmap
    /// maps each of `chars`.
    fn registered(
        fonts: &mut Fonts,
        family: &str,
        chars: &str,
    ) -> Option<(String, f32, Vec<bool>)> {
        let info = fonts.context.collection.family_by_name(family)?;
        let font = info.fonts().first()?;
        let blob = font.load(Some(&mut fonts.context.source_cache))?;
        let file = skrifa::FontRef::from_index(blob.data(), font.index()).ok()?;
        let name = file
            .localized_strings(StringId::POSTSCRIPT_NAME)
            .english_or_first()?
            .chars()
            .collect();
        let charmap = file.charmap();
        let mapped = chars.chars().map(|ch| charmap.map(ch).is_some()).collect();
        Some((name, font.weight().value(), mapped))
    }

    #[test]
    fn rule_faces_register_under_their_own_alias_when_text_matches_them() {
        let doc_url = Url::parse("https://app.test/index.html").unwrap();
        let resources = Resources {
            pack: None,
            blob_resolver: None,
            document_url: &doc_url,
        };
        let regular = data_url(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
        ));
        let bold = data_url(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Bold.woff2"
        ));
        let mono = data_url(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
        ));
        // The descriptors, not the files, decide the weights: the regular
        // file is the bold face and the bold file the regular one.
        let faces = vec![
            WebFont {
                family: "Fixture".into(),
                attributes: FaceAttributes {
                    weight: [700.0; 2],
                    ..Default::default()
                },
                sources: vec![FaceSource::Url(regular)],
            },
            WebFont {
                family: "Fixture".into(),
                attributes: FaceAttributes::default(),
                sources: vec![
                    FaceSource::Local("Strange Quark Missing Font".into()),
                    FaceSource::Url(bold),
                ],
            },
            WebFont {
                family: "fixture".into(),
                attributes: FaceAttributes {
                    ranges: vec![std::ops::RangeInclusive::new(0x400, 0x4FF)],
                    ..Default::default()
                },
                sources: vec![FaceSource::Url(mono)],
            },
        ];
        let mut fonts = Fonts::default();
        assert!(fonts.sync_web_fonts(resources, faces.clone(), &[]));
        assert!(!fonts.sync_web_fonts(resources, faces.clone(), &[]));
        let alias = web_fonts::alias;
        for index in 0..3 {
            assert!(
                fonts
                    .context
                    .collection
                    .family_by_name(&alias(index))
                    .is_none()
            );
        }
        let style = |weight| TextStyle {
            font_family: FontFamily::List(Cow::Owned(vec![
                FontFamilyName::Named(Cow::Borrowed("Fixture")),
                FontFamilyName::Generic(GenericFamily::SansSerif),
            ])),
            font_weight: parley::FontWeight::new(weight),
            ..TextStyle::default()
        };

        // Only the faces whose coverage meets the text load, and the face
        // covering a space comes first. The family name itself no longer
        // reaches installed fonts.
        let latin = fonts.with_web_fonts("A", style(400.0));
        assert_eq!(families(&latin), [alias(1), "SansSerif".into()]);
        assert!(fonts.context.collection.family_by_name(&alias(2)).is_none());
        let mixed = fonts.with_web_fonts("AЖ", style(400.0));
        assert_eq!(families(&mixed), [alias(1), alias(2), "SansSerif".into()]);
        // Each face's cmap keeps only the characters it renders: the
        // Cyrillic face wins Cyrillic, which the bold file also maps.
        assert_eq!(
            registered(&mut fonts, &alias(1), "AЖ"),
            Some(("LiberationSans-Bold".into(), 400.0, vec![true, false]))
        );
        assert_eq!(
            registered(&mut fonts, &alias(2), "AЖ"),
            Some(("LiberationMono".into(), 400.0, vec![false, true]))
        );
        assert!(fonts.context.collection.family_by_name(&alias(0)).is_none());

        let bold = fonts.with_web_fonts("AЖ", style(700.0));
        assert_eq!(families(&bold), [alias(0), "SansSerif".into()]);
        assert_eq!(
            registered(&mut fonts, &alias(0), "AЖ"),
            Some(("LiberationSans".into(), 700.0, vec![true, true]))
        );

        // A family without rules keeps resolving to installed fonts.
        let mut other = style(400.0);
        other.font_family = FontFamily::List(Cow::Owned(vec![FontFamilyName::Named(
            Cow::Borrowed("Other"),
        )]));
        assert_eq!(families(&fonts.with_web_fonts("A", other)), ["Other"]);

        // A changed rule set registers the faces again from the same
        // restricted fonts, so their font instances carry over.
        let blob_id = |fonts: &mut Fonts, family: &str| {
            let info = fonts.context.collection.family_by_name(family)?;
            let font = info.fonts().first()?;
            Some(font.load(Some(&mut fonts.context.source_cache))?.id())
        };
        let restricted = blob_id(&mut fonts, &alias(1)).unwrap();
        let mut more = faces.clone();
        more.push(WebFont {
            family: "Other".into(),
            ..faces[0].clone()
        });
        assert!(fonts.sync_web_fonts(resources, more, &[]));
        assert!(fonts.context.collection.family_by_name(&alias(1)).is_none());
        fonts.with_web_fonts("AЖ", style(400.0));
        assert_eq!(blob_id(&mut fonts, &alias(1)), Some(restricted));

        fonts.sync_web_fonts(resources, Vec::new(), &[]);
        assert!(fonts.context.collection.family_by_name(&alias(0)).is_none());
    }
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn shares_one_font_mapping_across_relayouts_and_contexts() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/squark-text-font-mapping");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join(format!("font-{}.ttf", std::process::id()));
    std::fs::write(
        &file,
        include_bytes!("../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.ttf"),
    )
    .unwrap();
    let file = file.canonicalize().unwrap();
    let source = fontique::SourceInfo::new(
        fontique::SourceId::new(),
        fontique::SourceKind::Path(std::sync::Arc::from(file.as_path())),
    );
    let count = || {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .filter(|line| line.ends_with(file.to_str().unwrap()))
            .count()
    };

    let mut fonts = Fonts::default();
    let blob = fonts.context.source_cache.get(&source).unwrap();
    let blob_id = blob.id();
    fonts.intern(&FontData::new(blob, 0), 16.0, &[]);
    assert_eq!(count(), 1);
    let mut other = Fonts::default();
    for _ in 0..3 {
        for _ in 0..256 {
            // An unrelated layout advances Parley's source cache epoch.
            let mut builder = fonts
                .layout
                .style_run_builder(&mut fonts.context, "text", 1.0, true);
            let default = builder.push_style(parley::TextStyle::default());
            builder.push_style_run(default, ..);
            let mut line = builder.build("text");
            line.break_all_lines(None);
        }
        assert_eq!(
            fonts.context.source_cache.get(&source).unwrap().id(),
            blob_id
        );
        assert_eq!(
            other.context.source_cache.get(&source).unwrap().id(),
            blob_id
        );
        assert_eq!(count(), 1);
    }
    std::fs::remove_file(&file).unwrap();
}
