// SPDX-License-Identifier: MIT

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var backdrop: texture_2d<f32>;
@group(0) @binding(2) var mask: texture_2d<f32>;

struct Params {
    options: vec4u,
    values: vec4f,
    color: vec4f,
}

@group(0) @binding(3) var<uniform> params: Params;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let vertices = array(vec2f(-1.0, -1.0), vec2f(3.0, -1.0), vec2f(-1.0, 3.0));
    return vec4f(vertices[index], 0.0, 1.0);
}

fn straight(p: vec4f) -> vec3f {
    return p.rgb / max(p.a, 1e-6);
}

fn sample_backdrop(p: vec2i) -> vec4f {
    let dimensions = vec2i(textureDimensions(backdrop));
    if any(p < vec2i(0)) || any(p >= dimensions) { return vec4f(0.0); }
    return textureLoad(backdrop, p, 0);
}

fn lum(c: vec3f) -> f32 {
    return dot(c, vec3f(0.3, 0.59, 0.11));
}

fn sat(c: vec3f) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn with_lum(original: vec3f, light: f32) -> vec3f {
    var c = original + vec3f(light - lum(original));
    let low = min(c.r, min(c.g, c.b));
    let high = max(c.r, max(c.g, c.b));
    if low < 0.0 {
        c = light + (c - light) * light / max(light - low, 1e-6);
    }
    if high > 1.0 {
        c = light + (c - light) * (1.0 - light) / max(high - light, 1e-6);
    }
    return c;
}

fn with_sat(c: vec3f, saturation: f32) -> vec3f {
    let low = min(c.r, min(c.g, c.b));
    return (c - low) * saturation / max(sat(c), 1e-6);
}

fn hard_light(b: vec3f, s: vec3f) -> vec3f {
    return select(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), s > vec3f(0.5));
}

fn blend(b: vec3f, s: vec3f, mode: u32) -> vec3f {
    switch mode {
        case 1u: { return b * s; }
        case 2u: { return b + s - b * s; }
        case 3u: { return hard_light(s, b); }
        case 4u: { return min(b, s); }
        case 5u: { return max(b, s); }
        case 6u: {
            return select(min(vec3f(1.0), b / max(1.0 - s, vec3f(1e-6))), vec3f(0.0), b <= vec3f(0.0));
        }
        case 7u: {
            return select(1.0 - min(vec3f(1.0), (1.0 - b) / max(s, vec3f(1e-6))), vec3f(1.0), b >= vec3f(1.0));
        }
        case 8u: { return hard_light(b, s); }
        case 9u: {
            let d = select(sqrt(b), ((16.0 * b - 12.0) * b + 4.0) * b, b <= vec3f(0.25));
            return select(b + (2.0 * s - 1.0) * (d - b), b - (1.0 - 2.0 * s) * b * (1.0 - b), s <= vec3f(0.5));
        }
        case 10u: { return abs(b - s); }
        case 11u: { return b + s - 2.0 * b * s; }
        case 12u: { return with_lum(with_sat(s, sat(b)), lum(b)); }
        case 13u: { return with_lum(with_sat(b, sat(s)), lum(b)); }
        case 14u: { return with_lum(s, lum(b)); }
        case 15u: { return with_lum(b, lum(s)); }
        default: { return s; }
    }
}

fn gaussian(p: vec2i, sigma: f32, vertical: bool, clamp_edges: bool) -> vec4f {
    if sigma <= 0.0 { return textureLoad(source, p, 0); }
    let radius = ceil(sigma * 3.0);
    let step = max(1.0, ceil(radius / 64.0));
    let taps = i32(ceil(radius / step));
    let dim = vec2i(textureDimensions(source));
    var total = vec4f(0.0);
    var weight = 0.0;
    for (var i = -taps; i <= taps; i++) {
        let d = f32(i) * step;
        let w = exp(-0.5 * (d / sigma) * (d / sigma));
        let offset = select(vec2i(i32(d), 0), vec2i(0, i32(d)), vertical);
        let q = p + offset;
        if clamp_edges {
            total += textureLoad(source, clamp(q, vec2i(0), dim - 1), 0) * w;
        } else if all(q >= vec2i(0)) && all(q < dim) {
            total += textureLoad(source, q, 0) * w;
        }
        weight += w;
    }
    return total / weight;
}

fn color_filter(src: vec4f, kind: u32, value: f32) -> vec4f {
    let c = straight(src);
    var rgb = c;
    switch kind {
        case 1u: { rgb = c * value; }
        case 2u: { rgb = (c - 0.5) * value + 0.5; }
        case 3u: { rgb = mix(c, vec3f(dot(c, vec3f(0.2126, 0.7152, 0.0722))), value); }
        case 4u: {
            let sin_a = sin(value);
            let cos_a = cos(value);
            let base = vec3f(0.213, 0.715, 0.072);
            rgb = vec3f(
                dot(c, base + vec3f(0.787, -0.715, -0.072) * cos_a + vec3f(-0.213, -0.715, 0.928) * sin_a),
                dot(c, base + vec3f(-0.213, 0.285, -0.072) * cos_a + vec3f(0.143, 0.140, -0.283) * sin_a),
                dot(c, base + vec3f(-0.213, -0.715, 0.928) * cos_a + vec3f(-0.787, 0.715, 0.072) * sin_a)
            );
        }
        case 5u: { rgb = mix(c, 1.0 - c, value); }
        case 6u: { return src * clamp(value, 0.0, 1.0); }
        case 7u: { rgb = mix(vec3f(dot(c, vec3f(0.213, 0.715, 0.072))), c, value); }
        case 8u: {
            let sepia = vec3f(
                dot(c, vec3f(0.393, 0.769, 0.189)),
                dot(c, vec3f(0.349, 0.686, 0.168)),
                dot(c, vec3f(0.272, 0.534, 0.131))
            );
            rgb = mix(c, sepia, value);
        }
        default: {}
    }
    return vec4f(clamp(rgb, vec3f(0.0), vec3f(1.0)) * src.a, src.a);
}

fn opacity(p: vec2i) -> f32 {
    var amount = clamp(params.values.x, 0.0, 1.0);
    if params.options.w != 0u { amount *= textureLoad(mask, p, 0).a; }
    return amount;
}

@fragment
fn draw(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let p = vec2i(position.xy);
    let src = textureLoad(source, p, 0);
    switch params.options.x {
        case 1u: { return color_filter(src, params.options.y, params.values.x); }
        case 2u: { return gaussian(p, params.values.x, params.options.y != 0u, params.options.z != 0u); }
        case 3u: {
            let delta = params.values.xy;
            let q = vec2f(p) - delta;
            let lo = vec2i(floor(q));
            let t = fract(q);
            let top = mix(sample_backdrop(lo).a, sample_backdrop(lo + vec2i(1, 0)).a, t.x);
            let bottom = mix(sample_backdrop(lo + vec2i(0, 1)).a, sample_backdrop(lo + vec2i(1)).a, t.x);
            let shadow = params.color * mix(top, bottom, t.y);
            if params.options.y != 0u { return shadow; }
            return src + shadow * (1.0 - src.a);
        }
        case 4u: {
            let b = textureLoad(backdrop, p, 0);
            let s = src * opacity(p);
            if params.options.y == 16u { return min(s + b, vec4f(1.0)); }
            let overlap = blend(straight(b), straight(s), params.options.y) * (s.a * b.a);
            return vec4f(
                s.rgb * (1.0 - b.a) + b.rgb * (1.0 - s.a) + overlap,
                s.a + b.a * (1.0 - s.a)
            );
        }
        case 5u: {
            let amount = textureLoad(mask, p, 0).a * opacity(p);
            return mix(textureLoad(backdrop, p, 0), src, amount);
        }
        case 6u: { return vec4f(src.rgb * src.a, src.a); }
        case 7u: { return vec4f(straight(src), src.a); }
        case 8u: {
            var paint = params.color;
            if params.options.y != 0u { paint = textureLoad(backdrop, p, 0); }
            return vec4f(paint.rgb, paint.a * text_coverage(src.a, paint.rgb));
        }
        default: { return src * opacity(p); }
    }
}
