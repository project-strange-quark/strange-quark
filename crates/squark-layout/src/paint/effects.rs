// SPDX-License-Identifier: MIT

use squark_types::{
    Point, Size,
    fragment::{
        AnimatedValue, BlendMode, Filter, GeometryBox, LengthPercentage, Position2D,
        PropertyValues, Shadow, Transform,
    },
};
use style::{
    color::AbsoluteColor,
    computed_values::mix_blend_mode::T as CssBlend,
    properties::{ComputedValues, animated_properties::AnimationValue},
    values::computed::{
        Angle, CSSPixelLength, LengthPercentage as CssLengthPercentage, Perspective, Rotate, Scale,
        Translate,
        effects::{Filter as CssFilter, SimpleShadow},
        transform::{Transform as CssTransform, TransformOperation as Op, TransformStyle},
    },
    values::generics::transform::get_normalized_vector_and_angle,
};

use super::{color, length_percentage, position2d};
use style::values::animated::ToAnimatedValue;

pub(super) fn project(
    css: &ComputedValues,
    border: Size,
    padding: Size,
    content: Size,
) -> PropertyValues {
    let geometry = css.get_box();
    let effects = css.get_effects();
    let origin = &geometry.transform_origin;
    let mut operations = Vec::new();
    match &geometry.translate {
        Translate::None => {}
        Translate::Translate(x, y, z) => operations.push(Op::Translate3D(x.clone(), y.clone(), *z)),
    }
    match geometry.rotate {
        Rotate::None => {}
        Rotate::Rotate(angle) => operations.push(Op::Rotate(angle)),
        Rotate::Rotate3D(x, y, z, angle) => operations.push(Op::Rotate3D(x, y, z, angle)),
    }
    if let Scale::Scale(x, y, z) = geometry.scale {
        operations.push(Op::Scale3D(x, y, z));
    }
    operations.extend(geometry.transform.0.iter().cloned());
    let transform = transform(&operations, border).unwrap_or_default();
    PropertyValues {
        transform,
        transform_origin: Position2D {
            x: length_percentage(&origin.horizontal),
            y: length_percentage(&origin.vertical),
        },
        transform_origin_z: origin.depth.px(),
        transform_box: GeometryBox::Border,
        perspective: match geometry.perspective {
            Perspective::Length(length) => Some(length.0.px().max(1.0)),
            Perspective::None => None,
        },
        perspective_origin: position2d(&geometry.perspective_origin),
        preserve_3d: geometry.transform_style == TransformStyle::Preserve3d,
        clip: super::shapes::clip(css.get_clip_path(), border, padding, content),
        opacity: effects.opacity,
        filters: filters(&effects.filter.0, css.get_color()),
        backdrop_filters: filters(&effects.backdrop_filter.0, css.get_color()),
        blend: blend(effects.mix_blend_mode),
        ..Default::default()
    }
}

fn transform(operations: &[Op], border: Size) -> Option<Box<[Transform]>> {
    if operations.is_empty() {
        Some(Box::default())
    } else {
        let reference = euclid::default::Rect::new(
            euclid::point2(CSSPixelLength::new(0.0), CSSPixelLength::new(0.0)),
            euclid::size2(
                CSSPixelLength::new(border.width),
                CSSPixelLength::new(border.height),
            ),
        );
        CssTransform::components_to_transform_3d_matrix(operations, Some(&reference))
            .map(|(matrix, _)| Box::<[Transform]>::from([Transform::Matrix(matrix.to_arrays())]))
            .ok()
    }
}

/// An animated transform list as functions the compositor can interpolate the
/// way CSS does. A rotation keeps its angle, which a matrix loses:
/// `rotate(360deg)` is the identity matrix, so a spinner built from matrices
/// would never move. Scales and skews keep their factors and angles, and
/// translations are resolved against the border box as the matrix path does.
/// Every other function becomes its matrix.
fn primitives(operations: &[Op], border: Size) -> Option<Box<[Transform]>> {
    let translate = |x: f32, y: f32, z: f32| Transform::Translate {
        xy: Position2D {
            x: LengthPercentage {
                px: x,
                fraction: 0.0,
            },
            y: LengthPercentage {
                px: y,
                fraction: 0.0,
            },
        },
        z,
    };
    let width = |value: &CssLengthPercentage| value.resolve(CSSPixelLength::new(border.width)).px();
    let height =
        |value: &CssLengthPercentage| value.resolve(CSSPixelLength::new(border.height)).px();
    let rotate = |axis: [f32; 3], angle: &Angle| Transform::Rotate {
        axis,
        angle: angle.radians(),
    };
    operations
        .iter()
        .map(|operation| {
            Some(match operation {
                Op::Translate(x, y) => translate(width(x), height(y), 0.0),
                Op::TranslateX(x) => translate(width(x), 0.0, 0.0),
                Op::TranslateY(y) => translate(0.0, height(y), 0.0),
                Op::TranslateZ(z) => translate(0.0, 0.0, z.px()),
                Op::Translate3D(x, y, z) => translate(width(x), height(y), z.px()),
                Op::Rotate(angle) | Op::RotateZ(angle) => rotate([0.0, 0.0, 1.0], angle),
                Op::RotateX(angle) => rotate([1.0, 0.0, 0.0], angle),
                Op::RotateY(angle) => rotate([0.0, 1.0, 0.0], angle),
                Op::Rotate3D(x, y, z, angle) => {
                    let (x, y, z, angle) = get_normalized_vector_and_angle(*x, *y, *z, *angle);
                    rotate([x, y, z], &angle)
                }
                Op::Scale(x, y) => Transform::Scale {
                    x: *x,
                    y: *y,
                    z: 1.0,
                },
                Op::ScaleX(x) => Transform::Scale {
                    x: *x,
                    y: 1.0,
                    z: 1.0,
                },
                Op::ScaleY(y) => Transform::Scale {
                    x: 1.0,
                    y: *y,
                    z: 1.0,
                },
                Op::ScaleZ(z) => Transform::Scale {
                    x: 1.0,
                    y: 1.0,
                    z: *z,
                },
                Op::Scale3D(x, y, z) => Transform::Scale {
                    x: *x,
                    y: *y,
                    z: *z,
                },
                Op::Skew(x, y) => Transform::Skew {
                    x: x.radians(),
                    y: y.radians(),
                },
                Op::SkewX(x) => Transform::Skew {
                    x: x.radians(),
                    y: 0.0,
                },
                Op::SkewY(y) => Transform::Skew {
                    x: 0.0,
                    y: y.radians(),
                },
                other => *transform(std::slice::from_ref(other), border)?.first()?,
            })
        })
        .collect()
}

pub(crate) fn animated_value(
    value: &AnimationValue,
    css: &ComputedValues,
    border: Size,
) -> Option<AnimatedValue> {
    match value {
        AnimationValue::Opacity(value) => Some(AnimatedValue::Opacity(*value)),
        AnimationValue::Transform(value) => {
            let geometry = css.get_box();
            if geometry.translate != Translate::None
                || geometry.rotate != Rotate::None
                || geometry.scale != Scale::None
            {
                return None;
            }
            let value = CssTransform::from_animated_value(value.clone());
            Some(AnimatedValue::Transform(primitives(&value.0, border)?))
        }
        AnimationValue::Filter(value) => {
            use style::properties::longhands::filter::computed_value::T as FilterList;
            let value = FilterList::from_animated_value(value.clone());
            let projected = filters(&value.0, css.get_color());
            (projected.len() == value.0.len()).then_some(AnimatedValue::Filter(projected))
        }
        _ => None,
    }
}

pub(super) fn box_shadows(css: &ComputedValues) -> Box<[Shadow]> {
    css.get_effects()
        .box_shadow
        .0
        .iter()
        .map(|value| Shadow {
            spread: value.spread.px(),
            inset: value.inset,
            ..shadow(&value.base, css.get_color())
        })
        .collect()
}

pub(super) fn text_shadows(css: &ComputedValues) -> Box<[Shadow]> {
    css.get_inherited_text()
        .text_shadow
        .0
        .iter()
        .map(|value| shadow(value, css.get_color()))
        .collect()
}

fn shadow(value: &SimpleShadow, current: &AbsoluteColor) -> Shadow {
    Shadow {
        offset: Point {
            x: value.horizontal.px(),
            y: value.vertical.px(),
        },
        blur: value.blur.0.px(),
        spread: 0.0,
        color: color(&value.color.resolve_to_absolute(current)),
        inset: false,
    }
}

pub(crate) fn filters(values: &[CssFilter], current: &AbsoluteColor) -> Box<[Filter]> {
    values
        .iter()
        .filter_map(|value| {
            Some(match value {
                CssFilter::Blur(v) => Filter::Blur(v.0.px()),
                CssFilter::Brightness(v) => Filter::Brightness(v.0),
                CssFilter::Contrast(v) => Filter::Contrast(v.0),
                CssFilter::DropShadow(v) => Filter::DropShadow(shadow(v, current)),
                CssFilter::Grayscale(v) => Filter::Grayscale(v.0),
                CssFilter::HueRotate(v) => Filter::HueRotate(v.radians()),
                CssFilter::Invert(v) => Filter::Invert(v.0),
                CssFilter::Opacity(v) => Filter::Opacity(v.0),
                CssFilter::Saturate(v) => Filter::Saturate(v.0),
                CssFilter::Sepia(v) => Filter::Sepia(v.0),
                CssFilter::Url(_) => return None,
            })
        })
        .collect()
}

fn blend(value: CssBlend) -> BlendMode {
    match value {
        CssBlend::Normal => BlendMode::Normal,
        CssBlend::Multiply => BlendMode::Multiply,
        CssBlend::Screen => BlendMode::Screen,
        CssBlend::Overlay => BlendMode::Overlay,
        CssBlend::Darken => BlendMode::Darken,
        CssBlend::Lighten => BlendMode::Lighten,
        CssBlend::ColorDodge => BlendMode::ColorDodge,
        CssBlend::ColorBurn => BlendMode::ColorBurn,
        CssBlend::HardLight => BlendMode::HardLight,
        CssBlend::SoftLight => BlendMode::SoftLight,
        CssBlend::Difference => BlendMode::Difference,
        CssBlend::Exclusion => BlendMode::Exclusion,
        CssBlend::Hue => BlendMode::Hue,
        CssBlend::Saturation => BlendMode::Saturation,
        CssBlend::Color => BlendMode::Color,
        CssBlend::Luminosity => BlendMode::Luminosity,
        CssBlend::PlusLighter => BlendMode::PlusLighter,
    }
}

#[cfg(test)]
mod tests {
    use style::values::computed::{Length, Percentage};

    use super::*;

    #[test]
    fn animated_transform_primitives_resolve_to_stylos_matrices() {
        let border = Size {
            width: 120.0,
            height: 80.0,
        };
        let deg = Angle::from_degrees;
        let px = |value| CssLengthPercentage::new_length(Length::new(value));
        let percent = |value| CssLengthPercentage::new_percent(Percentage(value));
        let operations = [
            Op::Rotate(deg(30.0)),
            Op::RotateZ(deg(-75.0)),
            Op::RotateX(deg(40.0)),
            Op::RotateY(deg(-50.0)),
            Op::Rotate3D(1.0, 2.0, 3.0, deg(70.0)),
            Op::Rotate3D(0.0, 0.0, 0.0, deg(40.0)),
            Op::Scale(2.0, 3.0),
            Op::ScaleX(-1.0),
            Op::ScaleZ(0.5),
            Op::Scale3D(2.0, 3.0, 4.0),
            Op::Skew(deg(10.0), deg(20.0)),
            Op::SkewX(deg(-15.0)),
            Op::SkewY(deg(25.0)),
            Op::Translate(px(10.0), percent(0.25)),
            Op::TranslateX(percent(-0.5)),
            Op::TranslateZ(Length::new(5.0)),
            Op::Translate3D(percent(0.5), px(-4.0), Length::new(3.0)),
        ];
        for operation in operations {
            let [primitive] = *primitives(std::slice::from_ref(&operation), border).unwrap() else {
                panic!("{operation:?} became more than one function");
            };
            let [Transform::Matrix(expected)] =
                *transform(std::slice::from_ref(&operation), border).unwrap()
            else {
                panic!("{operation:?} has no Stylo matrix");
            };
            let actual = primitive.matrix(border);
            let close = actual
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .all(|(a, b)| (a - b).abs() < 1e-5);
            assert!(close, "{operation:?}: {actual:?} != {expected:?}");
        }
    }

    #[test]
    fn a_full_turn_keeps_its_angle_as_a_keyframe() {
        let border = Size {
            width: 10.0,
            height: 10.0,
        };
        let turn = Op::Rotate(Angle::from_degrees(360.0));
        assert_eq!(
            *primitives(&[turn], border).unwrap(),
            [Transform::Rotate {
                axis: [0.0, 0.0, 1.0],
                angle: std::f32::consts::TAU,
            }]
        );
    }
}
