// SPDX-License-Identifier: MIT

@group(0) @binding(0) var<uniform> viewport: vec4f;
@group(0) @binding(1) var atlas: texture_2d<f32>;

struct Instance {
    @location(0) bounds: vec4f,
    @location(1) shape: vec4f,
    @location(2) params: vec4f,
    @location(3) color: vec4f,
    @location(4) second_color: vec4f,
    @location(5) source: vec4f,
}

struct Fragment {
    @builtin(position) position: vec4f,
    @location(0) point: vec2f,
    @location(1) unit: vec2f,
    @location(2) @interpolate(flat) shape: vec4f,
    @location(3) @interpolate(flat) params: vec4f,
    @location(4) @interpolate(flat) color: vec4f,
    @location(5) @interpolate(flat) second_color: vec4f,
    @location(6) @interpolate(flat) source: vec4f,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32, item: Instance) -> Fragment {
    let corners = array(vec2f(0.0, 0.0), vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0));
    let unit = corners[index];
    let point = item.bounds.xy + unit * item.bounds.zw;
    let ndc = point / viewport.xy * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0);
    return Fragment(vec4f(ndc, 0.0, 1.0), point, unit, item.shape, item.params,
                    item.color, item.second_color, item.source);
}

fn round_rect_distance(point: vec2f, rect: vec4f, radius: f32) -> f32 {
    let half_size = rect.zw * 0.5;
    let r = clamp(radius, 0.0, min(half_size.x, half_size.y));
    let q = abs(point - rect.xy - half_size) - half_size + vec2f(r);
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn coverage(distance: f32) -> f32 {
    return clamp(0.5 - distance / max(fwidth(distance), 0.001), 0.0, 1.0);
}

fn with_coverage(color: vec4f, amount: f32) -> vec4f {
    return vec4f(color.rgb, color.a * amount);
}

@fragment
fn quad(in: Fragment) -> @location(0) vec4f {
    return with_coverage(in.color, coverage(round_rect_distance(in.point, in.shape, in.params.x)));
}

@fragment
fn border(in: Fragment) -> @location(0) vec4f {
    let distance = round_rect_distance(in.point, in.shape, in.params.x);
    let outer = coverage(distance);
    let inner = coverage(distance + in.params.y);
    return with_coverage(in.color, max(outer - inner, 0.0));
}

@fragment
fn shadow(in: Fragment) -> @location(0) vec4f {
    let distance = round_rect_distance(in.point, in.shape, in.params.x);
    // Approximate the Gaussian edge integral with a logistic curve.
    let opacity = 1.0 / (1.0 + exp(1.65 * distance / in.params.y));
    return with_coverage(in.color, opacity);
}

@fragment
fn gradient(in: Fragment) -> @location(0) vec4f {
    let t = clamp((in.point.x - in.shape.x) / in.shape.z, 0.0, 1.0);
    let color = mix(in.color, in.second_color, t);
    return with_coverage(color, coverage(round_rect_distance(in.point, in.shape, in.params.x)));
}

@fragment
fn glyph(in: Fragment) -> @location(0) vec4f {
    let local = clamp(floor(in.unit * in.source.zw), vec2f(0.0), in.source.zw - vec2f(1.0));
    let texel = vec2i(in.source.xy + local);
    return with_coverage(in.color, textureLoad(atlas, texel, 0).r);
}
