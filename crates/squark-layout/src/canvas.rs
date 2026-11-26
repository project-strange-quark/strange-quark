// SPDX-License-Identifier: MIT
use std::sync::{Arc, Once};

use squark_assets::ImageCatalog;
use squark_mirror::Document;
use squark_style::{Cascade, canvas_rgba8};
use squark_text::{CanvasAlign, CanvasBaseline, CanvasText, Fonts};
use squark_types::{
    NodeId,
    engine::Direction,
    fragment::Filter,
    render::{FontInstanceId, ImageKey},
};

const HEADER: usize = 12;
const COMMAND_HEADER: usize = 8;
const DRAW_IMAGE: u8 = 23;
const TEXT: u8 = 26;
const FILTER: u8 = 27;
const SET_FILTER: u8 = 28;
const SHAPED: u8 = 24;

enum Command<'a> {
    Copy(&'a [u8]),
    ImageElement { source: NodeId, rects: &'a [u8] },
    Text(Text),
    Filter(Vec<u16>),
    IgnoreFilter,
}

struct Text {
    font: String,
    text: Vec<u16>,
    origin: [f64; 2],
    max_width: Option<f64>,
    align: CanvasAlign,
    baseline: CanvasBaseline,
    direction: Option<Direction>,
}

pub(crate) struct CanvasSubmit<'a> {
    commands: Vec<Command<'a>>,
}

pub(crate) struct RewrittenSubmit {
    pub stream: Arc<[u8]>,
    pub fonts: Vec<FontInstanceId>,
    pub images: Vec<ImageKey>,
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn number(bytes: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect()
}

impl Text {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let origin = [number(bytes, 0)?, number(bytes, 8)?];
        let max_width = number(bytes, 16)?;
        if !origin.iter().all(|value| value.is_finite())
            || (!max_width.is_nan() && (!max_width.is_finite() || max_width <= 0.0))
        {
            return None;
        }
        let align = match word(bytes, 24)? {
            0 => CanvasAlign::Start,
            1 => CanvasAlign::End,
            2 => CanvasAlign::Left,
            3 => CanvasAlign::Right,
            4 => CanvasAlign::Center,
            _ => return None,
        };
        let baseline = match word(bytes, 28)? {
            0 => CanvasBaseline::Top,
            1 => CanvasBaseline::Hanging,
            2 => CanvasBaseline::Middle,
            3 => CanvasBaseline::Alphabetic,
            4 => CanvasBaseline::Ideographic,
            5 => CanvasBaseline::Bottom,
            _ => return None,
        };
        let direction = match word(bytes, 32)? {
            0 => Some(Direction::Ltr),
            1 => Some(Direction::Rtl),
            2 => None,
            _ => return None,
        };
        let font_end = (word(bytes, 36)? as usize)
            .checked_mul(2)?
            .checked_add(44)?;
        let end = (word(bytes, 40)? as usize)
            .checked_mul(2)?
            .checked_add(font_end)?;
        if end != bytes.len() {
            return None;
        }
        Some(Self {
            font: String::from_utf16_lossy(&units(&bytes[44..font_end])),
            text: units(&bytes[font_end..]),
            origin,
            max_width: max_width.is_finite().then_some(max_width),
            align,
            baseline,
            direction,
        })
    }
}

impl<'a> CanvasSubmit<'a> {
    pub(crate) fn parse(stream: &'a [u8]) -> Result<Self, &'static str> {
        if stream.get(..4) != Some(b"JCV1")
            || word(stream, 4).map(|n| n as usize) != Some(stream.len())
        {
            return Err("invalid canvas stream header");
        }
        let count = word(stream, 8).ok_or("missing canvas command count")? as usize;
        let mut commands = Vec::new();
        let mut rest = &stream[HEADER..];
        for _ in 0..count {
            let header = rest
                .get(..COMMAND_HEADER)
                .ok_or("truncated canvas command")?;
            if header[1..4] != [0, 0, 0] {
                return Err("reserved canvas command flags");
            }
            let len = word(header, 4).unwrap() as usize;
            let end = len
                .checked_add(COMMAND_HEADER)
                .ok_or("canvas length overflow")?;
            let aligned = end
                .checked_next_multiple_of(8)
                .ok_or("canvas length overflow")?;
            let bytes = rest.get(..aligned).ok_or("truncated canvas payload")?;
            if !bytes[end..].iter().all(|&byte| byte == 0) {
                return Err("nonzero canvas padding");
            }
            let payload = &bytes[COMMAND_HEADER..end];
            let command = match header[0] {
                DRAW_IMAGE => match (payload.len(), word(payload, 0)) {
                    (72, Some(0)) => Command::ImageElement {
                        source: NodeId::from_raw(word(payload, 4).unwrap()),
                        rects: &payload[8..],
                    },
                    (72, Some(1)) => Command::Copy(bytes),
                    _ => return Err("invalid canvas image payload"),
                },
                TEXT => Command::Text(Text::parse(payload).ok_or("invalid canvas text payload")?),
                FILTER => match word(payload, 0) {
                    Some(len) if (len as usize).checked_mul(2) == Some(payload.len() - 4) => {
                        Command::Filter(units(&payload[4..]))
                    }
                    _ => Command::IgnoreFilter,
                },
                1..=25 => Command::Copy(bytes),
                _ => return Err("unknown canvas opcode"),
            };
            commands.push(command);
            rest = &rest[aligned..];
        }
        if !rest.is_empty() {
            return Err("canvas command count does not match buffer");
        }
        Ok(Self { commands })
    }

    pub(crate) fn rewrite(
        self,
        canvas: NodeId,
        document: &Document,
        cascade: &Cascade<'_>,
        fonts: &mut Fonts,
        images: &ImageCatalog,
        font_loads: &mut crate::font_loads::FontLoads,
    ) -> Result<RewrittenSubmit, &'static str> {
        let mut stream = vec![0; HEADER];
        let mut used_fonts = Vec::new();
        let mut used_images = Vec::new();
        let mut count = 0u32;
        for command in self.commands {
            match command {
                Command::Copy(bytes) => stream.extend_from_slice(bytes),
                Command::ImageElement { source, rects } => {
                    let Some(key) = document
                        .image_src(source)
                        .filter(|src| !src.trim().is_empty())
                        .and_then(|src| cascade.resources().resolve(src))
                        .and_then(|url| images.decoded_element_key(&url))
                    else {
                        continue;
                    };
                    let wire_id = u32::try_from(key.0).map_err(|_| "canvas image key overflow")?;
                    let mut payload = [0; 72];
                    payload[4..8].copy_from_slice(&wire_id.to_le_bytes());
                    payload[8..].copy_from_slice(rects);
                    append(&mut stream, DRAW_IMAGE, &payload)?;
                    used_images.push(key);
                }
                Command::Text(text) => {
                    let Some(css) = cascade.canvas_font(document, Some(canvas), &text.font) else {
                        continue;
                    };
                    let _ = font_loads.start_matching(&css, &text.text, cascade.resources());
                    let direction = text.direction.unwrap_or_else(|| {
                        if *css.get_direction() == style::computed_values::direction::T::Rtl {
                            Direction::Rtl
                        } else {
                            Direction::Ltr
                        }
                    });
                    let runs = fonts.shape_canvas(
                        CanvasText {
                            text: &text.text,
                            origin: text.origin,
                            max_width: text.max_width,
                            align: text.align,
                            baseline: text.baseline,
                            direction,
                        },
                        &css,
                    );
                    if runs.is_empty() {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    bytes.extend(0u32.to_le_bytes());
                    bytes.extend(
                        u32::try_from(runs.len())
                            .map_err(|_| "too many canvas shaped runs")?
                            .to_le_bytes(),
                    );
                    for run in runs {
                        used_fonts.push(run.font);
                        bytes.extend(run.font.0.to_le_bytes());
                        bytes.extend(
                            u32::try_from(run.glyphs.len())
                                .map_err(|_| "too many canvas glyphs")?
                                .to_le_bytes(),
                        );
                        for (glyph, [x, y]) in run.glyphs {
                            bytes.extend(glyph.to_le_bytes());
                            bytes.extend(x.to_le_bytes());
                            bytes.extend(y.to_le_bytes());
                        }
                    }
                    append(&mut stream, SHAPED, &bytes)?;
                }
                Command::Filter(text) => {
                    let bytes =
                        if let Some(css) = cascade.canvas_filter(document, Some(canvas), &text) {
                            filter_wire(&css)
                        } else if cascade.canvas_filter_uses_url(document, &text) {
                            unsupported_url_filter()
                        } else {
                            continue;
                        };
                    append(&mut stream, SET_FILTER, &bytes)?;
                }
                Command::IgnoreFilter => continue,
            }
            count += 1;
        }
        let len = u32::try_from(stream.len()).map_err(|_| "canvas output too large")?;
        stream[..4].copy_from_slice(b"JCV1");
        stream[4..8].copy_from_slice(&len.to_le_bytes());
        stream[8..12].copy_from_slice(&count.to_le_bytes());
        Ok(RewrittenSubmit {
            stream: stream.into(),
            fonts: used_fonts,
            images: used_images,
        })
    }
}

fn filter_wire(css: &style::properties::ComputedValues) -> Vec<u8> {
    use style::values::computed::effects::Filter as Css;

    let filters = &css.get_effects().filter.0;
    if filters.iter().any(|filter| matches!(filter, Css::Url(_))) {
        return unsupported_url_filter();
    }
    let mut payload = (filters.len() as u32).to_le_bytes().to_vec();
    for filter in filters.iter() {
        let (kind, amount): (u32, f32) = match filter {
            Css::Blur(value) => (0, value.0.px()),
            Css::Brightness(value) => (1, value.0),
            Css::Contrast(value) => (2, value.0),
            Css::DropShadow(value) => {
                payload.extend(3u32.to_le_bytes());
                for length in [
                    value.horizontal.px(),
                    value.vertical.px(),
                    value.blur.0.px(),
                ] {
                    payload.extend(length.to_le_bytes());
                }
                payload.extend(canvas_rgba8(
                    &value.color.resolve_to_absolute(css.get_color()),
                ));
                continue;
            }
            Css::Grayscale(value) => (4, value.0),
            Css::HueRotate(value) => (5, value.degrees()),
            Css::Invert(value) => (6, value.0),
            Css::Opacity(value) => (7, value.0),
            Css::Saturate(value) => (8, value.0),
            Css::Sepia(value) => (9, value.0),
            Css::Url(_) => unreachable!("URL filters are handled above"),
        };
        payload.extend(kind.to_le_bytes());
        payload.extend(amount.to_le_bytes());
    }
    payload
}

fn unsupported_url_filter() -> Vec<u8> {
    static WARN: Once = Once::new();
    WARN.call_once(|| eprintln!("CSS warning: Canvas url() filters are unavailable"));
    vec![0; 4]
}

fn append(stream: &mut Vec<u8>, op: u8, payload: &[u8]) -> Result<(), &'static str> {
    let len = u32::try_from(payload.len()).map_err(|_| "canvas command too large")?;
    stream.extend([op, 0, 0, 0]);
    stream.extend(len.to_le_bytes());
    stream.extend_from_slice(payload);
    stream.resize(stream.len() + (8 - payload.len() % 8) % 8, 0);
    Ok(())
}

pub(crate) fn parse_filter(
    document: &Document,
    cascade: &Cascade<'_>,
    canvas: Option<NodeId>,
    text: &[u16],
) -> Option<Box<[Filter]>> {
    let css = cascade.canvas_filter(document, canvas, text)?;
    let values = &css.get_effects().filter.0;
    let filters = crate::paint::effects::filters(values, css.get_color());
    (filters.len() == values.len()).then_some(filters)
}
