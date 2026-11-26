// SPDX-License-Identifier: MIT

use std::{sync::Arc, time::Instant};

use glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use squark_types::{
    Point, Size,
    fragment::{
        AnimatedValue, AnimationDirection, AnimationFill, AnimationKeyframe, CompositorAnimation,
        Easing, LengthPercentage, Position2D, Transform,
    },
};

use crate::{
    ColorSpace, EffectId, Filter, Matrix, PaintColor, PropertyTrees, Shadow, TransformId,
    input::PaintValues,
    properties::{IDENTITY, multiply, with_origin},
    shared::{filter, resolve_transforms},
};

#[derive(Clone, Debug, PartialEq)]
pub struct AnimationTarget {
    tracks: Arc<[CompositorAnimation]>,
    transform: Option<(TransformId, Matrix)>,
    effect: Option<(EffectId, f32, Box<[Filter]>)>,
    origin: [f32; 3],
    reference: Size,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnimationSample {
    pub changed: bool,
    pub running: bool,
}

impl AnimationTarget {
    pub(crate) fn new(
        style: &PaintValues,
        origin: [f32; 3],
        transform: Option<(TransformId, Matrix)>,
        effect: Option<EffectId>,
    ) -> Self {
        Self {
            tracks: Arc::clone(&style.animations),
            transform,
            effect: effect.map(|id| (id, style.opacity, style.filters.clone())),
            origin,
            reference: style.transform_reference,
        }
    }
}

pub(crate) fn affects_transform(track: &CompositorAnimation) -> bool {
    matches!(
        track.keyframes.first().map(|key| &key.value),
        Some(AnimatedValue::Transform(_))
    )
}

pub(crate) fn affects_effect(track: &CompositorAnimation) -> bool {
    matches!(
        track.keyframes.first().map(|key| &key.value),
        Some(AnimatedValue::Opacity(_) | AnimatedValue::Filter(_))
    )
}

impl PropertyTrees {
    /// Resolve the retained property trees at the render tick. Each target's base
    /// values are the values in the commit, so delay, removal and fill do not
    /// accumulate changes from an earlier sample.
    pub fn sample_animations(&mut self, now: Instant) -> AnimationSample {
        let mut sample = AnimationSample::default();
        for target in &*self.animations {
            let mut matrix = target.transform.map(|(_, base)| base);
            let mut opacity = target.effect.as_ref().map(|(_, base, _)| *base);
            let mut filters = None;
            for track in &*target.tracks {
                let (progress, pending) = progress(track, now);
                sample.running |= pending;
                let Some((from, to, t)) = progress.and_then(|phase| keyframes(track, phase)) else {
                    continue;
                };
                match (from, to) {
                    (AnimatedValue::Transform(from), AnimatedValue::Transform(to)) => {
                        matrix = Some(with_origin(
                            interpolate_transforms(from, to, target.reference, t),
                            target.origin,
                        ));
                    }
                    (AnimatedValue::Opacity(from), AnimatedValue::Opacity(to)) => {
                        opacity = Some(lerp(*from, *to, t).clamp(0.0, 1.0));
                    }
                    (AnimatedValue::Filter(from), AnimatedValue::Filter(to)) => {
                        filters = Some(interpolate_filters(from, to, t));
                    }
                    _ => {}
                }
            }
            if let (Some((id, _)), Some(matrix)) = (target.transform, matrix)
                && self.transforms[id.0 as usize].local != matrix
            {
                Arc::make_mut(&mut self.transforms)[id.0 as usize].local = matrix;
                sample.changed = true;
            }
            if let (Some((id, _, base)), Some(opacity)) = (&target.effect, opacity) {
                let effect = &self.effects[id.0 as usize];
                let filters = filters.as_deref().unwrap_or(base);
                if effect.opacity != opacity || &*effect.filters != filters {
                    let effect = &mut Arc::make_mut(&mut self.effects)[id.0 as usize];
                    effect.opacity = opacity;
                    effect.filters = filters.into();
                    sample.changed = true;
                }
            }
        }
        sample
    }
}

#[derive(Clone, Copy)]
struct Phase {
    progress: f32,
    before: bool,
}

fn progress(track: &CompositorAnimation, now: Instant) -> (Option<Phase>, bool) {
    if track.keyframes.is_empty() || track.iterations.is_nan() || !track.delay_seconds.is_finite() {
        return (None, false);
    }
    let elapsed = if now >= track.start {
        now.duration_since(track.start).as_secs_f64()
    } else {
        -track.start.duration_since(now).as_secs_f64()
    } - track.delay_seconds;
    let duration = track.duration.as_secs_f64();
    let iterations = track.iterations.max(0.0);
    let before = elapsed < 0.0;
    let ended = !before && (duration == 0.0 || elapsed >= duration * iterations);
    if before && !matches!(track.fill, AnimationFill::Backwards | AnimationFill::Both) {
        return (None, true);
    }
    if ended && !matches!(track.fill, AnimationFill::Forwards | AnimationFill::Both) {
        return (None, false);
    }
    let overall = if before {
        0.0
    } else if ended {
        if iterations.is_finite() {
            iterations
        } else {
            1.0
        }
    } else {
        elapsed / duration
    };
    let mut index = overall.floor();
    let mut part = overall - index;
    if ended && part == 0.0 && index > 0.0 {
        index -= 1.0;
        part = 1.0;
    }
    let odd = index % 2.0 == 1.0;
    let reverse = match track.direction {
        AnimationDirection::Normal => false,
        AnimationDirection::Reverse => true,
        AnimationDirection::Alternate => odd,
        AnimationDirection::AlternateReverse => !odd,
    };
    (
        Some(Phase {
            progress: (if reverse { 1.0 - part } else { part }) as f32,
            before: before && !reverse || ended && reverse,
        }),
        !ended,
    )
}

fn keyframes(
    track: &CompositorAnimation,
    phase: Phase,
) -> Option<(&AnimatedValue, &AnimatedValue, f32)> {
    let upper = track
        .keyframes
        .partition_point(|key| key.offset <= phase.progress);
    let first = track.keyframes.first()?;
    let last = track.keyframes.last()?;
    let (lower, upper) = match upper {
        0 => (first, first),
        upper if upper == track.keyframes.len() => (last, last),
        upper => (&track.keyframes[upper - 1], &track.keyframes[upper]),
    };
    Some((&lower.value, &upper.value, segment(lower, upper, phase)))
}

fn segment(from: &AnimationKeyframe, to: &AnimationKeyframe, phase: Phase) -> f32 {
    if to.offset <= from.offset {
        return 0.0;
    }
    let t = (phase.progress - from.offset) / (to.offset - from.offset);
    match from.easing {
        Easing::Linear => t,
        Easing::CubicBezier([x1, y1, x2, y2]) => {
            let cubic = |m: f32, a, b| {
                let r = 1.0 - m;
                3.0 * r * r * m * a + 3.0 * r * m * m * b + m * m * m
            };
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..20 {
                let mid = (lo + hi) * 0.5;
                if cubic(mid, x1, x2) < t {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            if t == 0.0 || t == 1.0 {
                t
            } else {
                cubic((lo + hi) * 0.5, y1, y2)
            }
        }
        Easing::Steps {
            count,
            jump_start,
            jump_end,
        } => {
            let jumps = count as f32 + u8::from(jump_start) as f32 - u8::from(!jump_end) as f32;
            if jumps < 1.0 {
                return t;
            }
            let scaled = t * count as f32;
            let step = scaled.floor() + u8::from(jump_start) as f32
                - u8::from(phase.before && scaled.fract() == 0.0) as f32;
            (step / jumps).clamp(0.0, 1.0)
        }
    }
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn interpolate_transforms(from: &[Transform], to: &[Transform], size: Size, t: f32) -> Matrix {
    if t == 0.0 || from == to {
        return resolve_transforms(from, size);
    }
    if t == 1.0 {
        return resolve_transforms(to, size);
    }
    // CSS Transforms 2: the shorter list is padded with identity functions,
    // pairs that share a primitive interpolate numerically, and from the first
    // pair that does not, the rest of both lists interpolate as matrices.
    let mut result = IDENTITY;
    for i in 0..from.len().max(to.len()) {
        let (a, b) = match (from.get(i), to.get(i)) {
            (Some(a), Some(b)) => (*a, *b),
            (Some(a), None) => (*a, identity_like(a)),
            (None, Some(b)) => (identity_like(b), *b),
            (None, None) => break,
        };
        let Some(step) = interpolate_function(a, b, size, t) else {
            let rest =
                |list: &[Transform]| resolve_transforms(list.get(i..).unwrap_or_default(), size);
            return multiply(result, interpolate_matrix(rest(from), rest(to), t));
        };
        result = multiply(result, step);
    }
    result
}

/// The identity function of the same kind, which pads the shorter list.
fn identity_like(transform: &Transform) -> Transform {
    match *transform {
        Transform::Matrix(_) => Transform::Matrix(IDENTITY),
        Transform::Translate { .. } => Transform::Translate {
            xy: Default::default(),
            z: 0.0,
        },
        Transform::Rotate { axis, .. } => Transform::Rotate { axis, angle: 0.0 },
        Transform::Scale { .. } => Transform::Scale {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        },
        Transform::Skew { .. } => Transform::Skew { x: 0.0, y: 0.0 },
    }
}

/// One pair of functions at `t`, or `None` when they share no primitive.
/// Angles interpolate as angles, so `rotate(0)` to `rotate(360deg)` turns once
/// and `rotate(270deg)` is reached clockwise, not by the shorter way round.
fn interpolate_function(from: Transform, to: Transform, size: Size, t: f32) -> Option<Matrix> {
    use Transform as T;
    let length = |a: LengthPercentage, b: LengthPercentage| LengthPercentage {
        px: lerp(a.px, b.px, t),
        fraction: lerp(a.fraction, b.fraction, t),
    };
    let value = match (from, to) {
        (T::Matrix(a), T::Matrix(b)) => return Some(interpolate_matrix(a, b, t)),
        (T::Translate { xy: a, z: az }, T::Translate { xy: b, z: bz }) => T::Translate {
            xy: Position2D {
                x: length(a.x, b.x),
                y: length(a.y, b.y),
            },
            z: lerp(az, bz, t),
        },
        (
            T::Rotate {
                axis: a,
                angle: from_angle,
            },
            T::Rotate {
                axis: b,
                angle: to_angle,
            },
        ) => {
            // A zero angle has no axis of its own, so it takes the other one.
            let axis = if from_angle == 0.0 { b } else { a };
            if to_angle != 0.0 && !same_axis(axis, b) {
                // Different axes: the shortest arc between the orientations,
                // which CSS takes through quaternions too.
                return Some(interpolate_matrix(from.matrix(size), to.matrix(size), t));
            }
            T::Rotate {
                axis,
                angle: lerp(from_angle, to_angle, t),
            }
        }
        (
            T::Scale { x, y, z },
            T::Scale {
                x: x2,
                y: y2,
                z: z2,
            },
        ) => T::Scale {
            x: lerp(x, x2, t),
            y: lerp(y, y2, t),
            z: lerp(z, z2, t),
        },
        (T::Skew { x, y }, T::Skew { x: x2, y: y2 }) => T::Skew {
            x: lerp(x, x2, t),
            y: lerp(y, y2, t),
        },
        _ => return None,
    };
    Some(value.matrix(size))
}

fn same_axis(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() <= 1e-6)
}

fn interpolate_matrix(from: Matrix, to: Matrix, t: f32) -> Matrix {
    if from == to {
        return from;
    }
    if let (Some(from), Some(to)) = (Components::of(from), Components::of(to)) {
        return from.interpolate(to, t).matrix();
    }
    if t < 0.5 { from } else { to }
}

struct Components {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
    skew: Vec3,
    perspective: Vec4,
}

impl Components {
    fn of(matrix: Matrix) -> Option<Self> {
        let matrix = Mat4::from_cols_array_2d(&matrix);
        let w = matrix.w_axis.w;
        if w == 0.0 || !matrix.is_finite() {
            return None;
        }
        let normal = matrix * w.recip();
        let affine = Mat4::from_cols(
            normal.x_axis.truncate().extend(0.0),
            normal.y_axis.truncate().extend(0.0),
            normal.z_axis.truncate().extend(0.0),
            normal.w_axis.truncate().extend(1.0),
        );
        if affine.determinant() == 0.0 {
            return None;
        }
        let perspective = affine.inverse().transpose() * normal.row(3);
        let (mut x, mut y, mut z) = (
            affine.x_axis.truncate(),
            affine.y_axis.truncate(),
            affine.z_axis.truncate(),
        );
        let mut scale = Vec3::ZERO;
        scale.x = x.length();
        x /= scale.x;
        let xy = x.dot(y);
        y -= x * xy;
        scale.y = y.length();
        y /= scale.y;
        let xz = x.dot(z);
        z -= x * xz;
        let yz = y.dot(z);
        z -= y * yz;
        scale.z = z.length();
        z /= scale.z;
        let skew = Vec3::new(xy / scale.y, xz / scale.z, yz / scale.z);
        if !skew.is_finite() || !perspective.is_finite() {
            return None;
        }
        if x.dot(y.cross(z)) < 0.0 {
            x = -x;
            y = -y;
            z = -z;
            scale = -scale;
        }
        Some(Self {
            translation: affine.w_axis.truncate(),
            rotation: Quat::from_mat3(&Mat3::from_cols(x, y, z)),
            scale,
            skew,
            perspective,
        })
    }

    fn interpolate(self, to: Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(to.translation, t),
            rotation: self.rotation.slerp(to.rotation, t),
            scale: self.scale.lerp(to.scale, t),
            skew: self.skew.lerp(to.skew, t),
            perspective: self.perspective.lerp(to.perspective, t),
        }
    }

    fn matrix(self) -> Matrix {
        let p = self.perspective;
        let perspective = Mat4::from_cols(
            Vec4::X + Vec4::W * p.x,
            Vec4::Y + Vec4::W * p.y,
            Vec4::Z + Vec4::W * p.z,
            Vec4::W * p.w,
        );
        let skew = Mat4::from_cols(
            Vec4::X,
            Vec4::Y + Vec4::X * self.skew.x,
            Vec4::Z + Vec4::X * self.skew.y + Vec4::Y * self.skew.z,
            Vec4::W,
        );
        (perspective
            * Mat4::from_rotation_translation(self.rotation, self.translation)
            * skew
            * Mat4::from_scale(self.scale))
        .to_cols_array_2d()
    }
}

fn interpolate_filters(
    from: &[squark_types::fragment::Filter],
    to: &[squark_types::fragment::Filter],
    t: f32,
) -> Box<[Filter]> {
    let result = (0..from.len().max(to.len()))
        .map(|i| {
            let from = from.get(i).copied().map(filter);
            let to = to.get(i).copied().map(filter);
            let (a, b) = match (from, to) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => {
                    let b = neutral(&a);
                    (a, b)
                }
                (None, Some(b)) => (neutral(&b), b),
                (None, None) => unreachable!(),
            };
            filter_pair(a, b, t)
        })
        .collect::<Option<Box<_>>>();
    result.unwrap_or_else(|| {
        let filters = if t < 0.5 { from } else { to };
        filters.iter().copied().map(filter).collect()
    })
}

fn neutral(value: &Filter) -> Filter {
    match value {
        Filter::Opacity(_) => Filter::Opacity(1.0),
        Filter::Brightness(_) => Filter::Brightness(1.0),
        Filter::Contrast(_) => Filter::Contrast(1.0),
        Filter::Saturate(_) => Filter::Saturate(1.0),
        Filter::Blur(_) => Filter::Blur(0.0),
        Filter::HueRotate(_) => Filter::HueRotate(0.0),
        Filter::Grayscale(_) => Filter::Grayscale(0.0),
        Filter::Invert(_) => Filter::Invert(0.0),
        Filter::Sepia(_) => Filter::Sepia(0.0),
        Filter::DropShadow(shadow) => Filter::DropShadow(Shadow {
            color: PaintColor {
                rgba: [0.0; 4],
                ..shadow.color
            },
            offset: Point::default(),
            blur: 0.0,
            spread: 0.0,
            inset: false,
        }),
        Filter::TextShadow(_) => unreachable!("CSS filter descriptors have no text-shadow"),
    }
}

fn filter_pair(a: Filter, b: Filter, t: f32) -> Option<Filter> {
    use Filter::*;
    let blend = |a, b| lerp(a, b, t);
    let unit = |a, b| blend(a, b).clamp(0.0, 1.0);
    Some(match (a, b) {
        (Blur(a), Blur(b)) => Blur(blend(a, b).max(0.0)),
        (Brightness(a), Brightness(b)) => Brightness(blend(a, b).max(0.0)),
        (Contrast(a), Contrast(b)) => Contrast(blend(a, b).max(0.0)),
        (Grayscale(a), Grayscale(b)) => Grayscale(unit(a, b)),
        (HueRotate(a), HueRotate(b)) => HueRotate(blend(a, b)),
        (Invert(a), Invert(b)) => Invert(unit(a, b)),
        (Opacity(a), Opacity(b)) => Opacity(unit(a, b)),
        (Saturate(a), Saturate(b)) => Saturate(blend(a, b).max(0.0)),
        (Sepia(a), Sepia(b)) => Sepia(unit(a, b)),
        (DropShadow(a), DropShadow(b)) => {
            let (space, tag) =
                if a.color.space == ColorSpace::Srgb && b.color.space == ColorSpace::Srgb {
                    (ColorSpace::Srgb, color::ColorSpaceTag::Srgb)
                } else {
                    (ColorSpace::Oklab, color::ColorSpaceTag::Oklab)
                };
            let rgba = a
                .color
                .dynamic()
                .interpolate(b.color.dynamic(), tag, color::HueDirection::Shorter)
                .eval(t)
                .components;
            let color = PaintColor { space, rgba };
            DropShadow(Shadow {
                offset: Point {
                    x: blend(a.offset.x, b.offset.x),
                    y: blend(a.offset.y, b.offset.y),
                },
                blur: blend(a.blur, b.blur).max(0.0),
                spread: blend(a.spread, b.spread),
                color,
                inset: false,
            })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn delays_fractional_iterations_and_fill_use_the_animation_direction() {
        let start = Instant::now();
        let animation = CompositorAnimation {
            id: 1,
            start,
            delay_seconds: 1.0,
            duration: Duration::from_secs(2),
            iterations: 1.25,
            direction: AnimationDirection::AlternateReverse,
            fill: AnimationFill::Both,
            keyframes: Box::new([
                AnimationKeyframe {
                    offset: 0.0,
                    value: AnimatedValue::Opacity(0.0),
                    easing: Easing::Linear,
                },
                AnimationKeyframe {
                    offset: 1.0,
                    value: AnimatedValue::Opacity(1.0),
                    easing: Easing::Linear,
                },
            ]),
        };
        for (at, expected, running) in [
            (0, 1.0, true),
            (1000, 1.0, true),
            (2000, 0.5, true),
            (3000, 0.0, true),
            (3500, 0.25, false),
            (5000, 0.25, false),
        ] {
            let (sample, pending) = progress(&animation, start + Duration::from_millis(at));
            assert_eq!(sample.unwrap().progress, expected);
            assert_eq!(pending, running);
        }
    }

    #[test]
    fn cubic_and_step_easing_sample_the_outgoing_keyframe() {
        let phase = Phase {
            progress: 0.5,
            before: false,
        };
        let from = AnimationKeyframe {
            offset: 0.0,
            value: AnimatedValue::Opacity(0.0),
            easing: Easing::CubicBezier([0.25, 0.1, 0.25, 1.0]),
        };
        let to = AnimationKeyframe {
            offset: 1.0,
            value: AnimatedValue::Opacity(1.0),
            easing: Easing::Linear,
        };
        let t = segment(&from, &to, phase);
        assert!((t - 0.802_403).abs() < 0.000_01);
        let start_steps = AnimationKeyframe {
            easing: Easing::Steps {
                count: 4,
                jump_start: true,
                jump_end: false,
            },
            ..from
        };
        assert_eq!(segment(&start_steps, &to, phase), 0.75);
        assert_eq!(
            segment(
                &start_steps,
                &to,
                Phase {
                    before: true,
                    ..phase
                }
            ),
            0.5
        );
    }

    const SIZE: Size = Size {
        width: 100.0,
        height: 50.0,
    };

    fn close(actual: Matrix, expected: Mat4) -> bool {
        Mat4::from_cols_array_2d(&actual).abs_diff_eq(expected, 1e-5)
    }

    fn rotate(axis: [f32; 3], degrees: f32) -> Transform {
        Transform::Rotate {
            axis,
            angle: degrees.to_radians(),
        }
    }

    #[test]
    fn rotations_interpolate_their_angles() {
        let z = |degrees| rotate([0.0, 0.0, 1.0], degrees);
        let turned = |degrees: f32| Mat4::from_rotation_z(degrees.to_radians());
        // A spinner, `none` to `rotate(360deg)`, turns a full circle although
        // both ends are the identity matrix.
        for (t, degrees) in [(0.25, 90.0), (0.5, 180.0), (0.75, 270.0)] {
            let sampled = interpolate_transforms(&[], &[z(360.0)], SIZE, t);
            assert!(close(sampled, turned(degrees)), "{t}: {sampled:?}");
        }
        // 270 degrees is reached clockwise, not by the shorter way round.
        let sampled = interpolate_transforms(&[z(0.0)], &[z(270.0)], SIZE, 0.5);
        assert!(close(sampled, turned(135.0)), "{sampled:?}");
        // A zero angle takes the other rotation's axis.
        let sampled = interpolate_transforms(
            &[rotate([1.0, 0.0, 0.0], 0.0)],
            &[rotate([0.0, 1.0, 0.0], 90.0)],
            SIZE,
            0.5,
        );
        assert!(close(sampled, Mat4::from_rotation_y(45f32.to_radians())));
        // Different axes take the shortest arc, like the matrix path.
        let (from, to) = (rotate([1.0, 0.0, 0.0], 60.0), rotate([0.0, 1.0, 0.0], 60.0));
        assert_eq!(
            interpolate_transforms(&[from], &[to], SIZE, 0.3),
            interpolate_matrix(from.matrix(SIZE), to.matrix(SIZE), 0.3)
        );
    }

    #[test]
    fn scales_skews_and_translations_interpolate_numerically() {
        // scale(1) to scale(-1) passes through zero; as matrices it would
        // turn half a circle instead.
        let scale = |x| Transform::Scale { x, y: 1.0, z: 1.0 };
        let sampled = interpolate_transforms(&[scale(1.0)], &[scale(-1.0)], SIZE, 0.5);
        assert!(
            close(sampled, Mat4::from_scale(Vec3::new(0.0, 1.0, 1.0))),
            "{sampled:?}"
        );
        // skewX interpolates its angle, not the tangent the matrix holds.
        let skew = |degrees: f32| Transform::Skew {
            x: degrees.to_radians(),
            y: 0.0,
        };
        let sampled = interpolate_transforms(&[skew(0.0)], &[skew(80.0)], SIZE, 0.5);
        assert!((sampled[1][0] - 40f32.to_radians().tan()).abs() < 1e-5);
        // Percentages stay relative to the reference box.
        let translate = |px, fraction| Transform::Translate {
            xy: Position2D {
                x: LengthPercentage { px, fraction },
                y: LengthPercentage::default(),
            },
            z: 0.0,
        };
        let sampled =
            interpolate_transforms(&[translate(10.0, 0.0)], &[translate(0.0, 0.5)], SIZE, 0.5);
        assert!(close(
            sampled,
            Mat4::from_translation(Vec3::new(30.0, 0.0, 0.0))
        ));
    }

    #[test]
    fn lists_interpolate_their_common_prefix_and_then_matrices() {
        let translate = |px| Transform::Translate {
            xy: Position2D {
                x: LengthPercentage { px, fraction: 0.0 },
                y: LengthPercentage::default(),
            },
            z: 0.0,
        };
        let spin = rotate([0.0, 0.0, 1.0], 90.0);
        let grow = Transform::Scale {
            x: 2.0,
            y: 2.0,
            z: 1.0,
        };
        // translate pairs with translate; rotate and scale share no primitive.
        let sampled = interpolate_transforms(
            &[translate(10.0), spin],
            &[translate(30.0), grow],
            SIZE,
            0.5,
        );
        let expected = multiply(
            crate::properties::translation(20.0, 0.0, 0.0),
            interpolate_matrix(spin.matrix(SIZE), grow.matrix(SIZE), 0.5),
        );
        assert_eq!(sampled, expected);
        // The shorter list is padded with identity functions of the same kind.
        let sampled =
            interpolate_transforms(&[translate(10.0)], &[translate(30.0), spin], SIZE, 0.5);
        let expected = Mat4::from_translation(Vec3::new(20.0, 0.0, 0.0))
            * Mat4::from_rotation_z(45f32.to_radians());
        assert!(close(sampled, expected), "{sampled:?}");
    }

    #[test]
    fn matrices_interpolate_rotation_without_shrinking_or_discarding_skew() {
        let end = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2).to_cols_array_2d();
        let midpoint = Mat4::from_cols_array_2d(&interpolate_matrix(IDENTITY, end, 0.5));
        let p = midpoint * Vec4::X;
        assert!(p.abs_diff_eq(
            Vec4::new(
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0
            ),
            1e-5
        ));
        let matrix = Mat4::from_cols(
            Vec4::new(2.0, 0.0, 0.0, 0.0),
            Vec4::new(1.2, 3.0, 0.0, 0.0),
            Vec4::new(0.8, 0.4, 4.0, -0.1),
            Vec4::new(1.0, 2.0, 3.0, 1.0),
        );
        let decomposed = Components::of(matrix.to_cols_array_2d()).unwrap();
        assert!(Mat4::from_cols_array_2d(&decomposed.matrix()).abs_diff_eq(matrix, 1e-6));
    }
}
