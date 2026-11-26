// SPDX-License-Identifier: MIT
use std::{
    borrow::Cow,
    cmp::Ordering,
    ops::{Range, RangeInclusive},
    sync::Arc,
};

use parley::{FontFamily, FontFamilyName, TextStyle, fontique};
use style::values::computed::FontStyle;

use crate::{Fonts, TextBrush};

#[derive(Clone, Debug, PartialEq)]
pub struct FaceAttributes {
    pub weight: [f32; 2],
    pub style: [FontStyle; 2],
    pub ranges: Vec<RangeInclusive<u32>>,
}

impl FaceAttributes {
    pub fn covers(&self, ch: char) -> bool {
        self.ranges.is_empty()
            || self
                .ranges
                .iter()
                .any(|range| range.contains(&u32::from(ch)))
    }

    fn style_rank(&self, style: FontStyle) -> (u8, f32) {
        let [first, last] = self.style;
        if style >= first && style <= last {
            return (0, 0.0);
        }
        if first == FontStyle::ITALIC {
            return (2, 0.0);
        }
        if style == FontStyle::ITALIC {
            if first == FontStyle::NORMAL && last == FontStyle::NORMAL {
                return (2, 0.0);
            }
            let nearest = 14.0_f32.clamp(first.oblique_degrees(), last.oblique_degrees());
            return (1, (14.0 - nearest).abs());
        }
        let angle = style.oblique_degrees();
        let nearest = angle.clamp(first.oblique_degrees(), last.oblique_degrees());
        (1, (angle - nearest).abs())
    }

    fn weight_rank(&self, weight: f32) -> (u8, f32) {
        let [first, last] = self.weight;
        let nearest = weight.clamp(first, last);
        if weight == nearest {
            return (0, 0.0);
        }
        let rank = if weight < 400.0 {
            if nearest < weight { 1 } else { 2 }
        } else if weight > 500.0 {
            if nearest > weight { 1 } else { 2 }
        } else if (weight..=500.0).contains(&nearest) {
            1
        } else if nearest < weight {
            2
        } else {
            3
        };
        (rank, (nearest - weight).abs())
    }

    pub fn compare(&self, other: &Self, weight: f32, style: FontStyle) -> Ordering {
        let compare =
            |(a, x): (u8, f32), (b, y): (u8, f32)| a.cmp(&b).then_with(|| x.total_cmp(&y));
        compare(self.style_rank(style), other.style_rank(style))
            .then_with(|| compare(self.weight_rank(weight), other.weight_rank(weight)))
    }
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

    pub(crate) fn register(&self, context: &mut parley::FontContext) {
        let alias = self.alias();
        let overrides = fontique::FontInfoOverride {
            family_name: Some(&alias),
            style: Some(crate::css::font_style(self.attributes.style[0])),
            weight: Some(fontique::FontWeight::new(self.attributes.weight[0])),
            ..Default::default()
        };
        context.collection.register_fonts(
            fontique::Blob::new(Arc::new(self.bytes.clone())),
            Some(overrides),
        );
    }
}

impl Fonts {
    /// The line strut uses the primary font that can supply a space,
    /// including faces registered through the document's FontFaceSet.
    pub fn primary_style(
        &self,
        style: TextStyle<'static, 'static, TextBrush>,
    ) -> TextStyle<'static, 'static, TextBrush> {
        if self.document_fonts.is_empty() {
            return style;
        }
        self.style_ranges(" ", style)
            .pop()
            .expect("a space has one style range")
            .1
    }

    pub fn style_ranges(
        &self,
        text: &str,
        style: TextStyle<'static, 'static, TextBrush>,
    ) -> Vec<(Range<usize>, TextStyle<'static, 'static, TextBrush>)> {
        let FontFamily::List(families) = &style.font_family else {
            return vec![(0..text.len(), style)];
        };
        if self.document_fonts.is_empty()
            || !families.iter().any(|family| {
                matches!(family, FontFamilyName::Named(name)
            if self.document_fonts.iter().any(|face| face.family.eq_ignore_ascii_case(name)))
            })
        {
            return vec![(0..text.len(), style)];
        }
        let weight = style.font_weight.value();
        let slope = match style.font_style {
            fontique::FontStyle::Normal => FontStyle::NORMAL,
            fontique::FontStyle::Italic => FontStyle::ITALIC,
            fontique::FontStyle::Oblique(angle) => FontStyle::oblique(angle.unwrap_or(14.0)),
        };
        let mut spans: Vec<(Range<usize>, TextStyle<'_, '_, _>)> = Vec::new();
        for (offset, ch) in text.char_indices() {
            let mut names = Vec::new();
            for family in families.iter() {
                if let FontFamilyName::Named(name) = family
                    && let Some(face) = self
                        .document_fonts
                        .iter()
                        .rev()
                        .filter(|face| {
                            face.family.eq_ignore_ascii_case(name) && face.attributes.covers(ch)
                        })
                        .min_by(|a, b| a.attributes.compare(&b.attributes, weight, slope))
                {
                    names.push(FontFamilyName::Named(Cow::Owned(face.alias())));
                }
                names.push(family.clone());
            }
            let end = offset + ch.len_utf8();
            let family = FontFamily::List(Cow::Owned(names));
            if let Some((range, prior)) = spans.last_mut()
                && prior.font_family == family
            {
                range.end = end;
            } else {
                spans.push((
                    offset..end,
                    TextStyle {
                        font_family: family,
                        ..style.clone()
                    },
                ));
            }
        }
        spans
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
