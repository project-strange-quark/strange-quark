// SPDX-License-Identifier: MIT

use std::f32::consts::TAU;

use squark_types::{Point, Rect, Size, fragment as css};

use crate::shared::{color, color_space};
use crate::{Gradient, GradientGeometry, HueInterpolation, resolve_length, resolve_position};

pub fn resolve_gradient(value: &css::Gradient, rect: Rect) -> Gradient {
    let width = rect.size.width;
    let height = rect.size.height;
    let absolute = |point: Point| Point {
        x: rect.origin.x + point.x,
        y: rect.origin.y + point.y,
    };
    let mid = Point {
        x: width / 2.0,
        y: height / 2.0,
    };
    let line = |x: f32, y: f32| {
        let length = width * x.abs() + height * y.abs();
        let delta = Point {
            x: x * length / 2.0,
            y: y * length / 2.0,
        };
        (
            GradientGeometry::Linear {
                start: absolute(Point {
                    x: mid.x - delta.x,
                    y: mid.y - delta.y,
                }),
                end: absolute(Point {
                    x: mid.x + delta.x,
                    y: mid.y + delta.y,
                }),
            },
            length,
        )
    };

    let (geometry, reference) = match value.geometry {
        css::GradientGeometry::Linear {
            angle_radians: angle,
        } => line(angle.sin(), -angle.cos()),
        css::GradientGeometry::LinearTo {
            horizontal: x,
            vertical: y,
        } if x != 0 && y != 0 => {
            let diagonal = width.hypot(height).max(f32::EPSILON);
            line(
                f32::from(x) * height / diagonal,
                f32::from(y) * width / diagonal,
            )
        }
        css::GradientGeometry::LinearTo {
            horizontal: x,
            vertical: y,
        } => line(f32::from(x), f32::from(y)),
        css::GradientGeometry::Radial {
            center,
            extent,
            circle,
        } => {
            let center = resolve_position(center, rect.size);
            let radii = radial_radii(center, rect.size, extent, circle);
            (
                GradientGeometry::Radial {
                    center: absolute(center),
                    radii,
                },
                radii.width,
            )
        }
        css::GradientGeometry::Conic {
            center,
            from_radians,
        } => {
            let center = absolute(resolve_position(center, rect.size));
            (
                GradientGeometry::Conic {
                    center,
                    start_angle: from_radians,
                },
                TAU,
            )
        }
    };
    let (stops, hints) = stops(&value.stops, reference);
    Gradient {
        geometry,
        interpolation: color_space(value.space),
        hue: match value.hue {
            css::HueInterpolation::Shorter => HueInterpolation::Shorter,
            css::HueInterpolation::Longer => HueInterpolation::Longer,
            css::HueInterpolation::Increasing => HueInterpolation::Increasing,
            css::HueInterpolation::Decreasing => HueInterpolation::Decreasing,
        },
        stops,
        hints,
        repeating: value.repeating,
    }
}

fn radial_radii(center: Point, size: Size, extent: css::RadialExtent, circle: bool) -> Size {
    use css::RadialExtent as R;
    if let R::Explicit(radii) = extent {
        let radii = resolve_position(radii, size);
        return Size {
            width: radii.x.max(0.0),
            height: if circle { radii.x } else { radii.y }.max(0.0),
        };
    }
    let xs = [center.x.abs(), (size.width - center.x).abs()];
    let ys = [center.y.abs(), (size.height - center.y).abs()];
    let closest = matches!(extent, R::ClosestSide | R::ClosestCorner);
    let corner = matches!(extent, R::ClosestCorner | R::FarthestCorner);
    let pick = if closest { f32::min } else { f32::max };
    let x = pick(xs[0], xs[1]);
    let y = pick(ys[0], ys[1]);
    if circle {
        let radius = if corner { x.hypot(y) } else { pick(x, y) };
        Size {
            width: radius,
            height: radius,
        }
    } else {
        let factor = if corner {
            std::f32::consts::SQRT_2
        } else {
            1.0
        };
        Size {
            width: x * factor,
            height: y * factor,
        }
    }
}

type Stops = Box<[(f32, crate::PaintColor)]>;
type Hints = Box<[Option<f32>]>;

fn stops(values: &[css::GradientStop], reference: f32) -> (Stops, Hints) {
    let normalized = |position| match position {
        css::StopPosition::Length(value) => {
            resolve_length(value, reference) / reference.max(f32::EPSILON)
        }
        css::StopPosition::AngleRadians(value) => value / TAU,
    };
    let mut colors: Vec<(Option<f32>, _)> = Vec::new();
    let mut hints = Vec::new();
    for value in values {
        match *value {
            css::GradientStop::Color {
                position,
                color: value,
            } => {
                colors.push((position.map(normalized), color(value)));
                hints.push(None);
            }
            css::GradientStop::Hint(position) => {
                if let Some(last) = hints.last_mut() {
                    *last = Some(normalized(position));
                }
            }
        }
    }
    if let Some(first) = colors.first_mut() {
        first.0.get_or_insert(0.0);
    }
    if let Some(last) = colors.last_mut() {
        last.0.get_or_insert(1.0);
    }
    let mut previous = f32::NEG_INFINITY;
    for (position, _) in &mut colors {
        if let Some(value) = position {
            *value = value.max(previous);
            previous = *value;
        }
    }
    let mut i = 0;
    while i + 1 < colors.len() {
        let start = colors[i].0.unwrap();
        let relative_end = colors[i + 1..]
            .iter()
            .position(|(position, _)| position.is_some())
            .unwrap();
        let end = colors[i + 1 + relative_end].0.unwrap();
        for j in 1..=relative_end {
            colors[i + j].0 = Some(start + (end - start) * j as f32 / (relative_end + 1) as f32);
        }
        i += 1 + relative_end;
    }
    (
        colors
            .into_iter()
            .map(|(pos, color)| (pos.unwrap(), color))
            .collect(),
        hints.into_boxed_slice(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_stops_share_the_space_between_fixed_stops() {
        let color = css::PaintColor::BLACK;
        let color_at = |value| css::GradientStop::Color {
            position: value,
            color,
        };
        let position =
            |fraction| css::StopPosition::Length(css::LengthPercentage { px: 0.0, fraction });
        let (stops, hints) = stops(
            &[
                color_at(None),
                color_at(None),
                css::GradientStop::Hint(position(0.6)),
                color_at(Some(position(0.8))),
                color_at(Some(position(0.5))),
                color_at(None),
            ],
            100.0,
        );
        let positions: Vec<_> = stops.iter().map(|stop| stop.0).collect();
        assert_eq!(positions, [0.0, 0.4, 0.8, 0.8, 1.0]);
        assert_eq!(hints[1], Some(0.6));
    }
}
