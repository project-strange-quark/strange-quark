// SPDX-License-Identifier: MIT

use squark_types::{
    Size,
    fragment::{Clip, ClipShape, Edges, FillRule, GeometryBox, LengthPercentage, Position2D},
};
use style::values::{
    computed::{
        Position,
        basic_shape::{BasicShape, ClipPath, FillRule as CssFillRule, ShapeRadius},
    },
    generics::{
        basic_shape::{ShapeBox, ShapeGeometryBox},
        position::GenericPositionOrAuto,
    },
};

use super::{length_percentage, position2d, radius};

pub(super) fn clip(css: &ClipPath, border: Size, padding: Size, content: Size) -> Option<Clip> {
    let (shape, geometry) = match css {
        ClipPath::Shape(shape, geometry) => (Some(shape.as_ref()), *geometry),
        ClipPath::Box(geometry) => (None, *geometry),
        ClipPath::None | ClipPath::Url(_) => return None,
    };
    let (reference_box, size) = match geometry {
        ShapeGeometryBox::FillBox => (GeometryBox::Fill, content),
        ShapeGeometryBox::StrokeBox => (GeometryBox::Stroke, border),
        ShapeGeometryBox::ViewBox => (GeometryBox::View, border),
        ShapeGeometryBox::ShapeBox(ShapeBox::PaddingBox) => (GeometryBox::Padding, padding),
        ShapeGeometryBox::ShapeBox(ShapeBox::ContentBox) => (GeometryBox::Content, content),
        ShapeGeometryBox::ElementDependent | ShapeGeometryBox::ShapeBox(_) => {
            (GeometryBox::Border, border)
        }
    };
    let shape = match shape {
        None => ClipShape::Box,
        Some(BasicShape::Rect(inset)) => ClipShape::Inset {
            edges: Edges {
                top: length_percentage(&inset.rect.0),
                right: length_percentage(&inset.rect.1),
                bottom: length_percentage(&inset.rect.2),
                left: length_percentage(&inset.rect.3),
            },
            radii: [
                radius(&inset.round.top_left),
                radius(&inset.round.top_right),
                radius(&inset.round.bottom_right),
                radius(&inset.round.bottom_left),
            ],
        },
        Some(BasicShape::Circle(circle)) => {
            let center = center(&circle.position);
            let [x, y] = distances(center, size);
            let extent = match circle.radius {
                ShapeRadius::Length(ref length) => {
                    let basis = size.width.hypot(size.height) * std::f32::consts::FRAC_1_SQRT_2;
                    resolve(length_percentage(&length.0), basis)
                }
                ShapeRadius::ClosestSide | ShapeRadius::ClosestCorner => x[0].min(y[0]),
                ShapeRadius::FarthestSide | ShapeRadius::FarthestCorner => x[1].max(y[1]),
            };
            let radius = LengthPercentage {
                px: extent,
                fraction: 0.0,
            };
            ClipShape::Ellipse {
                center,
                radii: Position2D {
                    x: radius,
                    y: radius,
                },
            }
        }
        Some(BasicShape::Ellipse(ellipse)) => {
            let center = center(&ellipse.position);
            let [x, y] = distances(center, size);
            let axis = |radius: &ShapeRadius, [near, far]: [f32; 2]| match radius {
                ShapeRadius::Length(length) => length_percentage(&length.0),
                ShapeRadius::ClosestSide | ShapeRadius::ClosestCorner => LengthPercentage {
                    px: near,
                    fraction: 0.0,
                },
                ShapeRadius::FarthestSide | ShapeRadius::FarthestCorner => LengthPercentage {
                    px: far,
                    fraction: 0.0,
                },
            };
            ClipShape::Ellipse {
                center,
                radii: Position2D {
                    x: axis(&ellipse.semiaxis_x, x),
                    y: axis(&ellipse.semiaxis_y, y),
                },
            }
        }
        Some(BasicShape::Polygon(polygon)) => ClipShape::Polygon {
            fill_rule: match polygon.fill {
                CssFillRule::Nonzero => FillRule::NonZero,
                CssFillRule::Evenodd => FillRule::EvenOdd,
            },
            points: polygon
                .coordinates
                .iter()
                .map(|point| Position2D {
                    x: length_percentage(&point.0),
                    y: length_percentage(&point.1),
                })
                .collect(),
        },
        Some(BasicShape::PathOrShape(_)) => return None,
    };
    Some(Clip {
        reference_box,
        shape,
    })
}

fn resolve(value: LengthPercentage, basis: f32) -> f32 {
    value.px + value.fraction * basis
}

fn center(position: &GenericPositionOrAuto<Position>) -> Position2D {
    match position {
        GenericPositionOrAuto::Position(position) => position2d(position),
        GenericPositionOrAuto::Auto => {
            let half = LengthPercentage {
                px: 0.0,
                fraction: 0.5,
            };
            Position2D { x: half, y: half }
        }
    }
}

fn distances(center: Position2D, size: Size) -> [[f32; 2]; 2] {
    let sides = |center, length| {
        let first = resolve(center, length).abs();
        let second = (length - resolve(center, length)).abs();
        [first.min(second), first.max(second)]
    };
    [sides(center.x, size.width), sides(center.y, size.height)]
}
