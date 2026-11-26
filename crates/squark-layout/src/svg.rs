// SPDX-License-Identifier: MIT

use std::fmt::Write;

use squark_mirror::Document;
use squark_text::Fonts;
use squark_types::{
    NodeId, Rect, Size,
    fragment::{MirrorRead, SvgGeometry},
};
use style::{
    color::AbsoluteColor,
    values::computed::{SVGPaint, SVGPaintKind},
};

pub(crate) fn geometry(document: &Document, root: NodeId, size: Size) -> Option<SvgGeometry> {
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let mut xml = String::new();
    write_node(document, root, Some(size), None, &mut xml);
    squark_assets::svg_geometry(&xml)
}

pub(crate) fn bounds(
    document: &Document,
    fonts: &mut Fonts,
    root: NodeId,
    target: NodeId,
    size: Size,
) -> Option<(squark_types::Rect, squark_types::Rect)> {
    let mut name = if document.is_svg_root(target) {
        None
    } else {
        document.attribute(target, "id")
    }
    .map(String::from_utf16_lossy)
    .filter(|name| !name.is_empty())
    .unwrap_or_default();
    if name.is_empty() {
        name = format!("squark-query-{}", target.raw());
        while svg_id_exists(document, root, &name) {
            name.push('_');
        }
    }
    let mut xml = String::new();
    let mut query = BoundsQuery {
        target,
        name: &name,
        fonts,
    };
    write_node(document, root, Some(size), Some(&mut query), &mut xml);
    squark_assets::svg_bounds(&xml, &name)
}

fn svg_id_exists(document: &Document, root: NodeId, name: &str) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if document
            .attribute(node, "id")
            .is_some_and(|id| id.iter().copied().eq(name.encode_utf16()))
        {
            return true;
        }
        pending.extend(document.children(node).unwrap_or_default());
    }
    false
}

pub(crate) fn element_name(document: &Document, id: NodeId) -> Option<String> {
    let name = document.node(id)?.name?;
    let namespace = document.atom(name.namespace?)?;
    namespace
        .iter()
        .copied()
        .eq("http://www.w3.org/2000/svg".encode_utf16())
        .then(|| String::from_utf16_lossy(document.atom(name.local).unwrap_or_default()))
}

struct BoundsQuery<'a> {
    target: NodeId,
    name: &'a str,
    fonts: &'a mut Fonts,
}

impl BoundsQuery<'_> {
    fn write_text_bbox(&mut self, document: &Document, text: NodeId, xml: &mut String) {
        let is_target = std::iter::successors(Some(self.target), |&id| document.parent(id))
            .any(|id| id == text);
        let target = if is_target { self.target } else { text };
        let Some(bounds) = crate::svg_text::measure(self.fonts, document, target).bounds else {
            return;
        };
        let Rect { origin, size } = bounds;
        let _ = write!(
            xml,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
            origin.x, origin.y, size.width, size.height
        );
        if is_target {
            xml.push_str(" id=\"");
            escape(&self.name.encode_utf16().collect::<Vec<_>>(), xml);
            xml.push('"');
        }
        if let Some(transform) = document.attribute(text, "transform") {
            xml.push_str(" transform=\"");
            escape(transform, xml);
            xml.push('"');
        }
        xml.push_str("/>");
    }
}

fn write_node(
    document: &Document,
    id: NodeId,
    root_size: Option<Size>,
    mut query: Option<&mut BoundsQuery<'_>>,
    xml: &mut String,
) {
    let Some(node) = MirrorRead::node(document, id) else {
        return;
    };
    let Some(name) = node.name else {
        if let Some(text) = node.text {
            escape(text, xml);
        }
        return;
    };
    if !name
        .namespace
        .and_then(|ns| document.atom(ns))
        .is_some_and(|ns| {
            ns.iter()
                .copied()
                .eq("http://www.w3.org/2000/svg".encode_utf16())
        })
    {
        return;
    }
    let name = String::from_utf16_lossy(document.atom(name.local).unwrap_or_default());
    if !is_xml_name(&name) {
        return;
    }
    if name == "text"
        && let Some(query) = query.as_deref_mut()
    {
        query.write_text_bbox(document, id, xml);
        return;
    }
    let query_name = query
        .as_deref()
        .filter(|query| query.target == id)
        .map(|query| query.name);
    let query_group = name == "svg" && query_name.is_some();
    let _ = write!(xml, "<{name}");
    if let Some(size) = root_size {
        let _ = write!(
            xml,
            " xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{}\" height=\"{}\"",
            size.width, size.height
        );
    }
    let projected = write_paints(document, id, root_size.is_some(), xml);
    for attr in node.attrs {
        let Some(name) = document.atom(attr.name) else {
            continue;
        };
        let name = String::from_utf16_lossy(name);
        let name = name.rsplit('\0').next().unwrap_or_default();
        if !is_xml_name(name)
            || name == "xmlns"
            || name.starts_with("xmlns:")
            || (projected && name == "style")
            || (root_size.is_some() && matches!(name, "width" | "height"))
            || (query_name.is_some() && !query_group && name == "id")
        {
            continue;
        }
        let _ = write!(xml, " {name}=\"");
        escape(attr.value, xml);
        xml.push('"');
    }
    if let Some(name) = query_name.filter(|_| !query_group) {
        xml.push_str(" id=\"");
        escape(&name.encode_utf16().collect::<Vec<_>>(), xml);
        xml.push('"');
    }
    xml.push('>');
    if let Some(name) = query_name.filter(|_| query_group) {
        let _ = write!(xml, "<g id=\"{name}\">");
    }
    for &child in document.children(id).unwrap_or_default() {
        write_node(document, child, None, query.as_deref_mut(), xml);
    }
    if query_group {
        xml.push_str("</g>");
    }
    let _ = write!(xml, "</{name}>");
}

fn write_paints(document: &Document, id: NodeId, root: bool, xml: &mut String) -> bool {
    let Some(css) = document.computed_style(id) else {
        return false;
    };
    let original = document.attribute(id, "style");
    let parent = (!root)
        .then(|| {
            document
                .parent(id)
                .and_then(|parent| document.computed_style(parent))
        })
        .flatten();
    // usvg sees this subtree in isolation. Carry computed paints over the
    // document cascade, including values reached through custom properties.
    // Leave inherited declarations implicit so a referenced <use> subtree
    // can still pick up the instance's own inherited paint.
    xml.push_str(" style=\"");
    let current = css.get_color();
    if original.is_some()
        || parent
            .as_ref()
            .is_none_or(|parent| parent.get_color() != current)
    {
        write_color("color", current, xml);
    }
    if original.is_some()
        || parent
            .as_ref()
            .is_none_or(|parent| parent.get_fill() != css.get_fill())
    {
        write_paint("fill", css.get_fill(), current, xml);
    }
    if original.is_some()
        || parent
            .as_ref()
            .is_none_or(|parent| parent.get_stroke() != css.get_stroke())
    {
        write_paint("stroke", css.get_stroke(), current, xml);
    }
    if let Some(original) = original {
        escape(original, xml);
    }
    xml.push('"');
    true
}

fn write_paint(property: &str, paint: &SVGPaint, current: &AbsoluteColor, xml: &mut String) {
    match &paint.kind {
        SVGPaintKind::None => {
            let _ = write!(xml, "{property}:none!important;");
        }
        SVGPaintKind::Color(color) if color.is_currentcolor() => {
            let _ = write!(xml, "{property}:currentColor!important;");
        }
        SVGPaintKind::Color(color) => {
            write_color(property, &color.resolve_to_absolute(current), xml);
        }
        // Keep paint servers and context-dependent values in the SVG source.
        _ => {}
    }
}

fn write_color(property: &str, color: &AbsoluteColor, xml: &mut String) {
    let [r, g, b, a] = squark_style::canvas_rgba8(color);
    let _ = write!(xml, "{property}:#{r:02x}{g:02x}{b:02x}{a:02x}!important;");
}

fn is_xml_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn escape(value: &[u16], xml: &mut String) {
    for c in char::decode_utf16(value.iter().copied()) {
        match c.unwrap_or(char::REPLACEMENT_CHARACTER) {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            c @ ('\t' | '\n' | '\r') => xml.push(c),
            c if !c.is_control() => xml.push(c),
            _ => {}
        }
    }
}
