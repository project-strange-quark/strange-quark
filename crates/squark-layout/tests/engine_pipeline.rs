// SPDX-License-Identifier: MIT
//! Journal → Stylo → Taffy/Parley integration test.

use parley::{FontContext, GenericFamily, Layout, LayoutContext, LineHeight, StyleProperty};
use squark_journal::BatchDecoder;
use squark_mirror::Document;
use squark_types::journal::{Opcode, Record};
use squark_types::{NodeId, Rect};
use style::context::QuirksMode;
use style::device::Device;
use style::device::servo::{FontMetricsProvider, MediaData};
use style::font_metrics::FontMetrics;
use style::media_queries::{MediaList, MediaType};
use style::properties::{ComputedValues, style_structs::Font};
use style::servo_arc::Arc;
use style::stylesheets::{AllowImportRules, DocumentStyleSheet, Origin, Stylesheet};
use style::stylist::Stylist;
use style::values::computed::font::{
    GenericFontFamily, LineHeight as CssLineHeight, QueryFontMetricsFlags,
};
use style::values::computed::{CSSPixelLength, Length, Size as CssSize};
use taffy::prelude::TaffyAuto;
use taffy::{AvailableSpace, BoxSizing, Dimension, Display, Size, Style, TaffyTree};

const VIEWPORT: Size<f32> = Size {
    width: 800.0,
    height: 600.0,
};
const DOCUMENT: u32 = 1;
const CARD: u32 = 2;
const STYLE_ELEMENT: u32 = 3;
const STYLE_TEXT: u32 = 4;
const PARAGRAPH: u32 = 5;
const TEXT: u32 = 6;

#[test]
fn lays_out_a_journal_document_at_800_by_600() {
    let mut document = replay_fixture();
    let stylesheet_node = document.children(node(CARD)).unwrap()[0];
    assert_eq!(stylesheet_node, node(STYLE_ELEMENT));
    let stylesheet_text = document.children(stylesheet_node).unwrap()[0];
    let mut stylist = Stylist::new(device(), QuirksMode::NoQuirks);
    add_sheet(
        &mut stylist,
        &document,
        Origin::UserAgent,
        "style { display: none }",
    );
    add_sheet(
        &mut stylist,
        &document,
        Origin::Author,
        &String::from_utf16_lossy(document.text(stylesheet_text).unwrap()),
    );
    document.cascade(
        &mut stylist,
        &url::Url::parse("https://example.test/index.html")
            .unwrap()
            .into(),
    );

    let card = document.computed_style(node(CARD)).unwrap();
    let paragraph = document.computed_style(node(PARAGRAPH)).unwrap();
    assert!(
        document
            .computed_style(stylesheet_node)
            .unwrap()
            .slow_clone_display()
            .is_none()
    );

    let text_node = document.children(node(PARAGRAPH)).unwrap()[0];
    let text = &String::from_utf16_lossy(document.text(text_node).unwrap());
    let font = paragraph.get_font();
    let mut fonts = FontContext::new();
    let mut layouts = LayoutContext::new();
    let mut builder = layouts.ranged_builder(&mut fonts, text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(GenericFamily::Monospace.into()));
    builder.push_default(StyleProperty::FontSize(font.font_size.used_size.0.px()));
    let CssLineHeight::Length(line_height) = font.line_height else {
        panic!("expected computed length for line-height");
    };
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
        line_height.0.px(),
    )));
    let shaped: Layout<()> = builder.build(text);

    let mut tree = TaffyTree::new();
    tree.disable_rounding();
    let p_box = tree
        .new_leaf_with_context(block_style(&paragraph), shaped)
        .unwrap();
    let card_box = tree
        .new_with_children(block_style(&card), &[p_box])
        .unwrap();
    let viewport_box = tree
        .new_with_children(
            Style {
                display: Display::Block,
                size: VIEWPORT.map(Dimension::length),
                ..Default::default()
            },
            &[card_box],
        )
        .unwrap();

    tree.compute_layout_with_measure(
        viewport_box,
        VIEWPORT.map(AvailableSpace::Definite),
        |inputs, _, context, style| {
            taffy::compute_leaf_layout(
                inputs,
                style,
                |_, _| unreachable!("fixture has no calc()"),
                |known, space| {
                    let Some(text) = context else {
                        return Size::ZERO;
                    };
                    text.break_all_lines(known.width.or(space.width.into_option()));
                    Size {
                        width: text.width(),
                        height: text.height(),
                    }
                },
            )
        },
    )
    .unwrap();
    let card_rect = border_box(tree.layout(card_box).unwrap(), Rect::default());
    let paragraph_rect = border_box(tree.layout(p_box).unwrap(), card_rect);
    assert_eq!(card_rect, Rect::new(0.0, 0.0, 200.0, 84.0));
    assert_eq!(paragraph_rect, Rect::new(10.0, 10.0, 180.0, 64.0));
    let lines = tree.get_node_context(p_box).unwrap();
    assert_eq!(lines.lines().count(), 2);
    assert!(lines.lines().all(|line| line.runs().next().is_some()));
}

fn node(id: u32) -> NodeId {
    NodeId::new(id, 0).unwrap()
}

fn replay_fixture() -> Document {
    use Opcode::{CreateElement, CreateText, Insert, SetAttr};
    let strings = [
        "http://www.w3.org/1999/xhtml",
        "div",
        "style",
        "p",
        "class",
        "copy",
        "div { display: block; width: 100px; padding: 8px; border: 2px solid; line-height: 32px }
         @media (min-width: 800px) { div { width: 22.5vw } }
         p { display: block; font-size: 12px }
         div > p.copy { font-size: 24px }",
        "MMMMMMMM MMMMMMMM",
    ];
    let record = |op, id, b, c| Record {
        op,
        flags: 0,
        a: node(id).raw(),
        b,
        c,
    };
    let insert = |parent, child| record(Insert, parent, node(child).raw(), 0);
    let atom = |id, kind| Record {
        op: Opcode::DefineAtom,
        flags: 0,
        a: id,
        b: id - 1,
        c: kind,
    };
    let records = [
        atom(1, 4),
        atom(2, 1),
        atom(3, 1),
        atom(4, 1),
        atom(5, 2),
        record(CreateElement, CARD, 2, 1),
        record(CreateElement, STYLE_ELEMENT, 3, 1),
        record(CreateText, STYLE_TEXT, 6, 0),
        record(CreateElement, PARAGRAPH, 4, 1),
        record(SetAttr, PARAGRAPH, 5, 5),
        record(CreateText, TEXT, 7, 0),
        insert(DOCUMENT, CARD),
        insert(CARD, STYLE_ELEMENT),
        insert(STYLE_ELEMENT, STYLE_TEXT),
        insert(CARD, PARAGRAPH),
        insert(PARAGRAPH, TEXT),
    ];
    let mut offsets = vec![0u32];
    let mut units = Vec::new();
    for string in strings {
        units.extend(string.encode_utf16());
        offsets.push(units.len() as u32);
    }
    let mut bytes: Vec<_> = [
        (records.len() * 16) as u32,
        strings.len() as u32,
        units.len() as u32,
        1,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect();
    bytes.extend(records.iter().flat_map(Record::encode));
    bytes.extend(offsets.into_iter().flat_map(u32::to_le_bytes));
    bytes.extend(units.into_iter().flat_map(u16::to_le_bytes));

    let mut document = Document::new(node(DOCUMENT), QuirksMode::NoQuirks).unwrap();
    let mut decoder = BatchDecoder::new(node(DOCUMENT)).unwrap();
    document.apply_journal(&mut decoder, &bytes).unwrap();
    document
}

fn add_sheet(stylist: &mut Stylist, document: &Document, origin: Origin, css: &str) {
    let lock = document.style_lock();
    let sheet = Stylesheet::from_str(
        css,
        url::Url::parse("https://example.test/index.html")
            .unwrap()
            .into(),
        origin,
        Arc::new(lock.wrap(MediaList::empty())),
        lock.clone(),
        None,
        None,
        QuirksMode::NoQuirks,
        AllowImportRules::No,
    );
    stylist.append_stylesheet(DocumentStyleSheet(Arc::new(sheet)), &lock.read());
}

fn device() -> Device {
    Device::new(
        ComputedValues::initial_values_with_font_override(Font::initial_values()),
        Box::new(FixtureFontMetrics),
        "Submit".into(),
        MediaData {
            media_type: MediaType::screen(),
            viewport_size: euclid::Size2D::new(VIEWPORT.width, VIEWPORT.height),
            device_size: euclid::Size2D::new(VIEWPORT.width, VIEWPORT.height),
            device_pixel_ratio: euclid::Scale::new(1.0),
            quirks_mode: QuirksMode::NoQuirks,
            prefers_color_scheme: style::queries::values::PrefersColorScheme::Light,
            primary_pointer_capabilities: Default::default(),
            all_pointer_capabilities: Default::default(),
        },
    )
}

#[derive(Debug)]
struct FixtureFontMetrics;

impl FontMetricsProvider for FixtureFontMetrics {
    fn query_font_metrics(
        &self,
        _: bool,
        _: &Font,
        _: CSSPixelLength,
        _: QueryFontMetricsFlags,
    ) -> FontMetrics {
        // The fixture uses pixel font sizes and line heights, so no font-relative metric is needed.
        FontMetrics::default()
    }

    fn base_size_for_generic(&self, _: GenericFontFamily) -> Length {
        Length::new(16.0)
    }
}

fn block_style(css: &ComputedValues) -> Style {
    let width = match &css.get_position().width {
        CssSize::Auto => Dimension::AUTO,
        CssSize::LengthPercentage(width) => Dimension::length(width.0.to_length().unwrap().px()),
        _ => panic!("unexpected computed width in fixture"),
    };
    let padding = css.get_padding();
    let border = css.get_border();
    let pad = |value: &style::values::computed::NonNegativeLengthPercentage| {
        taffy::LengthPercentage::length(value.0.to_length().unwrap().px())
    };
    let used_border = |width: f32, style: style::values::computed::BorderStyle| {
        taffy::LengthPercentage::length(if style.none_or_hidden() { 0.0 } else { width })
    };
    Style {
        display: Display::Block,
        box_sizing: BoxSizing::ContentBox,
        size: Size {
            width,
            height: Dimension::AUTO,
        },
        padding: taffy::Rect {
            left: pad(&padding.padding_left),
            right: pad(&padding.padding_right),
            top: pad(&padding.padding_top),
            bottom: pad(&padding.padding_bottom),
        },
        border: taffy::Rect {
            left: used_border(
                border.border_left_width.0.to_f32_px(),
                border.border_left_style,
            ),
            right: used_border(
                border.border_right_width.0.to_f32_px(),
                border.border_right_style,
            ),
            top: used_border(
                border.border_top_width.0.to_f32_px(),
                border.border_top_style,
            ),
            bottom: used_border(
                border.border_bottom_width.0.to_f32_px(),
                border.border_bottom_style,
            ),
        },
        ..Default::default()
    }
}

fn border_box(layout: &taffy::Layout, parent: Rect) -> Rect {
    Rect::new(
        parent.origin.x + layout.location.x,
        parent.origin.y + layout.location.y,
        layout.size.width,
        layout.size.height,
    )
}
