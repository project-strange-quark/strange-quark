// SPDX-License-Identifier: MIT

use parley::{
    BaseDirection, FontFeature, FontFeatures, Layout, PositionedLayoutItem, TextStyle,
    TextWrapMode, WhiteSpaceCollapse, setting::Tag,
};
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{NormalizedCoord, Size},
    raw::TableProvider,
    string::StringId,
};
use squark_types::{
    NodeId,
    engine::{Direction, MeasureText, TextMetrics},
    render::FontInstanceId,
};
use style::{properties::ComputedValues, values::computed::font::SingleFontFamily};

use crate::{Fonts, TextBrush};

#[derive(Clone, Copy, Debug)]
pub struct ControlMetrics {
    pub average: f32,
    pub maximum: f32,
    pub ascent: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum CanvasAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug)]
pub enum CanvasBaseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

#[derive(Clone, Copy)]
struct CanvasFontMetrics {
    ascent: f32,
    descent: f32,
    em_ascent: f32,
    em_descent: f32,
}

impl CanvasFontMetrics {
    fn new(font: &FontRef<'_>, px: f32, coords: &[NormalizedCoord]) -> Self {
        let metrics = font.metrics(Size::new(px), coords);
        let ascent = metrics.ascent.round();
        let descent = metrics.descent.abs().round();
        let (typo_ascent, typo_descent) = font
            .os2()
            .ok()
            .map(|table| {
                (
                    f32::from(table.s_typo_ascender()),
                    -f32::from(table.s_typo_descender()),
                )
            })
            .filter(|&(a, d)| a > 0.0 && d >= 0.0)
            .unwrap_or((ascent, descent));
        let height = typo_ascent + typo_descent;
        let em_ascent = if height > 0.0 {
            // Normalize the OS/2 typographic metrics to one em, then quantize
            // to the browser's CSS layout unit.
            (typo_ascent * px / height * 64.0).round() / 64.0
        } else {
            0.0
        };
        let em_descent = (px * 64.0).round() / 64.0 - em_ascent;
        Self {
            ascent,
            descent,
            em_ascent,
            em_descent,
        }
    }

    fn offset(self, baseline: CanvasBaseline) -> f32 {
        match baseline {
            CanvasBaseline::Top => self.em_ascent,
            CanvasBaseline::Hanging => self.ascent * 80.0 / 100.0,
            CanvasBaseline::Middle => (self.em_ascent - self.em_descent) / 2.0,
            CanvasBaseline::Alphabetic => 0.0,
            CanvasBaseline::Ideographic => -self.descent,
            CanvasBaseline::Bottom => -self.em_descent,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CanvasText<'a> {
    pub text: &'a [u16],
    pub origin: [f64; 2],
    pub max_width: Option<f64>,
    pub direction: Direction,
    pub align: CanvasAlign,
    pub baseline: CanvasBaseline,
}

#[derive(Debug)]
pub struct CanvasRun {
    pub font: FontInstanceId,
    pub glyphs: Vec<(u32, [f64; 2])>,
}

fn canvas_whitespace(text: &[u16]) -> String {
    char::decode_utf16(text.iter().copied())
        .map(|c| match c.unwrap_or(char::REPLACEMENT_CHARACTER) {
            '\t' | '\n' | '\r' | '\x0b' | '\x0c' => ' ',
            c => c,
        })
        .collect()
}

// Blink `third_party/blink/renderer/core/layout/forms/layout_text_control.cc`
// (`kFontFamiliesWithInvalidCharWidth`): legacy macOS font families whose OS/2
// average character width is missing or unreliable, so Blink falls back to the
// advance of `'0'` on every platform.
const FONT_FAMILIES_WITH_INVALID_CHAR_WIDTH: &[&str] = &[
    "American Typewriter",
    "Arial Hebrew",
    "Chalkboard",
    "Cochin",
    "Corsiva Hebrew",
    "Courier",
    "Euphemia UCAS",
    "Geneva",
    "Gill Sans",
    "Hei",
    "Helvetica",
    "Hoefler Text",
    "InaiMathi",
    "Kai",
    "Lucida Grande",
    "Marker Felt",
    "Monaco",
    "Mshtakan",
    "New Peninim MT",
    "Osaka",
    "Raanana",
    "STHeiti",
    "Symbol",
    "Times",
    "Apple Braille",
    "Apple LiGothic",
    "Apple LiSung",
    "Apple Symbols",
    "AppleGothic",
    "AppleMyungjo",
    "#GungSeo",
    "#HeadLineA",
    "#PCMyungjo",
    "#PilGi",
];

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlMetricsPlatform {
    Apple,
    Windows,
    Other,
}

impl ControlMetricsPlatform {
    const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::Apple
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::Other
        }
    }
}

fn has_valid_avg_char_width(
    font: &FontRef<'_>,
    css_primary_family: Option<&str>,
    raw_average: f32,
    zero_width: f32,
) -> bool {
    // Blink `layout_text_control::HasValidAvgCharWidth`: reject fonts whose
    // average width tracks CJK full-width glyphs rather than ASCII digits.
    if zero_width > 0.0 && raw_average > zero_width * 1.7 {
        return false;
    }

    if let Some(family) = css_primary_family
        && (family.is_empty() || FONT_FAMILIES_WITH_INVALID_CHAR_WIDTH.contains(&family))
    {
        return false;
    }

    let mut saw_font_family = false;
    for name in font.localized_strings(StringId::FAMILY_NAME) {
        saw_font_family = true;
        let family: String = name.chars().collect();
        if FONT_FAMILIES_WITH_INVALID_CHAR_WIDTH.contains(&family.as_str()) {
            return false;
        }
    }

    css_primary_family.is_some() || saw_font_family
}

/// Computes Blink-compatible `(average, maximum, ascent, height)` control
/// metrics for a resolved font face, mirroring:
/// - `third_party/blink/renderer/platform/fonts/simple_font_data.cc` (`PlatformInit`)
/// - `third_party/blink/renderer/core/layout/forms/layout_text_control.cc`
///   (`HasValidAvgCharWidth`, `GetAvgCharWidth`)
/// - `third_party/blink/renderer/core/layout/layout_box.cc`
///   (`TextFieldIntrinsicInlineSize`, `TextAreaIntrinsicInlineSize`)
fn control_metrics_from_font(
    font: &FontRef<'_>,
    font_size: Size,
    coords: &[NormalizedCoord],
    css_primary_family: Option<&str>,
    zero_width: f32,
    authored_line: Option<(f32, f32)>,
    platform: ControlMetricsPlatform,
) -> ControlMetrics {
    let metrics = font.metrics(font_size, coords);
    let glyph_metrics = font.glyph_metrics(font_size, coords);
    let x_width = font
        .charmap()
        .map('x')
        .and_then(|gid| glyph_metrics.advance_width(gid));
    let x_height = metrics
        .x_height
        .filter(|&x| x > 0.0)
        .unwrap_or(metrics.ascent * 0.56);
    let rounded_ascent = metrics.ascent.round();

    // `SimpleFontData::PlatformInit`: the average is Skia's `fAvgCharWidth`
    // (the OS/2 `xAvgCharWidth`) when non-zero, else the `'x'` glyph's advance
    // (falling back to `x_height`). Apple always uses the `'x'` advance, and so
    // does Windows: Skia's DirectWrite backend leaves `fAvgCharWidth` zero.
    // Pinned Chromium 159.0.8154.6 on Windows sizes `<input>` from the `'x'`
    // advance for Arial, Times New Roman, Consolas and Courier New.
    let raw_average = match platform {
        ControlMetricsPlatform::Apple | ControlMetricsPlatform::Windows => {
            x_width.unwrap_or(x_height)
        }
        ControlMetricsPlatform::Other => metrics
            .average_width
            .filter(|&w| w > 0.0)
            .or(x_width)
            .unwrap_or(x_height),
    };

    // `SimpleFontData::PlatformInit`: platform-specific `max_char_width_`.
    // Apple uses the ascent; Windows and Linux use the `head` table's x range
    // (`fXMax - fXMin`), which pinned Chromium on Windows matches.
    let raw_maximum = match platform {
        ControlMetricsPlatform::Apple => rounded_ascent,
        ControlMetricsPlatform::Windows | ControlMetricsPlatform::Other => metrics
            .bounds
            .map_or(raw_average, |bounds| (bounds.x_max - bounds.x_min).round()),
    };

    // `GetAvgCharWidth` + `TextFieldIntrinsicInlineSize`: when the family has
    // a valid average width, round up half-pixels (`max(w, round(w))`) and
    // include the `max_char_width - avg_char_width` allowance when positive;
    // otherwise fall back to `'0'` advance with no extra max allowance.
    let (average, maximum) =
        if has_valid_avg_char_width(font, css_primary_family, raw_average, zero_width) {
            let avg = raw_average.max(raw_average.round());
            (avg, raw_maximum.max(avg))
        } else {
            (zero_width, zero_width)
        };

    // Normal form-control line boxes use the primary font's integer ascent and
    // descent. Author-specified line heights keep the shaped line's baseline
    // (including half-leading) and fractional height.
    let (ascent, height) = authored_line.unwrap_or_else(|| {
        (
            rounded_ascent,
            rounded_ascent + metrics.descent.abs().round() + metrics.leading.round(),
        )
    });

    ControlMetrics {
        average,
        maximum,
        ascent,
        height,
    }
}

impl Fonts {
    fn shape_plain(
        &mut self,
        text: &str,
        mut style: TextStyle<'static, 'static, TextBrush>,
        direction: Direction,
        separate_spaces: bool,
    ) -> Layout<crate::TextBrush> {
        style.white_space_collapse = WhiteSpaceCollapse::Preserve;
        style.text_wrap_mode = TextWrapMode::NoWrap;
        let spans = [(0..text.len(), self.with_web_fonts(text, style))];
        let mut builder = self
            .layout
            .style_run_builder(&mut self.context, text, 1.0, false);
        for (range, style) in spans {
            let span = &text[range.clone()];
            if !separate_spaces || !span.contains(' ') {
                let id = builder.push_style(style);
                builder.push_style_run(id, range);
                continue;
            }
            // Canvas shapes words separately from spaces. A shaping boundary
            // on each side of a space keeps kerning within the word while the
            // layout still resolves direction and spacing across the full line.
            let mut space_style = style.clone();
            if let FontFeatures::List(features) = &mut space_style.font_features {
                const KERN: Tag = Tag::new(b"kern");
                let features = features.to_mut();
                features.retain(|feature| feature.tag != KERN);
                features.push(FontFeature {
                    tag: KERN,
                    value: 0,
                });
            }
            let id = builder.push_style(style);
            let space = builder.push_style(space_style);
            let mut start = range.start;
            for (offset, _) in span.match_indices(' ') {
                let end = range.start + offset;
                if start < end {
                    builder.push_style_run(id, start..end);
                }
                builder.push_style_run(space, end..end + 1);
                start = end + 1;
            }
            if start < range.end {
                builder.push_style_run(id, start..range.end);
            }
        }
        builder.set_base_direction(match direction {
            Direction::Ltr => BaseDirection::Ltr,
            Direction::Rtl => BaseDirection::Rtl,
        });
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        for line in layout.lines() {
            for run in line.runs() {
                let blob = &run.font().data;
                self.measured_sources
                    .entry(blob.id())
                    .or_insert_with(|| blob.clone());
            }
        }
        layout
    }

    pub fn control_metrics(&mut self, css: &ComputedValues) -> ControlMetrics {
        let mut style = crate::style(NodeId::NONE, css);
        style.letter_spacing = 0.0;
        style.word_spacing = 0.0;
        let layout = self.shape_plain("0", style, Direction::Ltr, false);
        let zero_width = layout.full_width();
        let fallback = ControlMetrics {
            average: zero_width,
            maximum: zero_width,
            ascent: layout.first_baseline().unwrap_or_default(),
            height: layout.height().round(),
        };
        let css_primary_family =
            css.get_font()
                .font_family
                .families
                .list
                .first()
                .and_then(|family| match family {
                    SingleFontFamily::FamilyName(name) => Some(name.name.as_ref()),
                    SingleFontFamily::Generic(_) => None,
                });
        let authored_line = (!matches!(
            css.get_line_height(),
            style::values::computed::LineHeight::Normal
        ))
        .then(|| (layout.first_baseline().unwrap_or_default(), layout.height()));
        if let Some(PositionedLayoutItem::GlyphRun(glyphs)) =
            layout.lines().next().and_then(|line| line.items().next())
        {
            let run = glyphs.run();
            if let Ok(font) = FontRef::from_index(run.font().data.as_ref(), run.font().index) {
                let coords: Vec<_> = run
                    .normalized_coords()
                    .iter()
                    .map(|c| NormalizedCoord::from_bits(c.to_bits()))
                    .collect();
                return control_metrics_from_font(
                    &font,
                    Size::new(run.font_size()),
                    coords.as_slice(),
                    css_primary_family,
                    zero_width,
                    authored_line,
                    ControlMetricsPlatform::current(),
                );
            }
        }
        fallback
    }

    pub fn shape_canvas(
        &mut self,
        request: CanvasText<'_>,
        css: &ComputedValues,
    ) -> Vec<CanvasRun> {
        let text = canvas_whitespace(request.text);
        if text.is_empty() {
            return Vec::new();
        }
        let mut style = crate::style(NodeId::NONE, css);
        style.letter_spacing = 0.0;
        style.word_spacing = 0.0;
        let mut layout = self.shape_plain(&text, style.clone(), request.direction, true);
        let width = f64::from(layout.full_width());
        if let Some(max) = request.max_width
            && max < width
        {
            // The Canvas algorithm permits reducing the font size to fit.
            style.font_size *= (max / width) as f32;
            layout = self.shape_plain(&text, style, request.direction, true);
        }
        let Some(line) = layout.lines().next() else {
            return Vec::new();
        };
        let mut glyph_runs = line
            .items()
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(run),
                _ => None,
            })
            .peekable();
        let Some(first) = glyph_runs.peek() else {
            return Vec::new();
        };
        let run = first.run();
        let Ok(font) = FontRef::from_index(run.font().data.as_ref(), run.font().index) else {
            return Vec::new();
        };
        let coords: Vec<_> = run
            .normalized_coords()
            .iter()
            .map(|coord| NormalizedCoord::from_bits(coord.to_bits()))
            .collect();
        let metrics = CanvasFontMetrics::new(&font, run.font_size(), &coords);
        let width = f64::from(layout.full_width());
        let rtl = matches!(request.direction, Direction::Rtl);
        let shift = match request.align {
            CanvasAlign::Start if rtl => width,
            CanvasAlign::End if !rtl => width,
            CanvasAlign::Right => width,
            CanvasAlign::Center => width / 2.0,
            _ => 0.0,
        };
        let baseline = f64::from(metrics.offset(request.baseline));
        let x = request.origin[0] - shift;
        let y = request.origin[1] + baseline - f64::from(line.metrics().baseline);
        let mut runs: Vec<CanvasRun> = Vec::new();
        for item in glyph_runs {
            let run = item.run();
            let coords: Vec<_> = run
                .normalized_coords()
                .iter()
                .map(|v| v.to_bits())
                .collect();
            let font = self.intern(
                run.font(),
                run.font_size(),
                &coords,
                crate::Synthesis::default(),
            );
            let glyphs = item
                .positioned_glyphs()
                .map(|glyph| (glyph.id, [x + f64::from(glyph.x), y + f64::from(glyph.y)]));
            if let Some(last) = runs.last_mut()
                && last.font == font
            {
                last.glyphs.extend(glyphs);
            } else {
                runs.push(CanvasRun {
                    font,
                    glyphs: glyphs.collect(),
                });
            }
        }
        runs
    }

    pub fn measure(&mut self, request: MeasureText<'_>, css: &ComputedValues) -> TextMetrics {
        self.measure_plain(request, css, false)
    }

    pub fn measure_canvas(
        &mut self,
        request: MeasureText<'_>,
        css: &ComputedValues,
    ) -> TextMetrics {
        self.measure_plain(request, css, true)
    }

    fn measure_plain(
        &mut self,
        request: MeasureText<'_>,
        css: &ComputedValues,
        canvas: bool,
    ) -> TextMetrics {
        let empty = request.text.is_empty();
        let text: String = if empty {
            " ".into()
        } else {
            canvas_whitespace(request.text)
        };
        let mut style = crate::style(NodeId::NONE, css);
        style.letter_spacing = request.letter_spacing;
        style.word_spacing = request.word_spacing;
        let layout = self.shape_plain(&text, style, request.direction, canvas);
        let mut result = TextMetrics {
            width: if empty {
                0.0
            } else {
                f64::from(layout.full_width())
            },
            ..Default::default()
        };
        let Some(line) = layout.lines().next() else {
            return result;
        };
        let baseline = line.metrics().baseline;
        let mut first = true;
        let mut ink: Option<[f32; 4]> = None;
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(item) = item else {
                continue;
            };
            let run = item.run();
            let Ok(font) = FontRef::from_index(run.font().data.as_ref(), run.font().index) else {
                continue;
            };
            let size = Size::new(run.font_size());
            let location: Vec<_> = run
                .normalized_coords()
                .iter()
                .map(|c| NormalizedCoord::from_bits(c.to_bits()))
                .collect();
            let coords = location.as_slice();
            if first {
                first = false;
                let metrics = CanvasFontMetrics::new(&font, run.font_size(), coords);
                result.font_ascent = f64::from(metrics.ascent);
                result.font_descent = f64::from(metrics.descent);
                result.em_ascent = f64::from(metrics.em_ascent);
                result.em_descent = f64::from(metrics.em_descent);
                result.hanging_baseline = f64::from(metrics.offset(CanvasBaseline::Hanging));
                result.ideographic_baseline =
                    f64::from(metrics.offset(CanvasBaseline::Ideographic));
            }
            if empty {
                continue;
            }
            let glyphs = font.glyph_metrics(size, coords);
            let instance = canvas.then(|| {
                let bits: Vec<_> = coords.iter().map(|coord| coord.to_bits()).collect();
                self.intern(
                    run.font(),
                    run.font_size(),
                    &bits,
                    crate::Synthesis::default(),
                )
            });
            for glyph in item.positioned_glyphs() {
                let id = GlyphId::new(glyph.id);
                let bounds = if let Some(instance) = instance {
                    self.canvas_ink.glyph(instance, id, &font, size, coords)
                } else {
                    glyphs.bounds(id)
                };
                let Some(bounds) = bounds else {
                    continue;
                };
                if bounds.x_min == bounds.x_max && bounds.y_min == bounds.y_max {
                    continue;
                }
                let x0 = glyph.x + bounds.x_min;
                let y0 = glyph.y - baseline - bounds.y_max;
                let x1 = glyph.x + bounds.x_max;
                let y1 = glyph.y - baseline - bounds.y_min;
                ink = Some(ink.map_or([x0, y0, x1, y1], |b| {
                    [b[0].min(x0), b[1].min(y0), b[2].max(x1), b[3].max(y1)]
                }));
            }
        }
        if let Some([x0, y0, x1, y1]) = ink {
            result.actual_left = -f64::from(x0);
            result.actual_right = f64::from(x1);
            result.actual_ascent = -f64::from(y0);
            result.actual_descent = f64::from(y1);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBERATION_MONO: &[u8] =
        include_bytes!("../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.ttf");

    #[test]
    fn control_metrics_platform_branches_match_blink_rules() {
        let font = FontRef::from_index(LIBERATION_MONO, 0).unwrap();
        let size = Size::new(13.3333);
        let no_coords: &[NormalizedCoord] = &[];
        let metrics = font.metrics(size, no_coords);
        let zero = font
            .glyph_metrics(size, no_coords)
            .advance_width(font.charmap().map('0').unwrap())
            .unwrap();
        let x_adv = font
            .glyph_metrics(size, no_coords)
            .advance_width(font.charmap().map('x').unwrap())
            .unwrap();

        let apple = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Liberation Mono"),
            zero,
            None,
            ControlMetricsPlatform::Apple,
        );
        assert_eq!(apple.average, x_adv.max(x_adv.round()));
        assert_eq!(apple.maximum, metrics.ascent.round().max(apple.average));

        let windows = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Liberation Mono"),
            zero,
            None,
            ControlMetricsPlatform::Windows,
        );
        assert_eq!(windows.average, x_adv.max(x_adv.round()));
        let bounds = metrics.bounds.unwrap();
        assert_eq!(
            windows.maximum,
            (bounds.x_max - bounds.x_min).round().max(windows.average)
        );
        let os2_avg = metrics.average_width.unwrap();

        let linux = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Liberation Mono"),
            zero,
            None,
            ControlMetricsPlatform::Other,
        );
        assert_eq!(linux.average, os2_avg.max(os2_avg.round()));
        assert_eq!(
            linux.maximum,
            (bounds.x_max - bounds.x_min).round().max(linux.average)
        );
    }

    #[test]
    fn windows_averages_the_x_advance_where_linux_uses_os2() {
        // Liberation Mono cannot tell the two apart (it is monospace); in
        // Liberation Sans the OS/2 average and the 'x' advance differ, so
        // this pins which one each platform uses.
        let ttf = wuff::decompress_woff2(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
        ))
        .unwrap();
        let font = FontRef::from_index(&ttf, 0).unwrap();
        let size = Size::new(16.0);
        const NO_COORDS: &[NormalizedCoord] = &[];
        let zero = font
            .glyph_metrics(size, NO_COORDS)
            .advance_width(font.charmap().map('0').unwrap())
            .unwrap();
        let metrics = |platform| {
            control_metrics_from_font(&font, size, NO_COORDS, Some("Arial"), zero, None, platform)
        };
        let x_advance = font
            .glyph_metrics(size, NO_COORDS)
            .advance_width(font.charmap().map('x').unwrap())
            .unwrap();
        let os2_average = font.metrics(size, NO_COORDS).average_width.unwrap();
        assert_ne!(x_advance.round(), os2_average.round());

        let windows = metrics(ControlMetricsPlatform::Windows);
        let linux = metrics(ControlMetricsPlatform::Other);
        assert_eq!(windows.average, x_advance.max(x_advance.round()));
        assert_eq!(linux.average, os2_average.max(os2_average.round()));
        // Both take the maximum from the `head` x range.
        assert_eq!(windows.maximum, linux.maximum);
    }

    #[test]
    fn invalid_avg_char_width_families_and_cjk_heuristic_fall_back_to_zero_width() {
        let font = FontRef::from_index(LIBERATION_MONO, 0).unwrap();
        let size = Size::new(13.3333);
        let no_coords: &[NormalizedCoord] = &[];
        let zero = 7.5;

        // Invalid family name in Blink's list (e.g. Helvetica) disables max extra width.
        let helv = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Helvetica"),
            zero,
            None,
            ControlMetricsPlatform::Apple,
        );
        assert_eq!(helv.average, zero);
        assert_eq!(helv.maximum, zero);

        // CJK heuristic: raw_average > 1.7 * zero_width falls back to zero_width.
        assert!(!has_valid_avg_char_width(&font, Some("Arial"), 17.1, 10.0));
        assert!(has_valid_avg_char_width(&font, Some("Arial"), 16.9, 10.0));
    }

    #[test]
    fn authored_line_height_keeps_baseline_with_half_leading() {
        let font = FontRef::from_index(LIBERATION_MONO, 0).unwrap();
        let size = Size::new(14.0);
        let no_coords: &[NormalizedCoord] = &[];
        let metrics = font.metrics(size, no_coords);

        let normal = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Liberation Mono"),
            8.4,
            None,
            ControlMetricsPlatform::Other,
        );
        assert_eq!(normal.ascent, metrics.ascent.round());

        let authored = control_metrics_from_font(
            &font,
            size,
            no_coords,
            Some("Liberation Mono"),
            8.4,
            Some((24.75, 40.0)),
            ControlMetricsPlatform::Other,
        );
        assert_eq!(authored.ascent, 24.75);
        assert_eq!(authored.height, 40.0);
        assert_eq!(authored.average, normal.average);
        assert_eq!(authored.maximum, normal.maximum);
    }
}
