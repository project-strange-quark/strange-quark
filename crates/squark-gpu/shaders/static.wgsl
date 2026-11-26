// SPDX-License-Identifier: MIT

struct Instance {
    bounds: vec4f,
    shape: vec4f,
    radii: array<vec4f, 2>,
    inner: vec4f,
    inner_radii: array<vec4f, 2>,
    colors: array<vec4f, 4>,
    params: vec4f,
    extra: vec4f,
    texels: vec4f,
    sample_rect: vec4f,
    tile: vec4f,
    period: vec4f,
    flags: vec4u,
    clip_stop: vec4u,
}

struct Clip {
    inverse: array<vec4f, 3>,
    shape: vec4f,
    radii: array<vec4f, 2>,
    texels: vec4f,
    flags: vec4u,
}

struct RasterViewport {
    css_size: vec2f,
    scale: f32,
    origin: vec2f,
}

struct PaintTransform {
    forward: mat4x4f,
    inverse: array<vec4f, 3>,
}

@group(0) @binding(0) var<uniform> viewport: RasterViewport;
@group(0) @binding(1) var<storage, read> transforms: array<PaintTransform>;
@group(0) @binding(2) var<storage, read> clips: array<Clip>;
@group(0) @binding(3) var<storage, read> items: array<Instance>;
@group(0) @binding(4) var path_atlas: texture_2d<f32>;
@group(0) @binding(5) var linear_sampler: sampler;
@group(1) @binding(0) var image: texture_2d<f32>;

struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) @interpolate(flat) index: u32,
}

@vertex
fn vertex(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) index: u32) -> Vertex {
    let corners = array(vec2f(0.0, 0.0), vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0));
    let bounds = items[index].bounds;
    let point = bounds.xy + corners[vertex_id] * bounds.zw;
    let world = transforms[items[index].flags.x].forward * vec4f(point, 0.0, 1.0);
    let local_world = world.xy - viewport.origin * (world.w / viewport.scale);
    let ndc = local_world * (vec2f(2.0, -2.0) / viewport.css_size) + vec2f(-world.w, world.w);
    return Vertex(vec4f(ndc, 0.0, world.w), index);
}

fn rounded_distance(point: vec2f, rect: vec4f, radii: array<vec4f, 2>) -> f32 {
    let a = point - rect.xy;
    let b = rect.zw - a;
    let distances = min(a, b);
    let edge = -min(distances.x, distances.y);
    // A radius may extend past its box midpoint. Test each corner box.
    let offsets = array(a, vec2f(b.x, a.y), b, vec2f(a.x, b.y));
    let corners = array(radii[0].xy, radii[0].zw, radii[1].xy, radii[1].zw);
    for (var i = 0u; i < 4u; i++) {
        let radius = corners[i];
        let offset = offsets[i];
        if all(radius > vec2f(0.0)) && all(offset < radius) {
            let normalized = (radius - offset) / radius;
            let slope = length(normalized / radius);
            return (dot(normalized, normalized) - 1.0) / max(2.0 * slope, 1e-6);
        }
    }
    return edge;
}

// Explicit pixel footprints keep gradients defined in a varying clip chain.
fn aa(distance: f32, pixel: f32) -> f32 {
    return clamp(0.5 - distance / max(pixel, 1e-5), 0.0, 1.0);
}

fn local_pixel(inverse: array<vec4f, 3>, at: vec2f, w: f32) -> f32 {
    let dx = (vec2f(inverse[0].x, inverse[1].x) - at * inverse[2].x) / w;
    let dy = (vec2f(inverse[0].y, inverse[1].y) - at * inverse[2].y) / w;
    return max(length(dx), length(dy)) / viewport.scale;
}

fn clip_amount(mut_id: u32, stop: u32, root: vec3f) -> f32 {
    var amount = 1.0;
    var id = mut_id;
    loop {
        if id == stop { break; }
        let clip = clips[id];
        let w = dot(clip.inverse[2].xyz, root);
        if w <= 0.0 { return 0.0; }
        let local = vec2f(dot(clip.inverse[0].xyz, root), dot(clip.inverse[1].xyz, root)) / w;
        let uv = (local - clip.shape.xy) / clip.shape.zw;
        if clip.flags.y == 2u {
            if any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) { return 0.0; }
            let texels = clip.texels;
            let texel = texels.xy + clamp(uv * texels.zw, vec2f(0.5), texels.zw - 0.5);
            if clip.flags.z != 0u {
                amount *= textureLoad(path_atlas, vec2i(floor(texel)), 0).a;
            } else {
                amount *= textureSampleLevel(path_atlas, linear_sampler, texel / vec2f(textureDimensions(path_atlas)), 0.0).a;
            }
        } else if clip.flags.y == 3u {
            return 0.0;
        } else {
            var distance = rounded_distance(local, clip.shape, clip.radii);
            if clip.flags.y == 1u {
                let edges = max(clip.shape.xy - local, local - (clip.shape.xy + clip.shape.zw));
                distance = max(select(-1e10, edges.x, clip.flags.z != 0u),
                               select(-1e10, edges.y, clip.flags.w != 0u));
            }
            amount *= aa(distance, local_pixel(clip.inverse, local, w));
        }
        if amount <= 0.0 { return 0.0; }
        id = clip.flags.x;
    }
    return amount;
}

fn premul(color: vec4f) -> vec4f {
    return vec4f(color.rgb * color.a, color.a);
}

fn texel_center(rect: vec4f, point: vec2f) -> vec2f {
    return clamp(point, rect.xy + 0.5, rect.xy + rect.zw - 0.5);
}

fn tile_point(item: Instance, point: vec2f) -> vec2f {
    let delta = point - item.tile.xy;
    let period = max(item.period.xy, vec2f(1e-5));
    let wrapped = delta - floor(delta / period) * period;
    return item.tile.xy + select(delta, wrapped, item.period.xy > vec2f(0.0));
}

fn inside_tile(item: Instance, point: vec2f) -> bool {
    let delta = point - item.tile.xy;
    return all(delta >= vec2f(0.0)) && all(delta < item.tile.zw);
}

// Vello writes straight alpha. Convert the taps before interpolation.
fn sample_straight(at: vec2f, rect: vec4f) -> vec4f {
    let texel = at - 0.5;
    let lo = vec2i(floor(texel));
    let hi = min(lo + 1, vec2i(rect.xy + rect.zw) - 1);
    let t = fract(texel);
    let top = mix(premul(textureLoad(image, lo, 0)), premul(textureLoad(image, vec2i(hi.x, lo.y), 0)), t.x);
    let bottom = mix(premul(textureLoad(image, vec2i(lo.x, hi.y), 0)), premul(textureLoad(image, hi, 0)), t.x);
    return mix(top, bottom, t.y);
}

fn sample_image(item: Instance, point: vec2f, pixel: f32) -> vec4f {
    var uv = (point - item.shape.xy) / item.shape.zw;
    var coverage = 1.0;
    if item.period.z != 0.0 {
        let local = tile_point(item, point);
        if !inside_tile(item, local) { return vec4f(0.0); }
        uv = (local - item.tile.xy) / item.tile.zw;
        coverage = aa(rounded_distance(point, item.shape, item.radii), pixel);
    }
    let texel = texel_center(item.texels, item.sample_rect.xy + uv * item.sample_rect.zw);
    let straight = (item.flags.w & 2u) != 0u;
    if (item.flags.w & 1u) == 0u {
        let sample = textureLoad(image, vec2i(floor(texel)), 0);
        return select(sample, premul(sample), straight) * coverage;
    }
    if straight {
        return sample_straight(texel, item.texels) * coverage;
    }
    // Match the fractional weights of Chromium's software replaced-image
    // sampling. CSS background tiles and glyph masks use the full coordinates.
    let replaced = item.flags.z == 4u && item.period.z == 0.0;
    let sample = select(texel, floor(texel * 16.0 + 1e-4) / 16.0, replaced);
    return textureSampleLevel(image, linear_sampler, sample / vec2f(textureDimensions(image)), 0.0) * coverage;
}

fn sample_path(item: Instance, point: vec2f) -> vec4f {
    let uv = (point - item.shape.xy) / item.shape.zw;
    let texel = texel_center(item.texels, item.texels.xy + uv * item.texels.zw);
    if item.flags.w != 0u {
        return premul(textureLoad(image, vec2i(floor(texel)), 0));
    }
    return sample_straight(texel, item.texels);
}

fn border(item: Instance, point: vec2f, pixel: f32) -> vec4f {
    let outer = aa(rounded_distance(point, item.shape, item.radii), pixel);
    var inner = 0.0;
    if all(item.inner.zw > vec2f(0.0)) {
        inner = aa(rounded_distance(point, item.inner, item.inner_radii), pixel);
    }
    let p = point - item.shape.xy;
    let edges = vec4f(p.y, item.shape.z - p.x, item.shape.w - p.y, p.x);
    var side = 0u;
    var best = 1e15;
    for (var i = 0u; i < 4u; i++) {
        if item.params[i] > 0.0 && edges[i] / item.params[i] < best {
            side = i;
            best = edges[i] / item.params[i];
        }
    }
    let width = max(item.params[side], 1e-5);
    let along = select(p.x, p.y, side == 1u || side == 3u);
    let side_length = select(item.shape.z, item.shape.w, side == 1u || side == 3u);
    let style = u32(item.extra[side]);
    var impact = 1.0;
    var color = item.colors[side];
    switch style {
        case 0u, 1u: { impact = 0.0; }
        case 3u, 4u: {
            let uniform_square = all(item.params == vec4f(width)) &&
                all(item.extra == vec4f(f32(style))) &&
                all(item.radii[0] == vec4f(0.0)) && all(item.radii[1] == vec4f(0.0));
            if uniform_square {
                if style == 3u {
                    // Center one dot in each corner and space the in-between dots evenly.
                    let span = max(side_length - width, 0.0);
                    let count = max(round(span / (2.0 * width)), 1.0);
                    let step = max(span / count, 1e-5);
                    let travel = along - width * 0.5;
                    let center = clamp(round(travel / step), 0.0, count) * step;
                    let distance = length(vec2f(travel - center, edges[side] - width * 0.5));
                    impact = aa(distance - width * 0.5, pixel);
                } else {
                    // Fit full dashes into the side with an equal gap between each pair.
                    let dash = width * 2.0;
                    let span = max(side_length - 2.0 * dash, 0.0);
                    let count = max(round((span - width) / (dash + width)), 0.0);
                    let gap = max((span - count * dash) / (count + 1.0), 0.0);
                    let step = dash + gap;
                    let travel = along - dash;
                    let edge = min(travel, span - travel);
                    let phase = travel - floor(travel / step) * step;
                    let distance = min(edge, max(gap - phase, phase - step));
                    impact = aa(distance, pixel);
                }
            } else {
                let dash = select(width * 3.0, width, style == 3u);
                let periods = max(round(side_length / (2.0 * dash)), 1.0);
                let period = max(side_length / periods, 1e-5);
                let travel = abs(fract(along / period + 0.5) - 0.5) * period;
                if style == 3u {
                    let distance = length(vec2f(travel, edges[side] - width * 0.5));
                    impact = aa(distance - width * 0.5, pixel);
                } else {
                    impact = aa(travel - period * 0.25, pixel);
                }
            }
        }
        case 5u: {
            if width >= 3.0 * pixel {
                impact = max(aa(edges[side] - width / 3.0, pixel), aa(2.0 * width / 3.0 - edges[side], pixel));
            }
        }
        case 6u, 7u, 8u, 9u: {
            var dark = side == 0u || side == 3u;
            if style == 7u || style == 9u { dark = !dark; }
            if (style == 6u || style == 7u) && best > 0.5 { dark = !dark; }
            color = vec4f(color.rgb * select(1.0, 0.65, dark), color.a);
        }
        default: {}
    }
    return premul(color) * (max(outer - inner, 0.0) * impact);
}

fn normal_cdf(x: f32) -> f32 {
    let z = clamp(x, -6.0, 6.0);
    // 0.5 + 0.5 * tanh(y), written as the logistic 1 / (1 + exp(-2y)):
    // WARP's tanh is too coarse in the tails, which spread box-shadow
    // blur far past the shadow and cut it off at the instance's quad.
    let y = 0.79788456 * z * (1.0 + 0.044715 * z * z);
    return 1.0 / (1.0 + exp(-2.0 * y));
}

fn blur_box(rect: vec4f, point: vec2f, distance: f32, sigma: f32) -> f32 {
    let lo = (rect.xy - point) / sigma;
    let hi = (rect.xy + rect.zw - point) / sigma;
    let x = max(normal_cdf(hi.x) - normal_cdf(lo.x), 0.0);
    let y = max(normal_cdf(hi.y) - normal_cdf(lo.y), 0.0);
    let edges = max(lo, -hi);
    let square = length(max(edges, vec2f(0.0))) + min(max(edges.x, edges.y), 0.0);
    // Account for the part of the rectangular kernel outside a rounded
    // corner, keeping the exact separable box response along straight edges.
    let corner = min(1.0, normal_cdf(-distance / sigma) / max(normal_cdf(-square), 1e-5));
    return x * y * corner;
}

fn shadow(item: Instance, point: vec2f, pixel: f32) -> vec4f {
    let original = aa(rounded_distance(point, item.inner, item.inner_radii), pixel);
    let distance = rounded_distance(point, item.shape, item.radii);
    let sigma = item.params.x;
    var blurred = aa(distance, pixel);
    if sigma > 0.0 {
        blurred = blur_box(item.shape, point, distance, sigma);
    }
    let amount = select(blurred * (1.0 - original), (1.0 - blurred) * original, item.flags.w != 0u);
    return premul(item.colors[0]) * amount;
}

// The angle of `delta` clockwise from 12 o'clock, in turns. GPU atan2 is not
// reliable when an argument is a signed zero: on the axes through a conic
// gradient's center, as when the center lies on a pixel center, it returned
// the opposite angle and drew a seam. Only comparisons pick the quadrant here,
// and atan sees a ratio in [0, 1].
fn conic_turns(delta: vec2f) -> f32 {
    let x = delta.x;
    let y = -delta.y;
    let ax = abs(x);
    let ay = abs(y);
    if ax == 0.0 && ay == 0.0 { return 0.0; }
    var angle = atan(min(ax, ay) / max(ax, ay));
    if ax > ay { angle = 1.57079632679 - angle; }
    if y < 0.0 { angle = 3.14159265359 - angle; }
    if x < 0.0 { angle = 6.28318530718 - angle; }
    return angle / 6.28318530718;
}

fn gradient(item: Instance, point: vec2f, pixel: f32) -> vec4f {
    var local = point;
    if item.period.z != 0.0 {
        local = tile_point(item, point);
        if !inside_tile(item, local) { return vec4f(0.0); }
    }
    let delta = local - item.params.xy;
    var t: f32;
    switch item.flags.w & 3u {
        case 1u: { t = length(delta / max(item.params.zw, vec2f(1e-5))); }
        case 2u: { t = fract(conic_turns(delta) - item.params.z / 6.28318530718); }
        default: {
            let direction = item.params.zw - item.params.xy;
            t = dot(delta, direction) / max(dot(direction, direction), 1e-10);
        }
    }
    let span = item.extra.y;
    t = select(1.0, (t - item.extra.x) / max(span, 1e-10), span > 0.0);
    if (item.flags.w & 4u) != 0u && span > 0.0 { t = fract(t); }
    let texel = item.texels.xy + vec2f(0.5 + clamp(t, 0.0, 1.0) * (item.texels.z - 1.0), 0.5);
    let color = textureSampleLevel(image, linear_sampler, texel / vec2f(textureDimensions(image)), 0.0);
    return color * aa(rounded_distance(point, item.shape, item.radii), pixel);
}

@fragment
fn draw(in: Vertex) -> @location(0) vec4f {
    let item = items[in.index];
    // The same device pixel must have the same coverage in the full scene
    // and in every cropped viewport. Triangle varying interpolation can
    // round differently when the viewport changes.
    let root = vec3f((in.position.xy + viewport.origin) / viewport.scale, 1.0);
    let inverse = transforms[item.flags.x].inverse;
    let w = dot(inverse[2].xyz, root);
    if w <= 0.0 { return vec4f(0.0); }
    let local = vec2f(dot(inverse[0].xyz, root), dot(inverse[1].xyz, root)) / w;
    let pixel = local_pixel(inverse, local, w);
    let clip = clip_amount(item.flags.y, item.clip_stop.x, root);
    var color: vec4f;
    switch item.flags.z {
        case 1u: { color = border(item, local, pixel); }
        case 2u: { color = shadow(item, local, pixel); }
        case 3u: { color = gradient(item, local, pixel); }
        case 4u: { color = sample_image(item, local, pixel); }
        case 5u: {
            let coverage = text_coverage(sample_image(item, local, pixel).r, item.colors[0].rgb);
            color = premul(item.colors[0]) * coverage;
        }
        case 6u: { color = sample_path(item, local); }
        default: { color = premul(item.colors[0]) * aa(rounded_distance(local, item.shape, item.radii), pixel); }
    }
    return color * clip;
}
