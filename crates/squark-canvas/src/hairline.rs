// SPDX-License-Identifier: MIT

use std::sync::Arc;

use vello::{
    kurbo::{Affine, Cap, CubicBez, ParamCurve, PathEl, Point, QuadBez, Rect, Shape, Stroke},
    peniko::{Blob, Brush, ImageAlphaType, ImageData, ImageFormat},
};

/// Anti-aliased butt-cap hairlines use device-space coverage along the minor
/// axis. Widening them to a filled stroke adds too much ink to diagonal lines.
pub(crate) fn rasterize(
    shape: &impl Shape,
    outline: &Stroke,
    transform: Affine,
    brush: &Brush,
    alpha: f32,
    size: [u32; 2],
) -> Option<(ImageData, Affine)> {
    let Brush::Solid(color) = brush else {
        return None;
    };
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let width_x = outline.width * a.hypot(b);
    let width_y = outline.width * c.hypot(d);
    if !transform.is_finite()
        || width_x > 1.0
        || width_y > 1.0
        || outline.start_cap != Cap::Butt
        || outline.end_cap != Cap::Butt
        || !outline.dash_pattern.is_empty()
    {
        return None;
    }
    let [width, height] = size.map(f64::from);
    let bounds = transform
        .transform_rect_bbox(shape.bounding_box())
        .inflate(1.0, 1.0);
    if !bounds.is_finite() {
        return None;
    }
    let rect = Rect::new(
        bounds.x0.floor(),
        bounds.y0.floor(),
        bounds.x1.ceil(),
        bounds.y1.ceil(),
    )
    .intersect(Rect::new(0.0, 0.0, width, height));
    let width = rect.width() as u32;
    let height = rect.height() as u32;
    if width == 0 || height == 0 {
        return None;
    }

    let rgba = color
        .multiply_alpha(alpha * ((width_x + width_y) * 0.5) as f32)
        .to_rgba8();
    let [red, green, blue, opacity] = rgba.to_u8_array();
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(len).ok()?;
    pixels.resize(len, 0u8);
    let mut previous = Point::ORIGIN;
    let mut start = Point::ORIGIN;
    let render_transform = Affine::translate((-rect.x0, -rect.y0)) * transform;
    let mut line = |from: Point, to: Point| {
        // Canvas hairlines place segment endpoints on a 1/64px grid.
        let snap = |p: Point| Point::new((p.x * 64.0).floor() / 64.0, (p.y * 64.0).floor() / 64.0);
        let (from, to) = (snap(from), snap(to));
        if from == to {
            return;
        }
        antialias(from, to, [width, height], |x, y, cover| {
            let at = (y as usize * width as usize + x as usize) * 4;
            let incoming = (cover * f64::from(opacity)).round() as u32;
            if incoming == 0 {
                return;
            }
            let existing = u32::from(pixels[at + 3]);
            let opacity = incoming + (existing * (255 - incoming) + 127) / 255;
            pixels[at..at + 4].copy_from_slice(&[red, green, blue, opacity as u8]);
        });
    };

    let elements = shape
        .path_elements(0.1)
        .map(|element| render_transform * element);
    for element in elements {
        match element {
            PathEl::MoveTo(p) => start = p,
            PathEl::LineTo(p) => line(previous, p),
            PathEl::QuadTo(control, end) => {
                let deviation = control - previous.midpoint(end);
                let distance = deviation.x.abs().max(deviation.y.abs());
                curve_lines(
                    QuadBez::new(previous, control, end),
                    distance,
                    0.5,
                    5,
                    &mut line,
                );
            }
            PathEl::CurveTo(first, second, end) => {
                let a = first - previous.lerp(end, 1.0 / 3.0);
                let b = second - previous.lerp(end, 2.0 / 3.0);
                let distance = a.x.abs().max(a.y.abs()).max(b.x.abs()).max(b.y.abs());
                curve_lines(
                    CubicBez::new(previous, first, second, end),
                    distance,
                    0.125,
                    9,
                    &mut line,
                );
            }
            PathEl::ClosePath => line(previous, start),
        }
        previous = element.end_point().unwrap_or(start);
    }
    let image = ImageData {
        data: Blob::new(Arc::new(pixels)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    };
    Some((image, Affine::translate((rect.x0, rect.y0))))
}

fn curve_lines(
    curve: impl ParamCurve,
    distance: f64,
    mut tolerance: f64,
    limit: u32,
    line: &mut impl FnMut(Point, Point),
) {
    // Halving a curve interval quarters its deviation from the chord.
    // Use evenly spaced samples so adjacent portions have the same coverage.
    let mut count = 1;
    for _ in 0..limit {
        if distance < tolerance {
            break;
        }
        count *= 2;
        tolerance *= 4.0;
    }
    let mut previous = curve.start();
    for i in 1..=count {
        let next = curve.eval(f64::from(i) / f64::from(count));
        line(previous, next);
        previous = next;
    }
}

fn antialias(from: Point, to: Point, size: [u32; 2], mut plot: impl FnMut(u32, u32, f64)) {
    let vertical = (to.y - from.y).abs() > (to.x - from.x).abs();
    let axes = |p: Point| if vertical { [p.y, p.x] } else { [p.x, p.y] };
    let mut from = axes(from);
    let mut to = axes(to);
    if from[0] > to[0] {
        std::mem::swap(&mut from, &mut to);
    }
    let [major, minor] = if vertical { [size[1], size[0]] } else { size };
    let slope = (to[1] - from[1]) / (to[0] - from[0]);
    let first = from[0].floor().clamp(0.0, f64::from(major)) as u32;
    let last = to[0].ceil().clamp(0.0, f64::from(major)) as u32;
    for col in first..last {
        let col_x = f64::from(col);
        let cap = (col_x + 1.0).min(to[0]) - col_x.max(from[0]);
        let row = from[1] + (col_x + 0.5 - from[0]) * slope - 0.5;
        let lo = row.floor();
        let mix = row - lo;
        for (row, weight) in [(lo, 1.0 - mix), (lo + 1.0, mix)] {
            if row >= 0.0 && row < f64::from(minor) {
                let row = row as u32;
                let (x, y) = if vertical { (row, col) } else { (col, row) };
                plot(x, y, cap * weight);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_a_diagonal_on_its_major_axis() {
        let mut samples = Vec::new();
        antialias(
            Point::new(0.5, 0.5),
            Point::new(3.5, 3.5),
            [4, 4],
            |x, y, a| {
                if a > 0.0 {
                    samples.push((x, y, a));
                }
            },
        );
        assert_eq!(
            samples,
            [(0, 0, 0.5), (1, 1, 1.0), (2, 2, 1.0), (3, 3, 0.5)]
        );
    }

    #[test]
    fn interpolates_a_fractional_minor_axis_and_clips_endpoints() {
        let mut samples = Vec::new();
        antialias(
            Point::new(-2.0, 1.25),
            Point::new(1.5, 1.25),
            [2, 3],
            |x, y, a| {
                samples.push((x, y, a));
            },
        );
        assert_eq!(
            samples,
            [(0, 0, 0.25), (0, 1, 0.75), (1, 0, 0.125), (1, 1, 0.375)]
        );
    }
}
