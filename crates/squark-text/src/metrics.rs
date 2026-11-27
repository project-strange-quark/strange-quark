// SPDX-License-Identifier: MIT
//! The font metrics that font-relative CSS units (`ex`, `ch`, `cap`, `ic`
//! and their root forms) resolve against: those of the primary font, the
//! first available font of the computed `font-family` list, as CSS font
//! matching picks it from the web fonts in effect and the installed fonts.
//! Stylo asks during the cascade; answers are cached and checked again when
//! the web fonts change, so styles that resolved against a fallback font
//! can be recomputed once the web font loads, as in browsers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use parley::{FontContext, FontFamilyName, fontique};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, NormalizedCoord, Size},
    outline::{DrawSettings, OutlinePen},
    raw::types::Tag,
};
use style::device::servo::FontMetricsProvider;
use style::font_metrics::FontMetrics;
use style::properties::style_structs::Font;
use style::values::computed::font::{FontStyle, GenericFontFamily, QueryFontMetricsFlags};
use style::values::computed::{CSSPixelLength, Length};

use crate::document_fonts::{Face, FaceSet, FontRequest, covers};
use crate::web_fonts::FaceSource;

/// Answers Stylo's font metrics queries. It is shared between the style
/// device and the `Fonts` that keep its web fonts current.
#[derive(Default)]
pub struct FontMetricsSource {
    state: Mutex<State>,
}

impl std::fmt::Debug for FontMetricsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontMetricsSource").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    set: Arc<FaceSet>,
    /// The web fonts changed since the cached answers were checked.
    stale: bool,
    /// Installed fonts. A default provider reads them on the first query
    /// that reaches them.
    context: Option<FontContext>,
    local: HashMap<String, Option<fontique::Blob<u8>>>,
    documents: HashMap<u32, (Arc<[u8]>, fontique::Blob<u8>)>,
    faces: HashMap<FaceKey, Unscaled>,
    answers: HashMap<Query, FontMetrics>,
}

/// What Stylo asks about: the computed font, reduced to what picks the
/// primary font and its instance, at a size.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Query {
    families: Vec<Family>,
    /// The weight and the `font-stretch` percentage, as bits.
    weight: u32,
    style: FontStyle,
    width: u32,
    /// `font-variation-settings` with an automatic `opsz`, as tags and bits.
    variations: Vec<([u8; 4], u32)>,
    size: u32,
    vertical: bool,
    web_fonts: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Family {
    Named(String),
    Generic(fontique::GenericFamily),
}

impl Query {
    fn new(font: &Font, size: f32, vertical: bool, web_fonts: bool) -> Self {
        Self {
            families: crate::css::font_families(font)
                .into_iter()
                .map(|family| match family {
                    FontFamilyName::Named(name) => Family::Named(name.into_owned()),
                    FontFamilyName::Generic(generic) => Family::Generic(generic),
                })
                .collect(),
            weight: font.font_weight.value().to_bits(),
            style: font.font_style,
            width: font.font_width.0.to_float().to_bits(),
            variations: crate::css::font_variations(font, size)
                .into_iter()
                .map(|variation| (variation.tag.to_bytes(), variation.value.to_bits()))
                .collect(),
            size: size.to_bits(),
            vertical,
            web_fonts,
        }
    }

    fn request(&self) -> FontRequest {
        FontRequest {
            weight: f32::from_bits(self.weight),
            style: self.style,
            width: f32::from_bits(self.width),
        }
    }
}

/// A font instance: a font, its index in its file and its variation
/// coordinates.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FaceKey {
    blob: u64,
    index: u32,
    coords: Vec<i16>,
}

struct Primary {
    blob: fontique::Blob<u8>,
    index: u32,
    coords: Vec<NormalizedCoord>,
}

/// A font's metrics in ems.
#[derive(Clone, Debug, Default, PartialEq)]
struct Unscaled {
    x_height: Option<f32>,
    cap_height: Option<f32>,
    zero_advance: Option<f32>,
    ic_width: Option<f32>,
    ascent: f32,
}

impl FontMetricsSource {
    /// A provider that finds installed fonts through `context`, which can
    /// share another collection's system font list as a clone of it.
    pub(crate) fn new(context: FontContext) -> Self {
        Self {
            state: Mutex::new(State {
                context: Some(context),
                ..State::default()
            }),
        }
    }

    /// Follows a new set of web fonts. Answers given so far are checked
    /// again by the next `revalidate`.
    pub(crate) fn set_faces(&self, set: Arc<FaceSet>) {
        let mut state = self.lock();
        state.set = set;
        state.stale = true;
    }

    /// Whether the web fonts changed the answer to a query made since the
    /// last check, so styles that used font-relative units need computing
    /// again.
    pub fn revalidate(&self) -> bool {
        let mut state = self.lock();
        if !std::mem::take(&mut state.stale) {
            return false;
        }
        let queries: Vec<Query> = state.answers.keys().cloned().collect();
        let mut changed = false;
        for query in queries {
            let answer = state.compute(&query);
            if state.answers.get(&query) != Some(&answer) {
                changed = true;
                state.answers.insert(query, answer);
            }
        }
        changed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl FontMetricsProvider for FontMetricsSource {
    fn query_font_metrics(
        &self,
        vertical: bool,
        font: &Font,
        base_size: CSSPixelLength,
        flags: QueryFontMetricsFlags,
    ) -> FontMetrics {
        let query = Query::new(
            font,
            base_size.px(),
            vertical,
            flags.contains(QueryFontMetricsFlags::USE_USER_FONT_SET),
        );
        let mut state = self.lock();
        if let Some(answer) = state.answers.get(&query) {
            return answer.clone();
        }
        let answer = state.compute(&query);
        state.answers.insert(query, answer.clone());
        answer
    }

    fn base_size_for_generic(&self, family: GenericFontFamily) -> Length {
        Length::new(if family == GenericFontFamily::Monospace {
            13.0
        } else {
            16.0
        })
    }
}

impl State {
    fn compute(&mut self, query: &Query) -> FontMetrics {
        let size = f32::from_bits(query.size);
        let Some(primary) = self.primary(query) else {
            return FontMetrics::default();
        };
        let key = FaceKey {
            blob: primary.blob.id(),
            index: primary.index,
            coords: primary.coords.iter().map(|coord| coord.to_bits()).collect(),
        };
        let unscaled = self
            .faces
            .entry(key)
            .or_insert_with(|| Unscaled::read(&primary));
        let scaled = |value: f32| Length::new(value * size);
        FontMetrics {
            x_height: unscaled.x_height.map(scaled),
            // Vertical metrics are not read. Upright text falls back to
            // 1em, the advance of most upright glyphs.
            zero_advance_measure: unscaled
                .zero_advance
                .filter(|_| !query.vertical)
                .map(scaled),
            cap_height: unscaled.cap_height.map(scaled),
            ic_width: unscaled.ic_width.filter(|_| !query.vertical).map(scaled),
            ascent: scaled(unscaled.ascent),
            script_percent_scale_down: None,
            script_script_percent_scale_down: None,
        }
    }

    /// The first available font, as text layout picks it: for a family
    /// with web fonts, the face matching picks that covers a space and
    /// loads; otherwise the installed family.
    fn primary(&mut self, query: &Query) -> Option<Primary> {
        let request = query.request();
        let attributes = fontique::Attributes::new(
            fontique::FontWidth::from_percentage(request.width),
            crate::css::font_style(request.style),
            fontique::FontWeight::new(request.weight),
        );
        let set = self.set.clone();
        for family in &query.families {
            let found = match family {
                Family::Named(name) => {
                    let (faces, installed) = if query.web_fonts {
                        set.match_family(name, request)
                    } else {
                        (Vec::new(), true)
                    };
                    let web = faces
                        .iter()
                        .filter(|&&face| covers(set.coverage(face), u32::from(' ')))
                        .find_map(|&face| self.web_font(&set, face));
                    match web {
                        Some(blob) => {
                            let coords = coordinates(&blob, 0, synthesis(&blob, request), query);
                            Some(Primary {
                                blob,
                                index: 0,
                                coords,
                            })
                        }
                        None if installed => {
                            self.installed(fontique::QueryFamily::Named(name), attributes, query)
                        }
                        None => None,
                    }
                }
                Family::Generic(generic) => {
                    self.installed(fontique::QueryFamily::Generic(*generic), attributes, query)
                }
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// The font of a web font face, from its first source that decodes.
    fn web_font(&mut self, set: &FaceSet, face: Face) -> Option<fontique::Blob<u8>> {
        match face {
            Face::Rule(index) => set.rules[index]
                .sources
                .iter()
                .find_map(|source| match source {
                    FaceSource::Url(url) => set.fetched.get(url)?.blob(),
                    FaceSource::Local(name) => self.local(name),
                }),
            Face::Document(index) => {
                let document = &set.documents[index];
                let (bytes, blob) = self.documents.entry(document.id).or_insert_with(|| {
                    let bytes = document.bytes.clone();
                    (bytes.clone(), fontique::Blob::new(Arc::new(bytes)))
                });
                if !Arc::ptr_eq(bytes, &document.bytes) {
                    *bytes = document.bytes.clone();
                    *blob = fontique::Blob::new(Arc::new(document.bytes.clone()));
                }
                Some(blob.clone())
            }
        }
    }

    fn context(&mut self) -> &mut FontContext {
        self.context.get_or_insert_with(crate::font_context)
    }

    fn local(&mut self, name: &str) -> Option<fontique::Blob<u8>> {
        let key = name.to_lowercase();
        if let Some(blob) = self.local.get(&key) {
            return blob.clone();
        }
        let blob = crate::web_fonts::installed(self.context(), name);
        self.local.insert(key, blob.clone());
        blob
    }

    fn installed(
        &mut self,
        family: fontique::QueryFamily<'_>,
        attributes: fontique::Attributes,
        query: &Query,
    ) -> Option<Primary> {
        let context = self.context();
        let mut fonts = context.collection.query(&mut context.source_cache);
        fonts.set_families([family]);
        fonts.set_attributes(attributes);
        let font = fonts.first_available_font()?;
        let synthesis = font
            .synthesis
            .variation_settings()
            .iter()
            .map(|&(tag, value)| (Tag::from_be_bytes(tag.to_be_bytes()), value))
            .collect();
        let coords = coordinates(&font.blob, font.index, synthesis, query);
        Some(Primary {
            blob: font.blob,
            index: font.index,
            coords,
        })
    }
}

/// The variation settings fontique gives a face for a request: the
/// requested width and weight, and the italic or slant axis for an italic
/// or oblique style.
fn synthesis(blob: &fontique::Blob<u8>, request: FontRequest) -> Vec<(Tag, f32)> {
    let Ok(font) = FontRef::from_index(blob.data(), 0) else {
        return Vec::new();
    };
    let axes = font.axes();
    let has = |tag: &[u8; 4]| axes.iter().any(|axis| axis.tag() == Tag::new(tag));
    let mut settings = Vec::new();
    if has(b"wdth") {
        settings.push((Tag::new(b"wdth"), request.width));
    }
    if has(b"wght") {
        settings.push((Tag::new(b"wght"), request.weight));
    }
    if request.style == FontStyle::ITALIC {
        if has(b"ital") {
            settings.push((Tag::new(b"ital"), 1.0));
        } else if has(b"slnt") {
            settings.push((Tag::new(b"slnt"), -14.0));
        }
    } else if request.style != FontStyle::NORMAL {
        let degrees = request.style.oblique_degrees();
        if has(b"slnt") {
            settings.push((Tag::new(b"slnt"), -degrees));
        } else if has(b"ital") && degrees > 0.0 {
            settings.push((Tag::new(b"ital"), 1.0));
        }
    }
    settings
}

/// Normalized coordinates for the synthesized settings followed by the
/// style's own, which win, as text shaping applies them.
fn coordinates(
    blob: &fontique::Blob<u8>,
    index: u32,
    synthesis: Vec<(Tag, f32)>,
    query: &Query,
) -> Vec<NormalizedCoord> {
    let Ok(font) = FontRef::from_index(blob.data(), index) else {
        return Vec::new();
    };
    let axes = font.axes();
    if axes.is_empty() {
        return Vec::new();
    }
    let settings = synthesis.into_iter().chain(
        query
            .variations
            .iter()
            .map(|&(tag, value)| (Tag::new(&tag), f32::from_bits(value))),
    );
    axes.location(settings).coords().to_vec()
}

impl Unscaled {
    /// The metrics Chromium reads through Skia: OS/2 `sxHeight` and
    /// `sCapHeight` when nonzero, else the top of the "x" and "H" glyphs'
    /// control boxes, else the ascent; the advances of "0" and of 水 U+6C34
    /// for `ch` and `ic`.
    fn read(primary: &Primary) -> Self {
        let Ok(font) = FontRef::from_index(primary.blob.data(), primary.index) else {
            return Self::default();
        };
        let location = LocationRef::new(&primary.coords);
        let em = Size::new(1.0);
        let metrics = font.metrics(em, location);
        let glyphs = font.glyph_metrics(em, location);
        let charmap = font.charmap();
        let advance = |ch| glyphs.advance_width(charmap.map(ch)?);
        let top = |ch| {
            let glyph = font.outline_glyphs().get(charmap.map(ch)?)?;
            let mut pen = Top(None);
            glyph
                .draw(DrawSettings::unhinted(em, location), &mut pen)
                .ok()?;
            pen.0
        };
        // Chromium measures a font with neither an x-height nor an "x",
        // Noto Naskh Arabic for one, at its ascent; Stylo's cap-height
        // fallback is the ascent too.
        let x_height = metrics
            .x_height
            .filter(|height| *height != 0.0)
            .or_else(|| top('x'))
            .unwrap_or(metrics.ascent);
        Self {
            x_height: Some(x_height),
            cap_height: metrics
                .cap_height
                .filter(|height| *height != 0.0)
                .or_else(|| top('H')),
            zero_advance: advance('0'),
            ic_width: advance('\u{6C34}'),
            ascent: metrics.ascent,
        }
    }
}

/// The highest point of an outline, control points included.
struct Top(Option<f32>);

impl Top {
    fn point(&mut self, y: f32) {
        self.0 = Some(self.0.map_or(y, |top| top.max(y)));
    }
}

impl OutlinePen for Top {
    fn move_to(&mut self, _: f32, y: f32) {
        self.point(y);
    }

    fn line_to(&mut self, _: f32, y: f32) {
        self.point(y);
    }

    fn quad_to(&mut self, _: f32, y0: f32, _: f32, y: f32) {
        self.point(y0);
        self.point(y);
    }

    fn curve_to(&mut self, _: f32, y0: f32, _: f32, y1: f32, _: f32, y: f32) {
        self.point(y0);
        self.point(y1);
        self.point(y);
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine as _;
    use style::values::computed::font::{
        FamilyName, FontFamily, FontFamilyList, FontFamilyNameSyntax, SingleFontFamily,
    };

    use crate::{FaceAttributes, Fonts, WebFont};

    const SANS: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
    );
    const MONO: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
    );

    fn read(bytes: &[u8]) -> Unscaled {
        let bytes = crate::decode_font(bytes).unwrap();
        Unscaled::read(&Primary {
            blob: fontique::Blob::new(Arc::new(bytes)),
            index: 0,
            coords: Vec::new(),
        })
    }

    /// Each measure at 13px, as Chromium resolves `10ex` and the like
    /// against these fonts loaded as web fonts.
    #[test]
    fn reads_the_metrics_chromium_reads() {
        let assistant = include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/Assistant-Regular.woff2"
        );
        let naskh = include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/NotoNaskhArabic-Regular.woff2"
        );
        let variable =
            include_bytes!("../../../tests/reftests/cases/r1/_assets/fonts/SquarkVar.ttf");
        let at = |value: Option<f32>| value.map(|value| value * 13.0);
        let close = |value: Option<f32>, chromium: Option<f32>| match (at(value), chromium) {
            (Some(value), Some(chromium)) => (value - chromium).abs() < 0.01,
            (value, chromium) => value == chromium,
        };
        // [ch, ex, cap]; Chromium uses 0.5em for a missing "0".
        for (bytes, [ch, ex, cap]) in [
            (SANS, [Some(7.2297), Some(6.8672), Some(8.9438)]),
            (MONO, [Some(7.8), Some(6.8672), Some(8.5625)]),
            (&assistant[..], [Some(6.3953), Some(6.5), Some(9.75)]),
            // No x-height, cap height, "x" or "H": both are the ascent.
            (&naskh[..], [Some(7.2656), Some(13.897), None]),
            (&variable[..], [None, Some(6.5), Some(9.1)]),
        ] {
            let metrics = read(bytes);
            assert!(close(metrics.zero_advance, ch), "{metrics:?}");
            assert!(close(metrics.x_height, ex), "{metrics:?}");
            assert!(close(metrics.cap_height, cap), "{metrics:?}");
            assert_eq!(metrics.ic_width, None);
        }
        assert!((read(&naskh[..]).ascent * 13.0 - 13.897).abs() < 0.01);
    }

    fn font(families: &[&str]) -> Font {
        let mut font = Font::initial_values();
        font.font_family = FontFamily {
            families: FontFamilyList {
                list: style::ArcSlice::from_iter(families.iter().map(|name| {
                    SingleFontFamily::FamilyName(FamilyName {
                        name: (*name).into(),
                        syntax: FontFamilyNameSyntax::Quoted,
                    })
                })),
            },
            is_system_font: false,
            is_initial: false,
        };
        font
    }

    fn data_url(bytes: &[u8]) -> url::Url {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        url::Url::parse(&format!("data:font/woff2;base64,{encoded}")).unwrap()
    }

    #[test]
    fn answers_from_the_primary_web_font_once_it_is_in_effect() {
        let document_url = url::Url::parse("https://app.test/").unwrap();
        let resources = squark_assets::Resources {
            pack: None,
            blob_resolver: None,
            document_url: &document_url,
        };
        let mut fonts = Fonts::default();
        let source = fonts.metrics_source();
        let ch = |font: &Font| {
            source
                .query_font_metrics(
                    false,
                    font,
                    CSSPixelLength::new(20.0),
                    QueryFontMetricsFlags::USE_USER_FONT_SET,
                )
                .zero_advance_measure
                .map(|length| length.px())
        };
        let web = font(&["Web Face", "Strange Quark Missing Font"]);
        let before = ch(&web);
        assert!(!source.revalidate());

        // A face whose range leaves out the space is not the primary font;
        // the one covering it is, though defined first.
        let face = |bytes, ranges| WebFont {
            family: "Web Face".into(),
            attributes: FaceAttributes {
                ranges,
                ..Default::default()
            },
            sources: vec![crate::FaceSource::Url(data_url(bytes))],
        };
        fonts.sync_web_fonts(
            resources,
            vec![
                face(MONO, Vec::new()),
                face(SANS, vec![std::ops::RangeInclusive::new(0x30, 0x39)]),
            ],
            &[],
        );
        assert!(source.revalidate());
        assert!(!source.revalidate());
        let after = ch(&web);
        assert_ne!(after, before);
        assert!((after.unwrap() - 12.0).abs() < 0.01, "{after:?}");
        // Media queries do not use web fonts.
        let media = source.query_font_metrics(
            false,
            &web,
            CSSPixelLength::new(20.0),
            QueryFontMetricsFlags::empty(),
        );
        assert_eq!(media.zero_advance_measure.map(|length| length.px()), before);
        // Upright vertical text falls back to 1em.
        let vertical = source.query_font_metrics(
            true,
            &web,
            CSSPixelLength::new(20.0),
            QueryFontMetricsFlags::USE_USER_FONT_SET,
        );
        assert_eq!(vertical.zero_advance_measure, None);
        assert!(vertical.x_height.is_some());
    }
}
