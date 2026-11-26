// SPDX-License-Identifier: MIT

use squark_mirror::Document;
use squark_text::Fonts;
use squark_types::{
    NodeId, Point, Rect,
    engine::{Direction, MeasureText},
};
use style::computed_values::direction::T;

#[derive(Default)]
pub(crate) struct TextGeometry {
    pub bounds: Option<Rect>,
    pub length: f64,
    chunk_length: f64,
}

pub(crate) fn measure(fonts: &mut Fonts, document: &Document, target: NodeId) -> TextGeometry {
    let mut ancestors = std::iter::successors(Some(target), |id| document.parent(*id));
    let Some(root) =
        ancestors.find(|&id| crate::svg::element_name(document, id).as_deref() == Some("text"))
    else {
        return TextGeometry::default();
    };
    let mut geometry = TextGeometry::default();
    let mut point = Point::default();
    let mut whitespace = Whitespace::default();
    visit(
        fonts,
        document,
        root,
        target,
        &mut point,
        &mut whitespace,
        &mut geometry,
    );
    if let Some(bounds) = geometry.bounds.as_mut()
        && let Some(css) = document.computed_style(root)
    {
        use style::values::computed::TextAnchor;
        let anchor = document
            .attribute(root, "text-anchor")
            .map(String::from_utf16_lossy);
        let anchor = match anchor.as_deref() {
            Some("middle") => TextAnchor::Middle,
            Some("end") => TextAnchor::End,
            _ => TextAnchor::Start,
        };
        let rtl = *css.get_direction() == T::Rtl;
        let factor = match (anchor, rtl) {
            (TextAnchor::Middle, _) => 0.5,
            (TextAnchor::End, false) | (TextAnchor::Start, true) => 1.0,
            _ => 0.0,
        };
        bounds.origin.x -= (geometry.chunk_length as f32) * factor;
    }
    geometry
}

#[derive(Default)]
struct Whitespace {
    seen_text: bool,
    pending_space: bool,
}

impl Whitespace {
    fn prepare(&mut self, units: &[u16], preserve: bool) -> Vec<u16> {
        let mut text = Vec::new();
        for &unit in units {
            if matches!(unit, 9..=13 | 32) && !preserve {
                self.pending_space = self.seen_text;
            } else {
                if self.pending_space {
                    text.push(32);
                    self.pending_space = false;
                }
                self.seen_text = true;
                text.push(unit);
            }
        }
        text
    }
}

fn visit(
    fonts: &mut Fonts,
    document: &Document,
    node: NodeId,
    target: NodeId,
    point: &mut Point,
    whitespace: &mut Whitespace,
    geometry: &mut TextGeometry,
) {
    let Some(css) = document.computed_style(node) else {
        return;
    };
    if css.get_display().is_none() {
        return;
    }
    let attr = |key| {
        let value = String::from_utf16_lossy(document.attribute(node, key)?);
        value
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .find(|s| !s.is_empty())?
            .trim_end_matches("px")
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite())
    };
    point.x = attr("x").unwrap_or(point.x) + attr("dx").unwrap_or_default();
    point.y = attr("y").unwrap_or(point.y) + attr("dy").unwrap_or_default();
    let text_style = squark_text::style(node, &css);
    let direction = if *css.get_direction() == T::Rtl {
        Direction::Rtl
    } else {
        Direction::Ltr
    };
    for &child in document.children(node).unwrap_or_default() {
        if document.is_text(child) {
            let preserve = matches!(
                text_style.white_space_collapse,
                parley::WhiteSpaceCollapse::Preserve | parley::WhiteSpaceCollapse::BreakSpaces
            );
            let text = whitespace.prepare(document.text(child).unwrap_or_default(), preserve);
            if text.is_empty() {
                continue;
            }
            let metrics = fonts.measure(
                MeasureText {
                    font_css: "",
                    text: &text,
                    direction,
                    letter_spacing: text_style.letter_spacing,
                    word_spacing: text_style.word_spacing,
                    font_environment: Some(node),
                },
                &css,
            );
            let width = metrics.width as f32;
            if target == node {
                let ascent = metrics.font_ascent as f32;
                let descent = metrics.font_descent as f32;
                let rect = Rect::new(point.x, point.y - ascent, width, ascent + descent);
                geometry.bounds = Some(crate::query::bounding(
                    geometry.bounds.into_iter().chain([rect]),
                ));
                geometry.length += metrics.width;
            }
            point.x += width;
            geometry.chunk_length += metrics.width;
        } else if matches!(
            crate::svg::element_name(document, child).as_deref(),
            Some("tspan" | "textPath" | "a")
        ) {
            let target = if target == node { child } else { target };
            visit(fonts, document, child, target, point, whitespace, geometry);
        }
    }
}
