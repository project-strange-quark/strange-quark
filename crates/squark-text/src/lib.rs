// SPDX-License-Identifier: MIT
//! Font discovery, packed web fonts and the Parley shaping context.

mod css;
mod document_fonts;
mod editing;
mod measure;
mod outline;
mod raster;
pub use css::{
    base_direction, style, text_align, text_align_last, vertical_align, white_space_collapse,
};
pub use document_fonts::{DocumentFont, FaceAttributes, decode_font};
pub use editing::{EditText, TextPositions};
pub use measure::{CanvasAlign, CanvasBaseline, CanvasRun, CanvasText, ControlMetrics};
pub use raster::GlyphRasterizer;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parley::{FontContext, FontData, LayoutContext, fontique};
use squark_assets::Resources;
use squark_types::{NodeId, render::FontInstanceId};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextBrush(pub NodeId);

impl Default for TextBrush {
    fn default() -> Self {
        Self(NodeId::NONE)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebFont {
    pub family: String,
    pub urls: Vec<Url>,
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
    document_fonts: Vec<DocumentFont>,
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
            document_fonts: Vec::new(),
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
    /// Refresh packed font registrations when the effective @font-face rules
    /// change, including when they cross a media-query boundary.
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
        for face in &faces {
            for url in &face.urls {
                let Some(data) = resources.get(url) else {
                    continue;
                };
                let Some(bytes) = decode_font(&data.bytes) else {
                    continue;
                };
                let overrides = fontique::FontInfoOverride {
                    family_name: Some(&face.family),
                    ..Default::default()
                };
                let registered = self
                    .context
                    .collection
                    .register_fonts(fontique::Blob::new(Arc::new(bytes)), Some(overrides));
                if !registered.is_empty() {
                    break;
                }
            }
        }
        for face in document_fonts {
            face.register(&mut self.context);
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

    use super::*;

    #[test]
    fn registers_effective_woff2_faces_with_the_css_family_name() {
        let woff2 = include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(woff2);
        let font_url = Url::parse(&format!("data:font/woff2;base64,{encoded}")).unwrap();
        let doc_url = Url::parse("https://app.test/index.html").unwrap();
        let resources = Resources {
            pack: None,
            blob_resolver: None,
            document_url: &doc_url,
        };
        let mut fonts = Fonts::default();
        let face = WebFont {
            family: "Squark WOFF2 fixture".into(),
            urls: vec![font_url],
        };
        fonts.sync_web_fonts(resources, vec![face.clone()], &[]);
        assert!(
            fonts
                .context
                .collection
                .family_by_name(&face.family)
                .is_some()
        );
        fonts.sync_web_fonts(resources, Vec::new(), &[]);
        assert!(
            fonts
                .context
                .collection
                .family_by_name(&face.family)
                .is_none()
        );
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
        fontique::SourceKind::Path(Arc::from(file.as_path())),
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
