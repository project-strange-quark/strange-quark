// SPDX-License-Identifier: MIT
//! CSS font matching over web font faces: the `@font-face` rules in effect and
//! the faces script adds to `document.fonts`.

use std::{borrow::Cow, cmp::Ordering, collections::HashMap, ops::RangeInclusive, sync::Arc};

use parley::{FontFamily, FontFamilyName, TextStyle, fontique};
use style::font_face::Descriptors;
use style::values::computed::FontStyle;

use crate::{Fonts, TextBrush};

/// The descriptors CSS font matching reads from a font face.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceAttributes {
    pub weight: [f32; 2],
    pub style: [FontStyle; 2],
    /// The `font-stretch` range, as percentages.
    pub width: [f32; 2],
    pub ranges: Vec<RangeInclusive<u32>>,
}

impl Default for FaceAttributes {
    fn default() -> Self {
        Self {
            weight: [400.0; 2],
            style: [FontStyle::NORMAL; 2],
            width: [100.0; 2],
            ranges: Vec::new(),
        }
    }
}

/// The weight, style and width a run of text asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontRequest {
    pub weight: f32,
    pub style: FontStyle,
    /// `font-stretch` as a percentage.
    pub width: f32,
}

impl FontRequest {
    pub fn from_font(font: &style::properties::style_structs::Font) -> Self {
        Self {
            weight: font.font_weight.value(),
            style: font.font_style,
            width: font.font_width.0.to_float(),
        }
    }

    fn from_text_style(style: &TextStyle<'_, '_, TextBrush>) -> Self {
        Self {
            weight: style.font_weight.value(),
            style: match style.font_style {
                fontique::FontStyle::Normal => FontStyle::NORMAL,
                fontique::FontStyle::Italic => FontStyle::ITALIC,
                fontique::FontStyle::Oblique(angle) => FontStyle::oblique(angle.unwrap_or(14.0)),
            },
            width: style.font_width.percentage(),
        }
    }
}

impl FaceAttributes {
    /// The matching descriptors of an `@font-face` rule or a `FontFace`, or
    /// `None` when one does not compute.
    pub fn from_descriptors(descriptors: &Descriptors) -> Option<Self> {
        let weight = match &descriptors.font_weight {
            Some(weight) => {
                let weight = weight.compute()?;
                [weight.0.value(), weight.1.value()]
            }
            None => [400.0; 2],
        };
        let style = match &descriptors.font_style {
            Some(style) => {
                let style = style.compute()?;
                [style.0, style.1]
            }
            None => [FontStyle::NORMAL; 2],
        };
        let width = match &descriptors.font_width {
            Some(width) => {
                let width = width.compute()?;
                [width.0.0.to_float(), width.1.0.to_float()]
            }
            None => [100.0; 2],
        };
        let ranges = descriptors
            .unicode_range
            .as_ref()
            .map(|ranges| ranges.iter().map(|range| range.start..=range.end).collect())
            .unwrap_or_default();
        Some(Self {
            weight,
            style,
            width,
            ranges,
        })
    }

    pub fn covers(&self, ch: char) -> bool {
        self.ranges.is_empty()
            || self
                .ranges
                .iter()
                .any(|range| range.contains(&u32::from(ch)))
    }

    /// Whether `unicode-range` includes any character of the text.
    pub fn covers_any(&self, text: &[u16]) -> bool {
        self.ranges.is_empty()
            || char::decode_utf16(text.iter().copied())
                .map(|ch| ch.unwrap_or(char::REPLACEMENT_CHARACTER))
                .any(|ch| self.covers(ch))
    }

    fn same_descriptors(&self, other: &Self) -> bool {
        self.weight == other.weight && self.style == other.style && self.width == other.width
    }

    fn slopes(&self) -> [f32; 2] {
        self.style.map(slope)
    }
}

/// A style as the slope Blink's font selection compares: `normal` is 0,
/// obliques are their angle and `italic` is 20.
fn slope(style: FontStyle) -> f32 {
    if style == FontStyle::ITALIC {
        20.0
    } else {
        style.oblique_degrees()
    }
}

/// The extent of every face's descriptors in one family, which the distances
/// below measure faces on the far side of a request against.
#[derive(Clone, Copy)]
struct Bounds {
    weight: [f32; 2],
    slope: [f32; 2],
    width: [f32; 2],
}

impl Bounds {
    fn of<'a>(faces: impl Iterator<Item = &'a FaceAttributes>) -> Self {
        let widen = |[low, high]: [f32; 2], [a, b]: [f32; 2]| [low.min(a), high.max(b)];
        let mut bounds = Self {
            weight: [f32::INFINITY, f32::NEG_INFINITY],
            slope: [f32::INFINITY, f32::NEG_INFINITY],
            width: [f32::INFINITY, f32::NEG_INFINITY],
        };
        for face in faces {
            bounds.weight = widen(bounds.weight, face.weight);
            bounds.slope = widen(bounds.slope, face.slopes());
            bounds.width = widen(bounds.width, face.width);
        }
        bounds
    }
}

// The distances of Blink's FontSelectionAlgorithm, which implements CSS
// Fonts 4 § 5.2 step 4: matching narrows by width, then style, then weight,
// keeping the faces at the least distance at each step.

fn width_distance([low, high]: [f32; 2], request: f32, bounds: &Bounds) -> f32 {
    if (low..=high).contains(&request) {
        0.0
    } else if request > 100.0 {
        if low > request {
            low - request
        } else {
            request.max(bounds.width[1]) - high
        }
    } else if high < request {
        request - high
    } else {
        low - request.min(bounds.width[0])
    }
}

fn style_distance([low, high]: [f32; 2], request: f32, bounds: &Bounds) -> f32 {
    const ITALIC: f32 = 20.0;
    if (low..=high).contains(&request) {
        0.0
    } else if request >= ITALIC {
        if low > request {
            low - request
        } else {
            request.max(bounds.slope[1]) - high
        }
    } else if request >= 0.0 {
        if high >= 0.0 && high < request {
            request - high
        } else if low > request {
            low
        } else {
            request.max(bounds.slope[1]) - high
        }
    } else if request > -ITALIC {
        if low > request && low <= 0.0 {
            low - request
        } else if high < request {
            -high
        } else {
            low - request.min(bounds.slope[0])
        }
    } else if high < request {
        request - high
    } else {
        low - request.min(bounds.slope[0])
    }
}

fn weight_distance([low, high]: [f32; 2], request: f32, bounds: &Bounds) -> f32 {
    const LOWER: f32 = 400.0;
    const UPPER: f32 = 500.0;
    if (low..=high).contains(&request) {
        0.0
    } else if (LOWER..=UPPER).contains(&request) {
        if low > request && low <= UPPER {
            low - request
        } else if high < request {
            UPPER - high
        } else {
            low - request.min(bounds.weight[0])
        }
    } else if request < LOWER {
        if high < request {
            request - high
        } else {
            low - request.min(bounds.weight[0])
        }
    } else if low > request {
        low - request
    } else {
        request.max(bounds.weight[1]) - high
    }
}

impl FaceAttributes {
    fn distance(&self, request: FontRequest, bounds: &Bounds) -> [f32; 3] {
        [
            width_distance(self.width, request.width, bounds),
            style_distance(self.slopes(), slope(request.style), bounds),
            weight_distance(self.weight, request.weight, bounds),
        ]
    }
}

/// CSS font matching within one family. Faces with the same weight, style and
/// width form one composite face whose members split the characters between
/// them by `unicode-range`; matching picks the composite face that best fits
/// the request and returns its members, the one defined last first, as it
/// wins a character both cover. `faces` is in definition order.
pub fn matching_faces<T: Copy>(faces: &[(T, &FaceAttributes)], request: FontRequest) -> Vec<T> {
    let bounds = Bounds::of(faces.iter().map(|&(_, attributes)| attributes));
    let distance = |attributes: &FaceAttributes| attributes.distance(request, &bounds);
    let Some(&(_, best)) = faces.iter().rev().min_by(|a, b| {
        let (a, b) = (distance(a.1), distance(b.1));
        a.iter()
            .zip(&b)
            .map(|(a, b)| a.total_cmp(b))
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
    }) else {
        return Vec::new();
    };
    faces
        .iter()
        .rev()
        .filter(|(_, attributes)| attributes.same_descriptors(best))
        .map(|&(face, _)| face)
        .collect()
}

/// The codepoints a face may render, as sorted, disjoint ranges.
pub(crate) type Coverage = Vec<RangeInclusive<u32>>;

const ALL: RangeInclusive<u32> = 0..=0x10FFFF;

fn normalized(ranges: &[RangeInclusive<u32>]) -> Coverage {
    if ranges.is_empty() {
        return vec![ALL];
    }
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| *range.start());
    let mut merged: Coverage = Vec::with_capacity(sorted.len());
    for range in sorted {
        match merged.last_mut() {
            Some(last) if *range.start() <= last.end().saturating_add(1) => {
                *last = *last.start()..=(*last.end()).max(*range.end());
            }
            _ => merged.push(range),
        }
    }
    merged
}

fn subtract(coverage: Coverage, remove: &[RangeInclusive<u32>]) -> Coverage {
    let mut result = coverage;
    for cut in remove {
        result = result
            .into_iter()
            .flat_map(|range| {
                let (start, end) = (*range.start(), *range.end());
                let before = (start < *cut.start()).then(|| start..=end.min(cut.start() - 1));
                let after = (end > *cut.end()).then(|| start.max(cut.end() + 1)..=end);
                [before, after].into_iter().flatten()
            })
            .collect();
    }
    result
}

pub(crate) fn covers(coverage: &[RangeInclusive<u32>], codepoint: u32) -> bool {
    let index = coverage.partition_point(|range| *range.end() < codepoint);
    coverage
        .get(index)
        .is_some_and(|range| range.contains(&codepoint))
}

pub(crate) fn is_full(coverage: &[RangeInclusive<u32>]) -> bool {
    coverage == [ALL]
}

/// What each face renders: its `unicode-range` less the ranges of the faces
/// that win over it within its composite face, the faces of the same family
/// and descriptors defined after it. `faces` is in definition order.
pub(crate) fn exclusive_coverage(faces: &[(&str, &FaceAttributes)]) -> Vec<Coverage> {
    faces
        .iter()
        .enumerate()
        .map(|(index, (family, attributes))| {
            faces[index + 1..]
                .iter()
                .filter(|(other, later)| {
                    other.eq_ignore_ascii_case(family) && later.same_descriptors(attributes)
                })
                .fold(normalized(&attributes.ranges), |coverage, (_, later)| {
                    subtract(coverage, &normalized(&later.ranges))
                })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFont {
    pub id: u32,
    pub family: String,
    pub attributes: FaceAttributes,
    pub bytes: Arc<[u8]>,
}

impl DocumentFont {
    fn alias(&self) -> String {
        format!("StrangeQuark FontFace {}", self.id)
    }

    pub(crate) fn register(
        &self,
        context: &mut parley::FontContext,
        coverage: &[RangeInclusive<u32>],
    ) {
        let blob = fontique::Blob::new(Arc::new(self.bytes.clone()));
        crate::web_fonts::register(
            context,
            crate::web_fonts::restricted(blob, coverage),
            &self.alias(),
            &self.attributes,
        );
    }
}

/// A face CSS font matching can pick: an `@font-face` rule by its index, or
/// a loaded `document.fonts` member by its index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Face {
    Rule(usize),
    Document(usize),
}

/// The faces of each family, by its ASCII-lowercased name, rules first and
/// each in definition order, so matching a run reads only its own family.
fn family_index(
    rules: &[crate::WebFont],
    documents: &[DocumentFont],
) -> HashMap<String, Vec<Face>> {
    let mut index: HashMap<String, Vec<Face>> = HashMap::new();
    let families = rules
        .iter()
        .enumerate()
        .map(|(face, rule)| (&rule.family, Face::Rule(face)))
        .chain(
            documents
                .iter()
                .enumerate()
                .map(|(face, document)| (&document.family, Face::Document(face))),
        );
    for (family, face) in families {
        index
            .entry(family.to_ascii_lowercase())
            .or_default()
            .push(face);
    }
    index
}

/// The web font faces in effect: the `@font-face` rules with their fetched
/// sources, and the loaded `document.fonts` faces. Text layout and the font
/// metrics provider share one set and match against it alike.
#[derive(Default)]
pub(crate) struct FaceSet {
    pub(crate) rules: Vec<crate::WebFont>,
    pub(crate) rule_coverage: Vec<Coverage>,
    pub(crate) fetched: HashMap<url::Url, crate::web_fonts::Fetched>,
    pub(crate) documents: Vec<DocumentFont>,
    pub(crate) document_coverage: Vec<Coverage>,
    families: HashMap<String, Vec<Face>>,
}

impl FaceSet {
    /// Fetches the rules' sources, reusing what `previous` fetched.
    pub(crate) fn new(
        resources: squark_assets::Resources<'_>,
        rules: Vec<crate::WebFont>,
        documents: &[DocumentFont],
        previous: &FaceSet,
    ) -> Self {
        let mut rule_coverage = exclusive_coverage(
            &rules
                .iter()
                .map(|face| (face.family.as_str(), &face.attributes))
                .chain(
                    documents
                        .iter()
                        .map(|face| (face.family.as_str(), &face.attributes)),
                )
                .collect::<Vec<_>>(),
        );
        let document_coverage = rule_coverage.split_off(rules.len());
        Self {
            fetched: crate::web_fonts::fetch(resources, &rules, &previous.fetched),
            families: family_index(&rules, documents),
            rules,
            rule_coverage,
            documents: documents.to_vec(),
            document_coverage,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.documents.is_empty()
    }

    /// The faces of the composite face matching picks for the family, best
    /// first, and whether the name still reaches installed fonts: a name an
    /// `@font-face` rule defines does not, as in browsers.
    pub(crate) fn match_family(&self, family: &str, request: FontRequest) -> (Vec<Face>, bool) {
        let Some(faces) = self.families.get(&family.to_ascii_lowercase()) else {
            return (Vec::new(), true);
        };
        let candidates: Vec<(Face, &FaceAttributes)> = faces
            .iter()
            .map(|&face| (face, self.attributes(face)))
            .collect();
        let installed = !faces.iter().any(|face| matches!(face, Face::Rule(_)));
        (matching_faces(&candidates, request), installed)
    }

    pub(crate) fn attributes(&self, face: Face) -> &FaceAttributes {
        match face {
            Face::Rule(index) => &self.rules[index].attributes,
            Face::Document(index) => &self.documents[index].attributes,
        }
    }

    pub(crate) fn coverage(&self, face: Face) -> &[RangeInclusive<u32>] {
        match face {
            Face::Rule(index) => &self.rule_coverage[index],
            Face::Document(index) => &self.document_coverage[index],
        }
    }
}

impl Fonts {
    /// The line strut uses the primary font, the face that covers a space,
    /// including web font faces.
    pub fn primary_style(
        &mut self,
        style: TextStyle<'static, 'static, TextBrush>,
    ) -> TextStyle<'static, 'static, TextBrush> {
        self.with_web_fonts(" ", style)
    }

    /// Puts the web font faces that CSS font matching picks for the text
    /// ahead of the families that follow them. Each face is registered with
    /// a cmap reduced to the characters it renders, its `unicode-range` less
    /// those of faces that win over it, so font fallback within one style
    /// chooses a face per character as browsers do, and fallback glyphs do
    /// not size the line. The face covering a space comes first: it is the
    /// first available font, whose metrics size the line. Faces load the
    /// first time they match.
    pub fn with_web_fonts(
        &mut self,
        text: &str,
        style: TextStyle<'static, 'static, TextBrush>,
    ) -> TextStyle<'static, 'static, TextBrush> {
        if self.set.is_empty() {
            return style;
        }
        let FontFamily::List(families) = &style.font_family else {
            return style;
        };
        let request = FontRequest::from_text_style(&style);
        let set = self.set.clone();
        let mut names = Vec::with_capacity(families.len());
        let mut changed = false;
        for family in families.iter() {
            let FontFamilyName::Named(name) = family else {
                names.push(family.clone().into_owned());
                continue;
            };
            let (faces, installed) = set.match_family(name, request);
            changed |= !faces.is_empty() || !installed;
            let primary = faces
                .iter()
                .position(|&face| covers(set.coverage(face), u32::from(' ')));
            let used = primary.into_iter().chain(
                (0..faces.len())
                    .filter(|&index| Some(index) != primary)
                    .filter(|&index| {
                        let coverage = set.coverage(faces[index]);
                        is_full(coverage) || text.chars().any(|ch| covers(coverage, ch.into()))
                    }),
            );
            for index in used.collect::<Vec<_>>() {
                if self.load(faces[index]) {
                    names.push(FontFamilyName::Named(Cow::Owned(self.alias(faces[index]))));
                }
            }
            if installed {
                names.push(family.clone().into_owned());
            }
        }
        if !changed {
            return style;
        }
        TextStyle {
            font_family: FontFamily::List(Cow::Owned(names)),
            ..style
        }
    }

    fn alias(&self, face: Face) -> String {
        match face {
            Face::Rule(index) => crate::web_fonts::alias(index),
            Face::Document(index) => self.set.documents[index].alias(),
        }
    }

    /// Loads a stylesheet face the first time it matches. Document faces
    /// arrive loaded.
    fn load(&mut self, face: Face) -> bool {
        match face {
            Face::Rule(index) => self.web.load(index, &self.set, &mut self.context),
            Face::Document(_) => true,
        }
    }
}

pub fn decode_font(bytes: &[u8]) -> Option<Arc<[u8]>> {
    use skrifa::raw::TableProvider;
    let decoded = if bytes.starts_with(b"wOF2") {
        Cow::Owned(wuff::decompress_woff2(bytes).ok()?)
    } else {
        Cow::Borrowed(bytes)
    };
    let font = skrifa::FontRef::from_index(&decoded, 0).ok()?;
    font.head().ok()?;
    font.maxp().ok()?;
    font.cmap().ok()?;
    Some(Arc::from(decoded.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(weight: [f32; 2], style: FontStyle, width: [f32; 2]) -> FaceAttributes {
        FaceAttributes {
            weight,
            style: [style; 2],
            width,
            ranges: Vec::new(),
        }
    }

    fn request(weight: f32, style: FontStyle, width: f32) -> FontRequest {
        FontRequest {
            weight,
            style,
            width,
        }
    }

    #[test]
    fn matching_narrows_by_width_then_style_then_weight() {
        let regular = face([400.0; 2], FontStyle::NORMAL, [100.0; 2]);
        let bold = face([700.0; 2], FontStyle::NORMAL, [100.0; 2]);
        let italic = face([400.0; 2], FontStyle::ITALIC, [100.0; 2]);
        let bold_italic = face([700.0; 2], FontStyle::ITALIC, [100.0; 2]);
        let condensed = face([700.0; 2], FontStyle::ITALIC, [75.0; 2]);
        let faces = [
            (0, &regular),
            (1, &bold),
            (2, &italic),
            (3, &bold_italic),
            (4, &condensed),
        ];
        let pick = |weight, style, width| matching_faces(&faces, request(weight, style, width));
        assert_eq!(pick(400.0, FontStyle::NORMAL, 100.0), [0]);
        assert_eq!(pick(700.0, FontStyle::NORMAL, 100.0), [1]);
        assert_eq!(pick(400.0, FontStyle::ITALIC, 100.0), [2]);
        assert_eq!(pick(700.0, FontStyle::ITALIC, 100.0), [3]);
        // Width narrows first: a condensed request takes the condensed face
        // even though it is bold italic.
        assert_eq!(pick(400.0, FontStyle::NORMAL, 75.0), [4]);
        // Between 400 and 500, lighter weights come before bolder ones; above
        // 500, bolder ones come first; below 400, lighter ones do.
        assert_eq!(pick(500.0, FontStyle::NORMAL, 100.0), [0]);
        assert_eq!(pick(600.0, FontStyle::NORMAL, 100.0), [1]);
        assert_eq!(pick(300.0, FontStyle::NORMAL, 100.0), [0]);
        assert_eq!(pick(900.0, FontStyle::NORMAL, 100.0), [1]);
        // A small oblique angle keeps to upright faces before italic ones,
        // and a large one takes the italic face.
        assert_eq!(pick(400.0, FontStyle::oblique(10.0), 100.0), [0]);
        assert_eq!(pick(400.0, FontStyle::oblique(30.0), 100.0), [2]);
        // With no italic face, an italic request takes the upright one.
        assert_eq!(
            matching_faces(&faces[..2], request(400.0, FontStyle::ITALIC, 100.0)),
            [0]
        );
    }

    #[test]
    fn expanded_requests_prefer_wider_faces() {
        let condensed = face([400.0; 2], FontStyle::NORMAL, [75.0; 2]);
        let expanded = face([400.0; 2], FontStyle::NORMAL, [125.0; 2]);
        let faces = [(0, &condensed), (1, &expanded)];
        assert_eq!(
            matching_faces(&faces, request(400.0, FontStyle::NORMAL, 100.0)),
            [0]
        );
        assert_eq!(
            matching_faces(&faces, request(400.0, FontStyle::NORMAL, 110.0)),
            [1]
        );
    }

    #[test]
    fn faces_with_equal_descriptors_form_one_composite_face_last_first() {
        let mut latin = face([400.0; 2], FontStyle::NORMAL, [100.0; 2]);
        latin.ranges = vec![RangeInclusive::new(0x0, 0xFF)];
        let mut cyrillic = latin.clone();
        cyrillic.ranges = vec![RangeInclusive::new(0x400, 0x4FF)];
        let variable = face([100.0, 900.0], FontStyle::NORMAL, [100.0; 2]);
        let bold = face([700.0; 2], FontStyle::NORMAL, [100.0; 2]);
        let faces = [(0, &latin), (1, &cyrillic), (2, &bold)];
        assert_eq!(
            matching_faces(&faces, request(400.0, FontStyle::NORMAL, 100.0)),
            [1, 0]
        );
        // A weight range containing the request matches exactly.
        let faces = [(0, &latin), (1, &variable)];
        assert_eq!(
            matching_faces(&faces, request(650.0, FontStyle::NORMAL, 100.0)),
            [1]
        );
        assert!(latin.covers('A') && !latin.covers('Ж') && cyrillic.covers('Ж'));
        assert!(cyrillic.covers_any(&"AЖ".encode_utf16().collect::<Vec<_>>()));
    }

    #[test]
    fn faces_render_their_range_less_the_later_faces_of_their_composite_face() {
        let everything = face([400.0; 2], FontStyle::NORMAL, [100.0; 2]);
        let mut digits = everything.clone();
        digits.ranges = vec![
            RangeInclusive::new(0x39, 0x39),
            RangeInclusive::new(0x30, 0x38),
        ];
        let mut bold = everything.clone();
        bold.weight = [700.0; 2];
        let coverage = exclusive_coverage(&[
            ("Sans", &everything),
            ("Sans", &digits),
            ("sans", &bold),
            ("Other", &digits),
        ]);
        assert_eq!(coverage[0], [0..=0x2F, 0x3A..=0x10FFFF]);
        assert_eq!(coverage[1], [RangeInclusive::new(0x30, 0x39)]);
        // A face with other descriptors, or another family, is its own
        // composite face.
        assert_eq!(coverage[2], [ALL]);
        assert_eq!(coverage[3], [RangeInclusive::new(0x30, 0x39)]);
        assert!(covers(&coverage[0], 0x41) && !covers(&coverage[0], 0x35));
        assert!(!covers(&coverage[1], 0x2F) && covers(&coverage[1], 0x39));
    }
}
