// SPDX-License-Identifier: MIT

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use bytemuck::{Pod, Zeroable};
use squark_paint::{
    BorderStyle, ClipId, ClipShape, CommittedFrame, EffectId, GradientGeometry, ImageSource,
    InversePlane, Matrix, PaintColor, Primitive, PropertyRefs, RoundedRect, SamplingMode,
    resolve_gradient,
};
use squark_types::{
    Point, Rect, Size,
    fragment::{BackgroundImage, FragmentId},
    render::ResourceKey,
};

use super::{
    background::{Tile, tile},
    color::{RAMP_WIDTH, gradient_ramp, srgb},
    depth,
    paths::Paths,
    resources::{Resources, texture},
};
use crate::{Error, SCENE_FORMAT, extent};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(super) struct Instance {
    bounds: [f32; 4],
    shape: [f32; 4],
    radii: [[f32; 4]; 2],
    inner: [f32; 4],
    inner_radii: [[f32; 4]; 2],
    colors: [[f32; 4]; 4],
    params: [f32; 4],
    extra: [f32; 4],
    texels: [f32; 4],
    sample_rect: [f32; 4],
    tile: [f32; 4],
    period: [f32; 4],
    flags: [u32; 4],
    clip_stop: [u32; 4],
}

impl Instance {
    fn new(bounds: Rect, props: PropertyRefs, kind: u32) -> Self {
        Self {
            bounds: rect(bounds),
            flags: [props.transform.0, props.clip.0, kind, 0],
            ..Self::zeroed()
        }
    }

    fn shape(&mut self, shape: RoundedRect) {
        self.shape = rect(shape.rect);
        self.radii = radii(shape);
    }

    fn tile(&mut self, tile: Tile) {
        self.tile = rect(tile.rect);
        self.period[..2].copy_from_slice(&tile.period);
        self.period[2] = 1.0;
    }

    fn gradient(
        &mut self,
        shape: RoundedRect,
        geometry: GradientGeometry,
        range: [f32; 2],
        repeating: bool,
    ) {
        self.flags[2] = 3;
        self.shape(shape);
        let (kind, params) = gradient_geometry(geometry);
        self.flags[3] = kind | ((repeating as u32) << 2);
        self.params = params;
        self.extra[..2].copy_from_slice(&range);
    }

    fn inner(&mut self, shape: RoundedRect) {
        self.inner = rect(shape.rect);
        self.inner_radii = radii(shape);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(super) struct RasterTransform {
    forward: Matrix,
    inverse: [[f32; 4]; 3],
}

impl RasterTransform {
    pub fn new(forward: Matrix) -> Self {
        Self {
            forward,
            inverse: inverse_plane(forward).unwrap_or_default(),
        }
    }
}

fn inverse_plane(world: Matrix) -> Option<[[f32; 4]; 3]> {
    let InversePlane(inverse) = InversePlane::new(world)?;
    Some(inverse.map(|[a, b, c]| [a as f32, b as f32, c as f32, 0.0]))
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(super) struct Clip {
    inverse: [[f32; 4]; 3],
    shape: [f32; 4],
    radii: [[f32; 4]; 2],
    texels: [f32; 4],
    flags: [u32; 4],
}

#[derive(Debug)]
pub(super) struct Batch {
    pub indices: Range<u32>,
    pub effect: EffectId,
    pub texture: usize,
    pub scissor: [u32; 4],
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Masks {
    pub composite: Option<u32>,
    pub backdrop: Option<u32>,
}

#[derive(Debug)]
pub(super) struct Prepared {
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    pub textures: Vec<wgpu::TextureView>,
    pub matrices: Vec<Matrix>,
    pub clips: Vec<Clip>,
    pub masks: Vec<Masks>,
}

impl Prepared {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &Resources,
        frame: &CommittedFrame,
        (paths, matrices): (&Paths, Vec<Matrix>),
        placeholder: &wgpu::TextureView,
        warned_3d: &mut HashSet<FragmentId>,
    ) -> Result<Self, Error> {
        let viewport = frame.viewport;
        let scale = viewport.scale as f32;
        let list = &frame.frame.display_list;
        let props = &frame.frame.property_trees;
        let mut scissor = Vec::with_capacity(props.clips.len());
        let mut clips = Vec::with_capacity(props.clips.len());
        for (index, node) in props.clips.iter().enumerate() {
            let mut clip = Clip::zeroed();
            let mut bounds = scissor.get(node.parent.0 as usize).copied().unwrap_or([
                0,
                0,
                viewport.device[0],
                viewport.device[1],
            ]);
            clip.flags[0] = node.parent.0;
            let world = matrices[node.space.0 as usize];
            if let Some(inverse) = inverse_plane(world) {
                clip.inverse = inverse;
                match &node.shape {
                    Some(ClipShape::RoundedRect(shape)) => {
                        let shape = snap_shape(*shape, world, scale);
                        clip.shape = rect(shape.rect);
                        clip.radii = radii(shape);
                        // Square overflow takes the same scissor fast path as a rect.
                        if clip.radii == [[0.0; 4]; 2] {
                            bounds = scissor_rect(bounds, shape.rect, world, scale);
                        }
                    }
                    Some(ClipShape::Rect(shape)) => {
                        let shape = snap_box(*shape, world, scale);
                        clip.shape = rect(shape);
                        bounds = scissor_rect(bounds, shape, world, scale);
                    }
                    Some(ClipShape::AxisAligned { rect: shape, axes }) => {
                        clip.shape = rect(snap_box(*shape, world, scale));
                        clip.flags[1..].copy_from_slice(&[1, axes[0] as u32, axes[1] as u32]);
                    }
                    Some(ClipShape::Path(..)) => {
                        if let Some(sprite) = paths.clips[index] {
                            clip.shape = rect(sprite.bounds);
                            clip.texels = sprite.texels.map(|p| p as f32);
                            clip.flags[1] = 2;
                            clip.flags[2] = sprite.aligned as u32;
                        } else {
                            clip.flags[1] = 3;
                        }
                    }
                    None => {}
                }
            } else {
                clip.flags[1] = 3;
            }
            scissor.push(bounds);
            clips.push(clip);
        }
        let ramps = Ramps::new(device, queue, list, placeholder)?;
        let mut scene = Self {
            instances: Vec::new(),
            batches: Vec::new(),
            textures: vec![placeholder.clone(), paths.view.clone(), ramps.view],
            matrices,
            clips,
            masks: Vec::new(),
        };
        let prefix = frame.frame.resource_updates;
        let full = [0, 0, viewport.device[0], viewport.device[1]];
        let order = depth::paint_order(list, props, &scene.matrices, warned_3d);
        let scrolls = &props.scrolls;
        for index in order {
            let item = &list[index];
            let props_ref = item.properties;
            let stop = clip_stop(
                &props.clips,
                props_ref.clip,
                props.effects[props_ref.effect.0 as usize].clip,
            );
            let scissor = if stop == ClipId(0) {
                scissor[props_ref.clip.0 as usize]
            } else {
                full
            };
            if scissor[2] == 0 || scissor[3] == 0 {
                continue;
            }
            let mut draw = Instance::new(inflate(item.bounds, 1.0 / scale), props_ref, 0);
            draw.clip_stop[0] = stop.0;
            let props = props_ref;
            match &item.primitive {
                Primitive::RoundedRect { shape, color } => {
                    let mut shape = *shape;
                    // Snap box edges in device-aligned paint spaces. Transforms
                    // that move the box off the device grid retain their geometry.
                    if pixel_aligned(scene.matrices[props.transform.0 as usize], scale) {
                        let snapped = snap_rect(shape.rect, scale);
                        if snapped.size.width <= 0.0 || snapped.size.height <= 0.0 {
                            continue;
                        }
                        shape = RoundedRect::new(snapped, shape.radii);
                        draw.bounds = rect(inflate(snapped, 1.0 / scale));
                    }
                    draw.shape(shape);
                    draw.colors[0] = srgb(*color);
                    scene.push(draw, props.effect, 0, scissor);
                }
                Primitive::Scrollbar { scroll, axis } => {
                    let node = &scrolls[scroll.0 as usize];
                    let thumb = node.scrollbar_thumb(*axis, item.bounds);
                    for (rect, gray) in [(item.bounds, 241.0 / 255.0), (thumb, 193.0 / 255.0)] {
                        let mut bar = draw;
                        bar.bounds = self::rect(inflate(rect, 1.0 / scale));
                        bar.shape(RoundedRect::new(rect, Default::default()));
                        bar.colors[0] = srgb(PaintColor::srgb([gray, gray, gray, 1.0]));
                        scene.push(bar, props.effect, 0, scissor);
                    }
                }
                Primitive::Border { shape, sides } => {
                    draw.flags[2] = 1;
                    let mut outer = *shape;
                    draw.params = sides.map(|side| side.width.max(0.0));
                    let mut inner = shape.inset(draw.params);
                    if pixel_aligned(scene.matrices[props.transform.0 as usize], scale) {
                        let snapped = snap_rect(outer.rect, scale);
                        if snapped.size.width <= 0.0 || snapped.size.height <= 0.0 {
                            continue;
                        }
                        // The curved corners keep analytic coverage. Snap the
                        // straight outer and inner edges in the same paint space.
                        outer = RoundedRect::new(snapped, outer.radii);
                        inner = RoundedRect::new(snap_rect(inner.rect, scale), inner.radii);
                        draw.params = [
                            inner.rect.origin.y - outer.rect.origin.y,
                            outer.rect.max_x() - inner.rect.max_x(),
                            outer.rect.max_y() - inner.rect.max_y(),
                            inner.rect.origin.x - outer.rect.origin.x,
                        ];
                        draw.bounds = rect(inflate(outer.rect, 1.0 / scale));
                    }
                    draw.shape(outer);
                    draw.inner(inner);
                    draw.extra = sides.map(|side| border_style(side.style) as f32);
                    draw.colors = sides.map(|side| srgb(side.color));
                    scene.push(draw, props.effect, 0, scissor);
                }
                Primitive::Shadow { shape, shadow } => {
                    draw.flags[2] = 2;
                    draw.flags[3] = shadow.inset as u32;
                    draw.inner(*shape);
                    let mut shifted = shape.inset(
                        [if shadow.inset {
                            shadow.spread
                        } else {
                            -shadow.spread
                        }; 4],
                    );
                    shifted.rect.origin.x += shadow.offset.x;
                    shifted.rect.origin.y += shadow.offset.y;
                    draw.shape(shifted);
                    draw.colors[0] = srgb(shadow.color);
                    draw.params[0] = shadow.blur * 0.5;
                    scene.push(draw, props.effect, 0, scissor);
                }
                Primitive::GradientRamp {
                    shape,
                    key,
                    geometry,
                    stop_range,
                    repeating,
                } => {
                    let resource = resources.get(ResourceKey::Gradient(*key), prefix)?;
                    let place = &resource.levels[0];
                    let shape =
                        snap_shape(*shape, scene.matrices[props.transform.0 as usize], scale);
                    draw.gradient(shape, *geometry, *stop_range, *repeating);
                    draw.texels = place.rect.map(|p| p as f32);
                    let texture = scene.bind_texture(&place.view);
                    scene.push(draw, props.effect, texture, scissor);
                }
                Primitive::BackgroundImage {
                    shape,
                    origin,
                    layer,
                } => match layer.image {
                    BackgroundImage::Gradient(_) => {
                        if let Some(ramp) = ramps.rows.get(&index) {
                            let shape = snap_shape(
                                *shape,
                                scene.matrices[props.transform.0 as usize],
                                scale,
                            );
                            draw.gradient(shape, ramp.geometry, ramp.range, ramp.repeating);
                            draw.tile(ramp.tile);
                            draw.texels = [0.0, ramp.row as f32, RAMP_WIDTH as f32, 1.0];
                            scene.push(draw, props.effect, 2, scissor);
                        }
                    }
                    BackgroundImage::Image(key) => {
                        let resource = resources.get(ResourceKey::Image(key), prefix)?;
                        let [_, _, w, h] = resource.levels[0].rect;
                        let intrinsic = Size {
                            width: w as f32,
                            height: h as f32,
                        };
                        if let Some(tile) = tile(layer, *origin, Some(intrinsic)) {
                            let src = Rect::new(0.0, 0.0, intrinsic.width, intrinsic.height);
                            let world = scene.matrices[props.transform.0 as usize];
                            let level =
                                mip_level(src, tile.rect, world, scale, SamplingMode::Linear);
                            let placed = &resource.levels[level.min(resource.levels.len() - 1)];
                            draw.flags[2] = 4;
                            draw.flags[3] = 1;
                            draw.shape(snap_shape(*shape, world, scale));
                            draw.tile(tile);
                            draw.texels = placed.rect.map(|p| p as f32);
                            draw.sample_rect = draw.texels;
                            let texture = scene.bind_texture(&placed.view);
                            scene.push(draw, props.effect, texture, scissor);
                        }
                    }
                },
                Primitive::Backdrop => {
                    draw.shape(RoundedRect::new(item.bounds, Default::default()));
                    scene.push(draw, props.effect, 0, scissor);
                }
                Primitive::Image(image) => {
                    let resource = match image.source {
                        ImageSource::Decoded(key) => {
                            resources.get(ResourceKey::Image(key), prefix)?
                        }
                        ImageSource::Canvas { id, .. } => {
                            let Some(canvas) = resources.canvas(id, prefix) else {
                                continue;
                            };
                            canvas
                        }
                    };
                    let Some(original) = resource.levels.first() else {
                        continue;
                    };
                    let world = scene.matrices[props.transform.0 as usize];
                    let level = mip_level(image.src, image.dst, world, scale, image.sampling);
                    let placed = &resource.levels[level.min(resource.levels.len() - 1)];
                    draw.flags[2] = 4;
                    draw.flags[3] = (image.sampling == SamplingMode::Linear) as u32;
                    if matches!(image.source, ImageSource::Canvas { .. }) {
                        draw.flags[3] |= 2;
                    }
                    draw.bounds = rect(item.bounds);
                    draw.shape = rect(image.dst);
                    draw.texels = placed.rect.map(|p| p as f32);
                    let sx = placed.rect[2] as f32 / original.rect[2] as f32;
                    let sy = placed.rect[3] as f32 / original.rect[3] as f32;
                    draw.sample_rect = [
                        draw.texels[0] + image.src.origin.x * sx,
                        draw.texels[1] + image.src.origin.y * sy,
                        image.src.size.width * sx,
                        image.src.size.height * sy,
                    ];
                    let texture = scene.bind_texture(&placed.view);
                    scene.push(draw, props.effect, texture, scissor);
                }
                Primitive::GlyphRun(run) => {
                    for &(key, position) in &run.glyphs {
                        let resource = resources.get(ResourceKey::Glyph(key), prefix)?;
                        let Some(placed) = resource.levels.first() else {
                            continue;
                        };
                        let [bx, by] = resource.bearing.map(|v| v as f32);
                        let [x, y, w, h] = placed.rect.map(|p| p as f32);
                        let [anchor_x, anchor_y] = key.device_origin(position);
                        let dst = Rect::new(
                            (anchor_x + bx) / scale,
                            (anchor_y - by) / scale,
                            w / scale,
                            h / scale,
                        );
                        draw.flags[2] = if resource.coverage { 5 } else { 4 };
                        draw.flags[3] = 1;
                        draw.bounds = rect(dst);
                        draw.shape = draw.bounds;
                        draw.texels = [x, y, w, h];
                        draw.sample_rect = draw.texels;
                        draw.colors[0] = srgb(run.color);
                        let texture = scene.bind_texture(&placed.view);
                        scene.push(draw, props.effect, texture, scissor);
                    }
                }
                Primitive::Path(_) => {
                    if let Some(sprite) = paths.items[index] {
                        draw.flags[2] = 6;
                        draw.flags[3] = sprite.aligned as u32;
                        draw.bounds = rect(sprite.bounds);
                        draw.shape = draw.bounds;
                        draw.texels = sprite.texels.map(|p| p as f32);
                        scene.push(draw, props.effect, 1, scissor);
                    }
                }
            }
        }
        let viewport_rect = Rect::new(0.0, 0.0, full[2] as f32 / scale, full[3] as f32 / scale);
        for (index, effect) in props.effects.iter().enumerate() {
            let ancestor = props.effects[effect.parent.0 as usize].clip;
            let stop = clip_stop(&props.clips, effect.clip, ancestor);
            let basic = PropertyRefs {
                clip: effect.clip,
                ..Default::default()
            };
            let composite = (index != 0 && effect.clip != stop).then(|| {
                scene.push_mask(
                    RoundedRect::new(viewport_rect, Default::default()),
                    basic,
                    stop,
                    scale,
                )
            });
            let backdrop = (index != 0 && !effect.backdrop.is_empty()).then(|| {
                let refs = PropertyRefs {
                    transform: effect.space,
                    ..basic
                };
                scene.push_mask(effect.bounds, refs, stop, scale)
            });
            scene.masks.push(Masks {
                composite,
                backdrop,
            });
        }
        Ok(scene)
    }

    fn push_mask(
        &mut self,
        shape: RoundedRect,
        refs: PropertyRefs,
        stop: ClipId,
        scale: f32,
    ) -> u32 {
        let mut draw = Instance::new(inflate(shape.rect, 1.0 / scale), refs, 0);
        draw.colors[0] = [1.0; 4];
        draw.clip_stop[0] = stop.0;
        draw.shape(shape);
        let index = self.instances.len() as u32;
        self.instances.push(draw);
        index
    }

    fn bind_texture(&mut self, view: &wgpu::TextureView) -> usize {
        self.textures
            .iter()
            .position(|known| known == view)
            .unwrap_or_else(|| {
                self.textures.push(view.clone());
                self.textures.len() - 1
            })
    }

    fn push(&mut self, instance: Instance, effect: EffectId, texture: usize, scissor: [u32; 4]) {
        let index = self.instances.len() as u32;
        self.instances.push(instance);
        if let Some(last) = self.batches.last_mut()
            && last.effect == effect
            && last.texture == texture
            && last.scissor == scissor
        {
            last.indices.end += 1;
        } else {
            self.batches.push(Batch {
                indices: index..index + 1,
                effect,
                texture,
                scissor,
            });
        }
    }
}

fn clip_stop(clips: &[squark_paint::ClipNode], mut clip: ClipId, ancestor: ClipId) -> ClipId {
    while clip != ancestor && clip != ClipId(0) {
        clip = clips[clip.0 as usize].parent;
    }
    clip
}

fn rect(rect: Rect) -> [f32; 4] {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ]
}

fn radii(shape: RoundedRect) -> [[f32; 4]; 2] {
    let [tl, tr, br, bl] = shape.radii;
    [
        [tl.width, tl.height, tr.width, tr.height],
        [br.width, br.height, bl.width, bl.height],
    ]
}

fn inflate(rect: Rect, padding: f32) -> Rect {
    Rect::new(
        rect.origin.x - padding,
        rect.origin.y - padding,
        rect.size.width + 2.0 * padding,
        rect.size.height + 2.0 * padding,
    )
}

fn pixel_aligned(world: Matrix, scale: f32) -> bool {
    world[0] == [1.0, 0.0, 0.0, 0.0]
        && world[1] == [0.0, 1.0, 0.0, 0.0]
        && world[3][3] == 1.0
        && (world[3][0] * scale).fract() == 0.0
        && (world[3][1] * scale).fract() == 0.0
}

fn snap_rect(rect: Rect, scale: f32) -> Rect {
    // Blink rounds each edge independently, with half pixels towards +infinity.
    let px = |value: f32| (f64::from(value) * f64::from(scale) + 0.5).floor() as f32 / scale;
    let x = px(rect.origin.x);
    let y = px(rect.origin.y);
    Rect::new(x, y, px(rect.max_x()) - x, px(rect.max_y()) - y)
}

fn snap_box(rect: Rect, world: Matrix, scale: f32) -> Rect {
    if pixel_aligned(world, scale) {
        snap_rect(rect, scale)
    } else {
        rect
    }
}

fn snap_shape(shape: RoundedRect, world: Matrix, scale: f32) -> RoundedRect {
    let rect = snap_box(shape.rect, world, scale);
    RoundedRect::new(rect, shape.radii)
}

fn scissor_rect(current: [u32; 4], rect: Rect, world: Matrix, scale: f32) -> [u32; 4] {
    if world[0][1] != 0.0
        || world[1][0] != 0.0
        || world[0][3] != 0.0
        || world[1][3] != 0.0
        || world[3][3] <= 0.0
    {
        return current;
    }
    let [x, y, _, w] = world[3];
    let a = [
        (rect.origin.x * world[0][0] + x) * scale / w,
        (rect.origin.y * world[1][1] + y) * scale / w,
    ];
    let b = [
        (rect.max_x() * world[0][0] + x) * scale / w,
        (rect.max_y() * world[1][1] + y) * scale / w,
    ];
    let [x, y, width, height] = current;
    let x0 = (a[0].min(b[0]).floor() as u32).clamp(x, x + width);
    let x1 = (a[0].max(b[0]).ceil() as u32).clamp(x, x + width);
    let y0 = (a[1].min(b[1]).floor() as u32).clamp(y, y + height);
    let y1 = (a[1].max(b[1]).ceil() as u32).clamp(y, y + height);
    [x0, y0, x1 - x0, y1 - y0]
}

fn border_style(style: BorderStyle) -> u32 {
    match style {
        BorderStyle::None => 0,
        BorderStyle::Hidden => 1,
        BorderStyle::Solid => 2,
        BorderStyle::Dotted => 3,
        BorderStyle::Dashed => 4,
        BorderStyle::Double => 5,
        BorderStyle::Groove => 6,
        BorderStyle::Ridge => 7,
        BorderStyle::Inset => 8,
        BorderStyle::Outset => 9,
    }
}

fn gradient_geometry(geometry: GradientGeometry) -> (u32, [f32; 4]) {
    match geometry {
        GradientGeometry::Linear { start, end } => (0, [start.x, start.y, end.x, end.y]),
        GradientGeometry::Radial { center, radii } => {
            (1, [center.x, center.y, radii.width, radii.height])
        }
        GradientGeometry::Conic {
            center,
            start_angle,
        } => (2, [center.x, center.y, start_angle, 0.0]),
    }
}

fn mip_level(src: Rect, dst: Rect, world: Matrix, scale: f32, sampling: SamplingMode) -> usize {
    if sampling == SamplingMode::Nearest {
        return 0;
    }
    let project = |p: Point| {
        let w = world[0][3] * p.x + world[1][3] * p.y + world[3][3];
        [
            (world[0][0] * p.x + world[1][0] * p.y + world[3][0]) / w,
            (world[0][1] * p.x + world[1][1] * p.y + world[3][1]) / w,
        ]
    };
    let origin = project(dst.origin);
    let x = project(Point {
        x: dst.max_x(),
        y: dst.origin.y,
    });
    let y = project(Point {
        x: dst.origin.x,
        y: dst.max_y(),
    });
    let distance = |p: [f32; 2]| (p[0] - origin[0]).hypot(p[1] - origin[1]) * scale;
    (src.size.width / distance(x))
        .max(src.size.height / distance(y))
        .log2()
        .max(0.0) as usize
}

struct Ramp {
    row: u32,
    range: [f32; 2],
    geometry: GradientGeometry,
    repeating: bool,
    tile: Tile,
}

struct Ramps {
    rows: HashMap<usize, Ramp>,
    view: wgpu::TextureView,
}

impl Ramps {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &[squark_paint::DisplayItem],
        placeholder: &wgpu::TextureView,
    ) -> Result<Self, Error> {
        let mut rows = HashMap::new();
        let mut data = Vec::new();
        for (index, item) in list.iter().enumerate() {
            let Primitive::BackgroundImage { layer, origin, .. } = &item.primitive else {
                continue;
            };
            let BackgroundImage::Gradient(value) = &layer.image else {
                continue;
            };
            let Some(tile) = tile(layer, *origin, None) else {
                continue;
            };
            let gradient = resolve_gradient(value, tile.rect);
            let row = (data.len() / (RAMP_WIDTH * 4)) as u32;
            let (range, bytes) = gradient_ramp(&gradient);
            data.extend(bytes);
            rows.insert(
                index,
                Ramp {
                    row,
                    range,
                    geometry: gradient.geometry,
                    repeating: gradient.repeating,
                    tile,
                },
            );
        }

        if rows.is_empty() {
            return Ok(Self {
                rows,
                view: placeholder.clone(),
            });
        }
        let height = (data.len() / (RAMP_WIDTH * 4)) as u32;
        if height > device.limits().max_texture_dimension_2d {
            return Err("frame gradient ramps exceed the atlas extent".into());
        }
        let width = RAMP_WIDTH as u32;
        let texture = texture(device, [width, height], SCENE_FORMAT);
        queue.write_texture(
            texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            extent(width, height),
        );
        Ok(Self {
            rows,
            view: texture.create_view(&Default::default()),
        })
    }
}
