// SPDX-License-Identifier: MIT

use color::{ColorSpaceTag as Tag, DynamicColor, HueDirection};
use squark_paint::{ColorSpace, Gradient, HueInterpolation, PaintColor};

pub(super) fn srgb(color: PaintColor) -> [f32; 4] {
    dynamic(color)
        .convert(Tag::Srgb)
        .components
        .map(|x| x.clamp(0.0, 1.0))
}

pub(super) fn premul([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

pub(super) fn dynamic(color: PaintColor) -> DynamicColor {
    color.dynamic()
}

pub(super) fn tag(space: ColorSpace) -> Tag {
    space.tag()
}

pub(super) fn hue(hue: HueInterpolation) -> HueDirection {
    match hue {
        HueInterpolation::Shorter => HueDirection::Shorter,
        HueInterpolation::Longer => HueDirection::Longer,
        HueInterpolation::Increasing => HueDirection::Increasing,
        HueInterpolation::Decreasing => HueDirection::Decreasing,
    }
}

pub(super) const RAMP_WIDTH: usize = 1024;

pub(super) fn gradient_ramp(gradient: &Gradient) -> ([f32; 2], Vec<u8>) {
    let stops = &gradient.stops;
    let first = stops.first().map_or(0.0, |stop| stop.0);
    let last = stops.last().map_or(1.0, |stop| stop.0);
    let extent = last - first;
    let space = tag(gradient.interpolation);
    let hue = hue(gradient.hue);
    let mut bytes = Vec::with_capacity(RAMP_WIDTH * 4);
    for i in 0..RAMP_WIDTH {
        let t = first + extent * i as f32 / (RAMP_WIDTH - 1) as f32;
        let next = stops.partition_point(|stop| stop.0 <= t);
        let mut rgba = if next > 0 && next < stops.len() {
            let (a, from) = stops[next - 1];
            let (b, to) = stops[next];
            let progress = (t - a) / (b - a);
            let progress =
                gradient
                    .hints
                    .get(next - 1)
                    .copied()
                    .flatten()
                    .map_or(progress, |hint| {
                        let hint = ((hint - a) / (b - a)).clamp(1e-5, 1.0 - 1e-5);
                        progress.powf(0.5_f32.ln() / hint.ln())
                    });
            dynamic(from)
                .interpolate(dynamic(to), space, hue)
                .eval(progress)
                .convert(Tag::Srgb)
                .components
                .map(|x| x.clamp(0.0, 1.0))
        } else {
            stops
                .get(next.saturating_sub(1))
                .map_or([0.0; 4], |stop| srgb(stop.1))
        };
        if extent == 0.0 && gradient.repeating && stops.len() > 1 {
            let first = srgb(stops[0].1);
            rgba = std::array::from_fn(|i| (first[i] + rgba[i]) * 0.5);
        }
        bytes.extend(premul(rgba).map(|x| (x * 255.0).round() as u8));
    }
    ([first, extent], bytes)
}
