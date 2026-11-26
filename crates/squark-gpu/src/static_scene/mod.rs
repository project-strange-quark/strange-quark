// SPDX-License-Identifier: MIT

mod background;
mod color;
mod depth;
mod effects;
mod instances;
mod paths;
mod resources;
mod tiles;

use std::{collections::HashSet, fmt, sync::mpsc, time::Duration};

use squark_paint::{CommittedFrame, EffectId, EffectNode, world_transforms};
use squark_types::{
    NodeId,
    fragment::{BlendMode, FragmentId},
    render::{ResourceMessage, ResourceSeq},
};
use wgpu::util::DeviceExt;

use self::{
    effects::{Effects, Params},
    instances::{Batch, Masks, Prepared, RasterTransform},
    paths::Paths,
    resources::Resources,
    tiles::{PixelRect, TEXTURE_BUDGET, TileCache, Update, intersection},
};
use crate::{
    AdapterPreference, Error, SCENE_FORMAT, connect, extent, native_instance, readback_region,
    readback_rgba, select_adapter, validate_extent,
};

/// GPU renderer for a committed static document and its ordered resources.
pub struct StaticRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,
    raster: Raster,
    effects: Effects,
    resources: Resources,
    placeholder: wgpu::TextureView,
    vello: Option<vello::Renderer>,
    warned_3d: HashSet<FragmentId>,
    tiles: Option<TileCache>,
    tile_stats: TileStats,
    in_flight: Option<wgpu::SubmissionIndex>,
}

/// How long a frame may wait for the previous frame's GPU work.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

pub use tiles::TileStats;

/// A tightly packed top-down snapshot of the canvas's straight RGBA8 bitmap.
#[derive(Debug)]
pub struct CanvasPixels {
    pub size: [u32; 2],
    pub rgba: Vec<u8>,
}

impl fmt::Debug for StaticRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticRenderer")
            .field("adapter", &self.adapter_info.name)
            .field("resource_watermark", &self.resources.watermark())
            .finish_non_exhaustive()
    }
}

impl StaticRenderer {
    pub async fn headless() -> Result<Self, Error> {
        Self::headless_on(AdapterPreference::Auto).await
    }

    pub async fn headless_on(preference: AdapterPreference) -> Result<Self, Error> {
        let adapter = select_adapter(&native_instance(), None, preference).await?;
        Self::new(&adapter).await
    }

    /// Use an adapter selected for the eventual surface, or the headless adapter.
    pub async fn new(adapter: &wgpu::Adapter) -> Result<Self, Error> {
        let device_start = crate::StartupSpan::new("device");
        let (device, queue) = connect(adapter).await?;
        drop(device_start);
        let placeholder =
            resources::texture(&device, [1, 1], SCENE_FORMAT).create_view(&Default::default());
        let pipelines = crate::StartupSpan::new("static-pipelines");
        let raster = Raster::new(&device);
        let effects = Effects::new(&device);
        drop(pipelines);
        let _atlas = crate::StartupSpan::new("atlas-init");
        Ok(Self {
            raster,
            effects,
            resources: Resources::new(&device),
            device,
            queue,
            adapter_info: adapter.get_info(),
            placeholder,
            vello: None,
            warned_3d: HashSet::new(),
            tiles: None,
            tile_stats: TileStats::default(),
            in_flight: None,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_info.name
    }

    pub fn applied_resources(&self) -> ResourceSeq {
        self.resources.watermark()
    }

    /// Upload the available ordered queue prefix independently of frame delivery.
    pub fn drain_resources(
        &mut self,
        receiver: &mpsc::Receiver<ResourceMessage>,
    ) -> Result<ResourceSeq, Error> {
        self.resources
            .drain(&self.device, &self.queue, &self.effects, receiver)
    }

    pub fn apply_resources(
        &mut self,
        messages: impl IntoIterator<Item = ResourceMessage>,
    ) -> Result<ResourceSeq, Error> {
        self.resources
            .apply(&self.device, &self.queue, &self.effects, messages)
    }

    /// Read a canvas at an already applied resource prefix. A rectangle may
    /// extend outside the bitmap, where its pixels are transparent black.
    pub fn read_canvas(
        &self,
        id: NodeId,
        prefix: ResourceSeq,
        region: Option<([i32; 2], [u32; 2])>,
    ) -> Result<CanvasPixels, Error> {
        self.resources.verify_prefix(prefix)?;
        let bitmap = self.resources.canvas_bitmap(id, prefix);
        let bounds = bitmap.map_or([0, 0], |bitmap| [bitmap.width(), bitmap.height()]);
        let (origin, size) = region.unwrap_or(([0, 0], bounds));
        let clipped = [0, 1].map(|axis| {
            let start = i64::from(origin[axis]);
            let end = start + i64::from(size[axis]);
            let low = start.max(0);
            let high = end.min(i64::from(bounds[axis]));
            (low as u32, (high - low).max(0) as u32)
        });
        let [(x, width), (y, height)] = clipped;
        let mut rgba = if let Some(bitmap) = bitmap.filter(|_| width != 0 && height != 0) {
            readback_region(&self.device, &self.queue, bitmap, [x, y], [width, height])?
        } else {
            Vec::new()
        };
        if [width, height] != size {
            let len = (size[0] as usize)
                .checked_mul(size[1] as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or("canvas readback size overflow")?;
            let mut padded = Vec::new();
            padded.try_reserve_exact(len)?;
            padded.resize(len, 0);
            if !rgba.is_empty() {
                let left = (i64::from(x) - i64::from(origin[0])) as usize;
                let top = (i64::from(y) - i64::from(origin[1])) as usize;
                let stride = size[0] as usize * 4;
                for (row, source) in rgba.chunks_exact(width as usize * 4).enumerate() {
                    let offset = (top + row) * stride + left * 4;
                    padded[offset..offset + source.len()].copy_from_slice(source);
                }
            }
            rgba = padded;
        }
        Ok(CanvasPixels { size, rgba })
    }

    /// Drop resources freed by the oldest prefix that can still be presented.
    /// The caller supplies that prefix after retiring earlier frame references.
    pub fn retire_resources(&mut self, oldest_frame: ResourceSeq) -> Result<(), Error> {
        self.resources.retire_before(&self.device, oldest_frame)
    }

    /// Render into an sRGB-encoded, premultiplied RGBA8 target.
    pub fn render(&mut self, frame: &CommittedFrame) -> Result<wgpu::Texture, Error> {
        let [width, height] = frame.viewport.device;
        let scale = frame.viewport.scale as f32;
        let data = self.prepare(frame)?;
        let size = [width, height];
        let target = Target::new(&self.device, size);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        drop(target.pass(&mut encoder, wgpu::LoadOp::Clear(wgpu::Color::WHITE)));
        if !data.instances.is_empty() {
            let raster = self
                .raster
                .bind(&self.device, &data, &[[0, 0, width, height]], scale)?;
            let mut passes = Passes {
                device: &self.device,
                encoder: &mut encoder,
                raster,
                effects: &self.effects,
                nodes: &frame.frame.property_trees.effects,
                masks: &data.masks,
                size,
                scale,
            };
            passes.group(EffectId(0), &target, &data.batches);
        }
        self.submit_frame(encoder)?;
        Ok(target.texture)
    }

    /// Submit a frame once the previous frame's GPU work has finished, so at
    /// most one frame waits behind the one being encoded. Without this limit
    /// an adapter slower than the host's frame loop (WARP runs on the CPU)
    /// queues frame after frame, each holding its effect targets, until the
    /// device runs out of memory. If the wait fails, the previous frame stays
    /// in flight, so the next frame waits for it again.
    fn submit_frame(&mut self, encoder: wgpu::CommandEncoder) -> Result<(), Error> {
        if let Some(previous) = &self.in_flight {
            self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(previous.clone()),
                timeout: Some(FRAME_TIMEOUT),
            })?;
        }
        self.in_flight = Some(self.queue.submit([encoder.finish()]));
        Ok(())
    }

    fn validate_frame(&self, frame: &CommittedFrame) -> Result<(), Error> {
        let size = frame.viewport.device;
        let scale = frame.viewport.scale;
        validate_extent(&self.device, size)?;
        if !(scale as f32).is_finite() || (scale as f32) <= 0.0 {
            return Err("invalid device pixel ratio".into());
        }
        self.resources.verify_prefix(frame.frame.resource_updates)
    }

    pub fn tile_stats(&self) -> TileStats {
        self.tile_stats
    }

    /// Update the retained 512-pixel tiles and the assembled scene. Effects that
    /// sample across tile edges use the full scene pass.
    pub fn render_cached(&mut self, frame: &CommittedFrame) -> Result<wgpu::Texture, Error> {
        let viewport = frame.viewport;
        self.validate_frame(frame)?;
        let [width, height] = viewport.device.map(u64::from);
        let cache_bytes = width * height * 8;
        if frame.frame.property_trees.effects.len() != 1
            || cache_bytes > TEXTURE_BUDGET.saturating_sub(self.resources.texture_bytes())
        {
            self.tiles = None;
            self.tile_stats = TileStats::default();
            return self.render(frame);
        }
        let mut cache = self
            .tiles
            .take()
            .filter(|cache| cache.viewport == viewport)
            .unwrap_or_else(|| TileCache::new(&self.device, viewport));
        let updates = cache.updates(frame);
        let areas: Vec<_> = cache
            .tiles
            .iter()
            .zip(&updates)
            .filter_map(|(tile, update)| (*update == Update::Rasterize).then_some(tile.area))
            .collect();
        let mut stats = TileStats {
            rasterized: areas.len(),
            ..TileStats::default()
        };
        if updates.iter().any(|update| *update != Update::Keep) {
            let data = (!areas.is_empty())
                .then(|| self.prepare(frame))
                .transpose()?;
            let raster = data
                .as_ref()
                .filter(|data| !data.instances.is_empty())
                .map(|data| {
                    self.raster
                        .bind(&self.device, data, &areas, viewport.scale as f32)
                })
                .transpose()?;
            let mut encoder = self.device.create_command_encoder(&Default::default());
            let mut view = 0;
            // Copies from the old scene complete before any changed tile is
            // composited back into it, including when the scroll shift overlaps.
            for (tile, update) in cache.tiles.iter().zip(&updates) {
                match *update {
                    Update::Keep => stats.reused += 1,
                    Update::Rasterize => {
                        let mut pass = tile
                            .target
                            .pass(&mut encoder, wgpu::LoadOp::Clear(wgpu::Color::WHITE));
                        if let (Some(raster), Some(data)) = (&raster, &data) {
                            raster.draw(&mut pass, &data.batches, tile.area, view);
                        }
                        view += 1;
                    }
                    Update::Shift {
                        source,
                        mut destination,
                    } => {
                        stats.shifted += 1;
                        destination[0] -= tile.area[0];
                        destination[1] -= tile.area[1];
                        tiles::copy(
                            &mut encoder,
                            &cache.scene,
                            source,
                            &tile.target,
                            destination,
                        );
                    }
                }
            }
            for (tile, update) in cache.tiles.iter().zip(updates) {
                if update != Update::Keep {
                    tiles::copy(&mut encoder, &tile.target, [0, 0], &cache.scene, tile.area);
                }
            }
            self.submit_frame(encoder)?;
        } else {
            stats.reused = updates.len();
        }
        cache.previous = Some(frame.clone());
        let scene = cache.scene.texture.clone();
        self.tile_stats = stats;
        self.tiles = Some(cache);
        Ok(scene)
    }

    fn prepare(&mut self, frame: &CommittedFrame) -> Result<Prepared, Error> {
        self.validate_frame(frame)?;
        let scale = frame.viewport.scale;
        let matrices = world_transforms(&frame.frame.property_trees);
        let paths = Paths::prepare(
            &self.device,
            &self.queue,
            &mut self.vello,
            &self.placeholder,
            &frame.frame,
            scale,
            &matrices,
        )?;
        Prepared::new(
            &self.device,
            &self.queue,
            &self.resources,
            frame,
            (&paths, matrices),
            &self.placeholder,
            &mut self.warned_3d,
        )
    }

    /// Return top-down RGBA8 with straight alpha and no row padding.
    pub fn render_rgba(&mut self, frame: &CommittedFrame) -> Result<Vec<u8>, Error> {
        let scene = self.render(frame)?;
        self.read_scene_rgba(&scene)
    }

    pub fn render_cached_rgba(&mut self, frame: &CommittedFrame) -> Result<Vec<u8>, Error> {
        let scene = self.render_cached(frame)?;
        self.read_scene_rgba(&scene)
    }

    pub fn read_scene_rgba(&self, scene: &wgpu::Texture) -> Result<Vec<u8>, Error> {
        let mut pixels = readback_rgba(&self.device, &self.queue, scene)?;
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            if alpha != 0 && alpha != 255 {
                for component in &mut pixel[..3] {
                    *component = ((u32::from(*component) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        Ok(pixels)
    }
}

#[derive(Clone, Debug)]
struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(device: &wgpu::Device, [width, height]: [u32; 2]) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squark static render target"),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        Self {
            view: texture.create_view(&Default::default()),
            texture,
        }
    }

    fn pass<'a>(
        &self,
        encoder: &'a mut wgpu::CommandEncoder,
        load: wgpu::LoadOp<wgpu::Color>,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("squark static render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        })
    }

    fn copy(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        size: [u32; 2],
    ) -> Self {
        let target = Self::new(device, size);
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            target.texture.as_image_copy(),
            self.texture.size(),
        );
        target
    }
}

pub(super) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

#[derive(Debug)]
struct Raster {
    layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl Raster {
    fn new(device: &wgpu::Device) -> Self {
        let buffer = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: binding == 0,
                min_binding_size: None,
            },
            count: None,
        };
        let storage = wgpu::BufferBindingType::Storage { read_only: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("squark static draw bindings"),
            entries: &[
                buffer(
                    0,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::BufferBindingType::Uniform,
                ),
                buffer(1, wgpu::ShaderStages::VERTEX_FRAGMENT, storage),
                buffer(2, wgpu::ShaderStages::FRAGMENT, storage),
                buffer(3, wgpu::ShaderStages::VERTEX_FRAGMENT, storage),
                texture_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("squark static draw textures"),
            entries: &[texture_entry(0)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("squark static draw layout"),
            bind_group_layouts: &[Some(&layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("squark static draw shader"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    crate::TEXT_COVERAGE_WGSL,
                    include_str!("../../shaders/static.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("squark static primitive pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("draw"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("squark atlas sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            layout,
            texture_layout,
            pipeline,
            sampler,
        }
    }

    fn bind<'a>(
        &'a self,
        device: &wgpu::Device,
        data: &Prepared,
        viewports: &[PixelRect],
        scale: f32,
    ) -> Result<BoundRaster<'a>, Error> {
        const VIEWPORT_BYTES: u64 = 32;
        let stride = (device.limits().min_uniform_buffer_offset_alignment as usize)
            .max(VIEWPORT_BYTES as usize);
        let mut viewport = vec![0; viewports.len() * stride];
        for (&[x, y, width, height], bytes) in
            viewports.iter().zip(viewport.chunks_exact_mut(stride))
        {
            let uniform = [
                width as f32 / scale,
                height as f32 / scale,
                scale,
                0.0,
                x as f32,
                y as f32,
                0.0,
                0.0,
            ];
            bytes[..VIEWPORT_BYTES as usize].copy_from_slice(bytemuck::bytes_of(&uniform));
        }
        let upload = |label, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let storage = |label, contents: &[u8]| {
            if contents.len() > device.limits().max_storage_buffer_binding_size as usize {
                return Err(format!("{label} exceeds the GPU storage binding limit"));
            }
            Ok(upload(label, contents, wgpu::BufferUsages::STORAGE))
        };
        let uniform = upload("squark viewport", &viewport, wgpu::BufferUsages::UNIFORM);
        let transforms: Vec<_> = data
            .matrices
            .iter()
            .copied()
            .map(RasterTransform::new)
            .collect();
        let matrices = storage("squark transforms", bytemuck::cast_slice(&transforms))?;
        let clips = storage("squark clips", bytemuck::cast_slice(&data.clips))?;
        let instances = storage("squark instances", bytemuck::cast_slice(&data.instances))?;
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("squark static draw state"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform,
                        offset: 0,
                        size: wgpu::BufferSize::new(VIEWPORT_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: matrices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: clips.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: instances.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&data.textures[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let textures = data
            .textures
            .iter()
            .map(|view| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("squark static draw texture"),
                    layout: &self.texture_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    }],
                })
            })
            .collect();
        Ok(BoundRaster {
            pipeline: &self.pipeline,
            bindings,
            textures,
            stride: stride as u32,
        })
    }
}

struct BoundRaster<'a> {
    pipeline: &'a wgpu::RenderPipeline,
    bindings: wgpu::BindGroup,
    textures: Vec<wgpu::BindGroup>,
    stride: u32,
}

impl BoundRaster<'_> {
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batches: &[Batch], area: PixelRect, view: u32) {
        pass.set_pipeline(self.pipeline);
        pass.set_bind_group(0, &self.bindings, &[view * self.stride]);
        for batch in batches {
            let Some([x, y, w, h]) = intersection(batch.scissor, area) else {
                continue;
            };
            pass.set_scissor_rect(x - area[0], y - area[1], w, h);
            pass.set_bind_group(1, &self.textures[batch.texture], &[]);
            pass.draw(0..4, batch.indices.clone());
        }
    }
}

struct Passes<'a> {
    device: &'a wgpu::Device,
    encoder: &'a mut wgpu::CommandEncoder,
    raster: BoundRaster<'a>,
    effects: &'a Effects,
    nodes: &'a [EffectNode],
    masks: &'a [Masks],
    size: [u32; 2],
    scale: f32,
}

impl Passes<'_> {
    fn group(&mut self, group: EffectId, target: &Target, mut batches: &[Batch]) {
        while let Some(first) = batches.first() {
            if first.effect == group {
                let count = batches
                    .iter()
                    .take_while(|batch| batch.effect == group)
                    .count();
                let walk = &batches[..count];
                self.raster.draw(
                    &mut target.pass(self.encoder, wgpu::LoadOp::Load),
                    walk,
                    [0, 0, self.size[0], self.size[1]],
                    0,
                );
                batches = &batches[count..];
            } else {
                let child = self
                    .child_of(group, first.effect)
                    .expect("effect tree encloses its batches");
                let count = batches
                    .iter()
                    .take_while(|batch| self.child_of(group, batch.effect) == Some(child))
                    .count();
                self.isolated(child, target, &batches[..count]);
                batches = &batches[count..];
            }
        }
    }

    fn child_of(&self, parent: EffectId, mut node: EffectId) -> Option<EffectId> {
        while node != parent && node != EffectId(0) {
            let ancestor = self.nodes[node.0 as usize].parent;
            if ancestor == parent {
                return Some(node);
            }
            node = ancestor;
        }
        None
    }

    /// Rasterize mask instance `index`. Only `scissor` of the mask is drawn;
    /// the rest stays transparent, so callers sample it only inside `scissor`.
    fn mask(&mut self, index: u32, scissor: PixelRect) -> Target {
        let mask = Target::new(self.device, self.size);
        let [w, h] = self.size;
        self.raster.draw(
            &mut mask.pass(self.encoder, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)),
            &[Batch {
                indices: index..index + 1,
                effect: EffectId(0),
                texture: 0,
                scissor,
            }],
            [0, 0, w, h],
            0,
        );
        mask
    }

    /// The device pixels of its parent that compositing isolated group `id`
    /// can change: the union of the scissors its batches draw within. Outside
    /// it the group stays transparent, and premultiplied `over` with a
    /// transparent source leaves the parent exactly as it was. A filter on
    /// the group, or a filter, backdrop or non-normal blend on an effect
    /// nested inside it, can paint beyond the scissors, so those groups use
    /// the whole viewport.
    fn painted_area(&self, id: EffectId, batches: &[Batch]) -> PixelRect {
        let full = [0, 0, self.size[0], self.size[1]];
        if !self.nodes[id.0 as usize].filters.is_empty() {
            return full;
        }
        let mut area: Option<[u32; 4]> = None;
        for batch in batches {
            let mut node = batch.effect;
            while node != id && node != EffectId(0) {
                let nested = &self.nodes[node.0 as usize];
                if !nested.filters.is_empty()
                    || !nested.backdrop.is_empty()
                    || nested.blend != BlendMode::Normal
                {
                    return full;
                }
                node = nested.parent;
            }
            let [x, y, w, h] = batch.scissor;
            let [right, bottom] = [x + w, y + h];
            area = Some(area.map_or([x, y, right, bottom], |[left, top, r, b]| {
                [left.min(x), top.min(y), r.max(right), b.max(bottom)]
            }));
        }
        area.and_then(|[x, y, right, bottom]| intersection([x, y, right - x, bottom - y], full))
            .unwrap_or([0, 0, 0, 0])
    }

    fn isolated(&mut self, id: EffectId, parent: &Target, batches: &[Batch]) {
        let effect = &self.nodes[id.0 as usize];
        let masks = self.masks[id.0 as usize];
        if let Some(index) = masks.backdrop {
            let original = parent.copy(self.device, self.encoder, self.size);
            let filtered = self
                .effects
                .filter(
                    self.device,
                    self.encoder,
                    original.clone(),
                    self.size,
                    self.scale,
                    true,
                )
                .apply(&effect.backdrop);
            let mask = self.mask(index, [0, 0, self.size[0], self.size[1]]);
            self.effects.draw(
                self.device,
                self.encoder,
                parent,
                [&filtered.view, &original.view, &mask.view],
                Params::backdrop(effect.opacity),
                false,
            );
        }
        let group = Target::new(self.device, self.size);
        drop(group.pass(self.encoder, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
        self.group(id, &group, batches);
        let filtered = self
            .effects
            .filter(
                self.device,
                self.encoder,
                group,
                self.size,
                self.scale,
                false,
            )
            .apply(&effect.filters);
        let area = if effect.blend == BlendMode::Normal {
            self.painted_area(id, batches)
        } else {
            [0, 0, self.size[0], self.size[1]]
        };
        if area[2] == 0 || area[3] == 0 {
            return;
        }
        let mask = masks.composite.map(|index| self.mask(index, area));
        let mask_view = mask.as_ref().map_or(&filtered.view, |mask| &mask.view);
        if effect.blend == BlendMode::Normal {
            self.effects.draw_in(
                self.device,
                self.encoder,
                parent,
                [&filtered.view, &filtered.view, mask_view],
                Params::opacity(effect.opacity).with_mask(mask.is_some()),
                true,
                Some(area),
            );
        } else {
            let backdrop = parent.copy(self.device, self.encoder, self.size);
            self.effects.draw(
                self.device,
                self.encoder,
                parent,
                [&filtered.view, &backdrop.view, mask_view],
                Params::blend(effect.blend, effect.opacity).with_mask(mask.is_some()),
                false,
            );
        }
    }
}
