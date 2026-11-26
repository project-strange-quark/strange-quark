// SPDX-License-Identifier: MIT

use std::f64::consts::TAU;

use squark_types::{
    NodeId,
    fragment::{Filter, PaintColor, Shadow},
    render::FontInstanceId,
};
use vello::{
    Glyph,
    kurbo::{Affine, Arc, BezPath, Cap, Join, PathEl, Point, Rect, Stroke},
    peniko::{Brush, Color, Compose, Fill, Gradient, ImageQuality},
};

use crate::Source;

type Result<T> = std::result::Result<T, &'static str>;

#[derive(Debug)]
pub(crate) enum Command<'a> {
    Save,
    Restore,
    Begin,
    Path(PathOp),
    Transform(Affine),
    DrawPath(Draw, u32, Fill),
    DrawRect(Draw, Rect),
    DefinePath(u32, BezPath),
    SetPaint(bool, u32),
    DefinePaint(u32, Brush),
    Sampling(f32, Compose, ImageQuality),
    Stroke(Stroke),
    Image(Source, Rect, Rect),
    Text(bool, Vec<Run>),
    Pixels([i32; 2], [u32; 2], &'a [u8]),
    Filters(Vec<Filter>),
    IgnoreFilter,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Draw {
    Fill,
    Stroke,
    Clip,
    Clear,
}

#[derive(Debug)]
pub(crate) struct Run {
    pub font: FontInstanceId,
    pub glyphs: Vec<Glyph>,
}

#[derive(Debug)]
pub(crate) enum PathOp {
    Move(Point),
    Line(Point),
    Quad(Point, Point),
    Cubic(Point, Point, Point),
    Close,
    Ellipse(Arc),
}

impl PathOp {
    pub fn append(self, path: &mut BezPath, matrix: Affine) {
        let point = |p| matrix * p;
        match self {
            Self::Move(p) => path.move_to(point(p)),
            Self::Line(p) => line_or_move(path, point(p)),
            Self::Quad(a, b) => {
                if path.current_position().is_none() {
                    path.move_to(point(a));
                }
                path.quad_to(point(a), point(b));
            }
            Self::Cubic(a, b, c) => {
                if path.current_position().is_none() {
                    path.move_to(point(a));
                }
                path.curve_to(point(a), point(b), point(c));
            }
            Self::Close => {
                if path.current_position().is_some() {
                    path.close_path();
                }
            }
            Self::Ellipse(arc) => {
                let on_ellipse = Point::new(
                    arc.radii.x * arc.start_angle.cos(),
                    arc.radii.y * arc.start_angle.sin(),
                );
                let start =
                    arc.center + (Affine::rotate(arc.x_rotation) * on_ellipse - Point::ORIGIN);
                line_or_move(path, point(start));
                if arc.sweep_angle != 0.0 && arc.radii.x > 0.0 && arc.radii.y > 0.0 {
                    for elem in arc.append_iter(0.01) {
                        if let PathEl::CurveTo(a, b, c) = elem {
                            path.curve_to(point(a), point(b), point(c));
                        }
                    }
                }
            }
        }
    }
}

fn line_or_move(path: &mut BezPath, point: Point) {
    match path.current_position() {
        None => path.move_to(point),
        Some(previous) if previous != point => path.line_to(point),
        Some(_) => {}
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Command<'_>>> {
    let mut reader = Reader(bytes);
    if reader.take(4)? != b"JCV1" || reader.word()? as usize != bytes.len() {
        return Err("invalid JCV1 header");
    }
    let count = reader.count(8)?;
    let mut commands = Vec::with_capacity(count);
    for _ in 0..count {
        let (op, mut payload) = reader.record()?;
        let command = payload.command(op)?;
        payload.end()?;
        commands.push(command);
    }
    reader.end()?;
    Ok(commands)
}

#[derive(Debug)]
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let (value, rest) = self
            .0
            .split_at_checked(size)
            .ok_or("truncated JCV1 field")?;
        self.0 = rest;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn number(&mut self) -> Result<f64> {
        let value = f64::from_le_bytes(self.array()?);
        (value as f32)
            .is_finite()
            .then_some(value)
            .ok_or("nonfinite JCV1 geometry")
    }

    fn float(&mut self) -> Result<f32> {
        let value = f32::from_le_bytes(self.array()?);
        value
            .is_finite()
            .then_some(value)
            .ok_or("nonfinite JCV1 filter amount")
    }

    fn count(&mut self, minimum_size: usize) -> Result<usize> {
        let count = self.word()? as usize;
        (count <= self.0.len() / minimum_size)
            .then_some(count)
            .ok_or("invalid JCV1 count")
    }

    fn enum_value<T: Copy>(&mut self, values: &[T]) -> Result<T> {
        values
            .get(self.word()? as usize)
            .copied()
            .ok_or("invalid JCV1 enumeration")
    }

    fn id(&mut self) -> Result<u32> {
        let id = self.word()?;
        (id != 0).then_some(id).ok_or("zero JCV1 definition ID")
    }

    fn point(&mut self) -> Result<Point> {
        Ok(Point::new(self.number()?, self.number()?))
    }

    fn rect(&mut self) -> Result<Rect> {
        let origin = self.point()?;
        let end = origin + (self.number()?, self.number()?);
        Ok(Rect::from_points(origin, end))
    }

    fn color(&mut self) -> Result<Color> {
        let [r, g, b, a] = self.array()?;
        Ok(Color::from_rgba8(r, g, b, a))
    }

    fn record(&mut self) -> Result<(u8, Reader<'a>)> {
        let [opcode, a, b, c] = self.array()?;
        if [a, b, c] != [0; 3] {
            return Err("reserved JCV1 header bits");
        }
        let size = self.word()? as usize;
        let payload = self.take(size)?;
        if self.take((8 - size % 8) % 8)?.iter().any(|&byte| byte != 0) {
            return Err("nonzero JCV1 padding");
        }
        Ok((opcode, Reader(payload)))
    }

    fn end(&self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("unexpected trailing JCV1 bytes")
        }
    }

    fn path(&mut self, op: u8) -> Result<PathOp> {
        Ok(match op {
            4 => PathOp::Move(self.point()?),
            5 => PathOp::Line(self.point()?),
            6 => PathOp::Quad(self.point()?, self.point()?),
            7 => PathOp::Cubic(self.point()?, self.point()?, self.point()?),
            8 => PathOp::Close,
            9 => {
                let center = self.point()?;
                let radii = (self.number()?, self.number()?);
                if radii.0 < 0.0 || radii.1 < 0.0 {
                    return Err("negative JCV1 ellipse radius");
                }
                let rotation = self.number()?.rem_euclid(TAU);
                let start = self.number()?;
                let end = self.number()?;
                let direction = self.enum_value(&[1.0, -1.0])?;
                let delta = (end - start) * direction;
                let wrapped = delta.rem_euclid(TAU);
                // Chromium preserves a complete turn when an opposite
                // sweep lands on the starting angle, e.g. arc(0, 2π, true).
                let sweep = if delta >= TAU || delta < 0.0 && wrapped == 0.0 {
                    TAU
                } else {
                    wrapped
                };
                PathOp::Ellipse(Arc::new(
                    center,
                    radii,
                    start.rem_euclid(TAU),
                    direction * sweep,
                    rotation,
                ))
            }
            _ => return Err("unknown JCV1 path opcode"),
        })
    }

    fn paint(&mut self) -> Result<Brush> {
        let mut gradient = match self.word()? {
            0 => return Ok(self.color()?.into()),
            1 => Gradient::new_linear(self.point()?, self.point()?),
            2 => {
                let start = self.point()?;
                let r0 = self.number()? as f32;
                let end = self.point()?;
                let r1 = self.number()? as f32;
                if r0 < 0.0 || r1 < 0.0 {
                    return Err("negative JCV1 gradient radius");
                }
                Gradient::new_two_point_radial(start, r0, end, r1)
            }
            3 => {
                let center = self.point()?;
                let start = self.number()?.rem_euclid(TAU) as f32;
                Gradient::new_sweep(center, start, start + std::f32::consts::TAU)
            }
            _ => return Err("unknown JCV1 paint kind"),
        };
        let count = self.count(12)?;
        if count == 0 {
            return Ok(Color::TRANSPARENT.into());
        }
        let mut stops = Vec::with_capacity(count);
        for _ in 0..count {
            let offset = self.number()?;
            if !(0.0..=1.0).contains(&offset) {
                return Err("invalid JCV1 gradient stop");
            }
            stops.push((offset as f32, self.color()?));
        }
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        gradient = gradient.with_stops(stops.as_slice());
        Ok(gradient.into())
    }

    fn stroke(&mut self) -> Result<Stroke> {
        let width = self.number()?;
        let miter_limit = self.number()?;
        let dash_offset = self.number()?;
        if width <= 0.0 || miter_limit <= 0.0 {
            return Err("invalid JCV1 stroke size");
        }
        let cap = self.enum_value(&[Cap::Butt, Cap::Round, Cap::Square])?;
        let join = self.enum_value(&[Join::Miter, Join::Round, Join::Bevel])?;
        let count = self.count(8)?;
        let mut dash_pattern = (0..count)
            .map(|_| self.number())
            .collect::<Result<Vec<_>>>()?;
        if dash_pattern.iter().any(|&dash| dash < 0.0) {
            return Err("negative JCV1 dash length");
        }
        if dash_pattern.iter().all(|&dash| dash == 0.0) {
            dash_pattern.clear();
        }
        if dash_pattern.len() % 2 != 0 {
            dash_pattern.extend_from_within(..);
        }
        Ok(Stroke {
            width,
            join,
            miter_limit,
            start_cap: cap,
            end_cap: cap,
            dash_pattern: dash_pattern.into(),
            dash_offset,
        })
    }

    fn text(&mut self) -> Result<Command<'a>> {
        let stroke = self.enum_value(&[false, true])?;
        let count = self.count(12)?;
        let mut runs = Vec::with_capacity(count);
        for _ in 0..count {
            let font = FontInstanceId(u64::from_le_bytes(self.array()?));
            let count = self.count(20)?;
            let mut glyphs = Vec::with_capacity(count);
            for _ in 0..count {
                glyphs.push(Glyph {
                    id: self.word()?,
                    x: self.number()? as f32,
                    y: self.number()? as f32,
                });
            }
            runs.push(Run { font, glyphs });
        }
        Ok(Command::Text(stroke, runs))
    }

    fn filters(&mut self) -> Result<Vec<Filter>> {
        let count = self.count(8)?;
        let mut filters = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = self.word()?;
            let value = self.float()?;
            filters.push(match kind {
                0 => Filter::Blur(value.max(0.0)),
                1 => Filter::Brightness(value),
                2 => Filter::Contrast(value),
                3 => {
                    let y = self.float()?;
                    let blur = self.float()?.max(0.0);
                    let [r, g, b, a] = self.color()?.components;
                    Filter::DropShadow(Shadow {
                        offset: squark_types::Point { x: value, y },
                        blur,
                        spread: 0.0,
                        color: PaintColor::srgb(r, g, b, a),
                        inset: false,
                    })
                }
                4 => Filter::Grayscale(value),
                5 => Filter::HueRotate(value.rem_euclid(360.0).to_radians()),
                6 => Filter::Invert(value),
                7 => Filter::Opacity(value),
                8 => Filter::Saturate(value),
                9 => Filter::Sepia(value),
                _ => return Err("unknown JCV1 filter kind"),
            });
        }
        self.end()?;
        Ok(filters)
    }

    fn command(&mut self, op: u8) -> Result<Command<'a>> {
        Ok(match op {
            1 => Command::Save,
            2 => Command::Restore,
            3 => Command::Begin,
            4..=9 => Command::Path(self.path(op)?),
            10 => {
                let mut coeffs = [0.0; 6];
                for value in &mut coeffs {
                    *value = self.number()?;
                }
                Command::Transform(Affine::new(coeffs))
            }
            11..=13 => {
                let draw = [Draw::Fill, Draw::Stroke, Draw::Clip][op as usize - 11];
                let id = self.word()?;
                let rule = self.enum_value(&[Fill::NonZero, Fill::EvenOdd])?;
                if draw == Draw::Stroke && rule != Fill::NonZero {
                    return Err("invalid JCV1 stroke fill rule");
                }
                Command::DrawPath(draw, id, rule)
            }
            14..=16 => Command::DrawRect(
                [Draw::Fill, Draw::Stroke, Draw::Clear][op as usize - 14],
                self.rect()?,
            ),
            17 => {
                let id = self.id()?;
                let len = self.word()? as usize;
                let mut records = Reader(self.take(len)?);
                let mut path = BezPath::new();
                while !records.0.is_empty() {
                    let (op, mut fields) = records.record()?;
                    fields.path(op)?.append(&mut path, Affine::IDENTITY);
                    fields.end()?;
                }
                Command::DefinePath(id, path)
            }
            18 | 19 => Command::SetPaint(op == 19, self.id()?),
            20 => Command::DefinePaint(self.id()?, self.paint()?),
            21 => {
                let alpha = self.number()?;
                if !(0.0..=1.0).contains(&alpha) {
                    return Err("invalid JCV1 global alpha");
                }
                let compose = self.enum_value(&[
                    Compose::SrcOver,
                    Compose::SrcIn,
                    Compose::SrcOut,
                    Compose::SrcAtop,
                    Compose::DestOver,
                    Compose::DestIn,
                    Compose::DestOut,
                    Compose::DestAtop,
                    Compose::Copy,
                    Compose::Xor,
                    Compose::Plus,
                ])?;
                let quality = self.enum_value(&[ImageQuality::Low, ImageQuality::Medium])?;
                Command::Sampling(alpha as f32, compose, quality)
            }
            22 => Command::Stroke(self.stroke()?),
            23 => {
                let canvas = self.enum_value(&[false, true])?;
                let id = self.word()?;
                let source = if canvas {
                    Source::Canvas(NodeId::from_raw(id))
                } else {
                    Source::Decoded(id)
                };
                Command::Image(source, self.rect()?, self.rect()?)
            }
            24 => self.text()?,
            25 => {
                let origin = [
                    i32::from_le_bytes(self.array()?),
                    i32::from_le_bytes(self.array()?),
                ];
                let size = [self.word()?, self.word()?];
                let bytes = 4usize
                    .checked_mul(size[0] as usize)
                    .and_then(|row| row.checked_mul(size[1] as usize))
                    .ok_or("JCV1 pixel dimensions overflow")?;
                Command::Pixels(origin, size, self.take(bytes)?)
            }
            28 => match self.filters() {
                Ok(filters) => Command::Filters(filters),
                Err(_) => {
                    self.0 = &[];
                    Command::IgnoreFilter
                }
            },
            _ => return Err("unknown JCV1 render opcode"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let aligned = payload.len().next_multiple_of(8);
        let mut bytes = b"JCV1".to_vec();
        bytes.extend(((20 + aligned) as u32).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend([opcode, 0, 0, 0]);
        bytes.extend((payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
        bytes.resize(20 + aligned, 0);
        bytes
    }

    #[test]
    fn opposite_full_turn_keeps_the_circle() {
        for (start, end, counterclockwise, expected) in [
            (0.0, TAU, 1u32, -TAU),
            (TAU, 0.0, 0, TAU),
            (0.0, 0.0, 1, 0.0),
            (0.0, TAU / 4.0, 1, -TAU * 0.75),
        ] {
            let mut payload: Vec<_> = [10.0, 10.0, 8.0, 8.0, 0.0, start, end]
                .into_iter()
                .flat_map(f64::to_le_bytes)
                .collect();
            payload.extend(counterclockwise.to_le_bytes());
            let bytes = stream(9, &payload);
            let [Command::Path(PathOp::Ellipse(arc))] = &decode(&bytes).unwrap()[..] else {
                panic!("expected an ellipse");
            };
            assert_eq!(arc.sweep_angle, expected);
        }
    }

    #[test]
    fn rejects_bad_lengths_flags_padding_and_counts() {
        let valid = stream(18, &1u32.to_le_bytes());
        assert!(matches!(
            decode(&valid).unwrap()[..],
            [Command::SetPaint(false, 1)]
        ));
        for (index, byte) in [(4, 20), (8, 2), (13, 1), (19, 1), (24, 1)] {
            let mut malformed = valid.clone();
            malformed[index] = byte;
            assert!(
                decode(&malformed).is_err(),
                "accepted malformed field at {index}"
            );
        }
        for length in 0..valid.len() {
            assert!(decode(&valid[..length]).is_err());
        }
        assert!(decode(&stream(27, &[])).is_err());
    }

    #[test]
    fn decodes_filter_values_in_css_order_and_ignores_unknown_ops() {
        let body = [
            2u32.to_le_bytes(),
            6u32.to_le_bytes(),
            0.93f32.to_le_bytes(),
            5u32.to_le_bytes(),
            180f32.to_le_bytes(),
        ]
        .concat();
        let bytes = stream(28, &body);
        let commands = decode(&bytes).unwrap();
        let [Command::Filters(filters)] = commands.as_slice() else {
            panic!("expected filters")
        };
        assert_eq!(
            filters,
            &[
                Filter::Invert(0.93),
                Filter::HueRotate(std::f32::consts::PI)
            ]
        );
        let mut body = body;
        body[12..16].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            decode(&stream(28, &body)).unwrap()[..],
            [Command::IgnoreFilter]
        ));
        assert!(
            matches!(decode(&stream(28, &[0; 4])).unwrap()[..], [Command::Filters(ref ops)] if ops.is_empty())
        );
    }
}
