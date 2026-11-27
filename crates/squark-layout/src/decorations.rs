// SPDX-License-Identifier: MIT

use parley::layout::GlyphRun;
use squark_mirror::Document;
use squark_text::TextBrush;
use squark_types::{
    NodeId,
    fragment::{DecorationKind, DecorationSpan, DecorationStyle},
};
use style::{
    computed_value_flags::ComputedValueFlags,
    properties::ComputedValues,
    values::{
        computed::{TextDecorationLine, TextDecorationStyle},
        generics::text::GenericTextDecorationLength,
        specified::box_::{DisplayInside, DisplayOutside},
    },
};

use crate::paint;

pub(crate) fn spans(
    document: &Document,
    first_ancestor: Option<NodeId>,
    css: &ComputedValues,
    run: &GlyphRun<'_, TextBrush>,
    x: [f32; 2],
    baseline: f32,
) -> Box<[DecorationSpan]> {
    if x[0] >= x[1]
        || !css
            .flags
            .contains(ComputedValueFlags::HAS_TEXT_DECORATION_LINES)
    {
        return Box::default();
    }
    let mut result = Vec::new();
    append(&mut result, css, run, x, baseline);
    if propagates_into(css) {
        let mut parent = first_ancestor;
        while let Some(node) = parent {
            let Some(css) = document.computed_style(node) else {
                break;
            };
            if !css
                .flags
                .contains(ComputedValueFlags::HAS_TEXT_DECORATION_LINES)
            {
                break;
            }
            append(&mut result, &css, run, x, baseline);
            if !propagates_into(&css) {
                break;
            }
            parent = document.flat_parent(node);
        }
    }
    result.into_boxed_slice()
}

// An ancestor's line decorations follow in-flow text through nested inline and block boxes.
fn propagates_into(css: &ComputedValues) -> bool {
    let display = css.get_display();
    !css.get_box().position.is_absolutely_positioned()
        && !css.get_float().is_floating()
        && (display.outside() != DisplayOutside::Inline
            || matches!(
                display.inside(),
                DisplayInside::Flow | DisplayInside::Contents
            ))
}

fn append(
    result: &mut Vec<DecorationSpan>,
    css: &ComputedValues,
    glyphs: &GlyphRun<'_, TextBrush>,
    x: [f32; 2],
    baseline: f32,
) {
    let lines = *css.get_text_decoration_line();
    if !lines.intersects(
        TextDecorationLine::UNDERLINE
            | TextDecorationLine::OVERLINE
            | TextDecorationLine::LINE_THROUGH,
    ) || css.get_display().inside() == DisplayInside::Contents
    {
        return;
    }
    let style = match css.get_text_decoration_style() {
        TextDecorationStyle::Solid => DecorationStyle::Solid,
        TextDecorationStyle::Double => DecorationStyle::Double,
        TextDecorationStyle::Dotted => DecorationStyle::Dotted,
        TextDecorationStyle::Dashed => DecorationStyle::Dashed,
        TextDecorationStyle::Wavy => DecorationStyle::Wavy,
        TextDecorationStyle::None => return,
    };
    let color = paint::color(
        &css.get_text_decoration_color()
            .resolve_to_absolute(css.get_color()),
    );
    let font_size = css.get_font().font_size.used_size.0;
    let run = glyphs.run();
    let metrics = run.font_metrics();
    let scale = font_size.px() / run.font_size().max(f32::MIN_POSITIVE);
    let thickness = |font_thickness: f32| match css.get_text_decoration_thickness() {
        GenericTextDecorationLength::LengthPercentage(value) => {
            value.resolve(font_size).px().max(0.0)
        }
        GenericTextDecorationLength::Auto => (font_size.px() / 10.0).floor().max(1.0),
        GenericTextDecorationLength::FromFont => (font_thickness * scale).max(1.0),
    };
    let underline_offset = css.get_text_underline_offset().non_auto().map_or_else(
        || (-metrics.underline_offset * scale).max(1.0),
        |offset| offset.resolve(font_size).px(),
    );
    for (flag, kind, offset, font_thickness) in [
        (
            TextDecorationLine::UNDERLINE,
            DecorationKind::Underline,
            underline_offset,
            metrics.underline_size,
        ),
        (
            TextDecorationLine::OVERLINE,
            DecorationKind::Overline,
            -glyphs.span_metrics().ascent * scale,
            metrics.underline_size,
        ),
        (
            TextDecorationLine::LINE_THROUGH,
            DecorationKind::LineThrough,
            -glyphs.span_metrics().ascent * scale * 0.4,
            metrics.strikethrough_size,
        ),
    ] {
        if lines.contains(flag) {
            let thickness = thickness(font_thickness);
            if thickness > 0.0 {
                result.push(DecorationSpan {
                    x,
                    y: baseline + offset,
                    thickness,
                    style,
                    color,
                    kind,
                });
            }
        }
    }
}
