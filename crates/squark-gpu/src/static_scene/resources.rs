// SPDX-License-Identifier: MIT

use std::{collections::HashMap, sync::mpsc, time::Duration};

use guillotiere::{AllocId, AtlasAllocator, size2};
use squark_types::{
    NodeId,
    render::{
        Bitmap, BitmapFormat, FontInstanceId, ImageKey, ResourceKey, ResourceMessage, ResourceSeq,
        ResourceUpdate,
    },
};
use wgpu::util::StagingBelt;

use super::effects::Effects;
use crate::{Error, extent};

#[derive(Clone, Debug)]
pub(super) struct Placement {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub rect: [u32; 4],
    slot: Option<(usize, AllocId)>,
}

struct Page {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    allocator: AtlasAllocator,
    format: wgpu::TextureFormat,
    side: u32,
}

#[derive(Debug)]
pub(super) struct Resource {
    pub levels: Vec<Placement>,
    pub bearing: [i32; 2],
    pub coverage: bool,
    born: ResourceSeq,
    freed: Option<ResourceSeq>,
}

pub(super) struct Resources {
    watermark: ResourceSeq,
    floor: ResourceSeq,
    entries: HashMap<ResourceKey, Resource>,
    fonts: HashMap<FontInstanceId, squark_canvas::Font>,
    canvases: HashMap<NodeId, Vec<Resource>>,
    canvas_states: HashMap<NodeId, squark_canvas::Context>,
    canvas_renderer: Option<squark_canvas::Renderer>,
    pages: Vec<Page>,
    belt: StagingBelt,
}

impl Resources {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            watermark: ResourceSeq(0),
            floor: ResourceSeq(0),
            entries: HashMap::new(),
            fonts: HashMap::new(),
            canvases: HashMap::new(),
            canvas_states: HashMap::new(),
            canvas_renderer: None,
            pages: Vec::new(),
            belt: StagingBelt::new(device.clone(), 1024 * 1024),
        }
    }

    pub fn watermark(&self) -> ResourceSeq {
        self.watermark
    }

    pub fn texture_bytes(&self) -> u64 {
        let atlases: u64 = self
            .pages
            .iter()
            .map(|page| {
                let bytes = if page.format == wgpu::TextureFormat::R8Unorm {
                    1
                } else {
                    4
                };
                u64::from(page.side).pow(2) * bytes
            })
            .sum();
        atlases
            + self
                .entries
                .values()
                .chain(self.canvases.values().flatten())
                .map(|resource| {
                    let bytes = if resource.coverage { 1 } else { 4 };
                    resource
                        .levels
                        .iter()
                        .filter(|level| level.slot.is_none())
                        .map(|level| u64::from(level.rect[2]) * u64::from(level.rect[3]) * bytes)
                        .sum::<u64>()
                })
                .sum::<u64>()
    }

    pub fn verify_prefix(&self, prefix: ResourceSeq) -> Result<(), Error> {
        if prefix < self.floor || prefix > self.watermark {
            return Err(format!(
                "frame resource prefix {} is outside {}..={}",
                prefix.0, self.floor.0, self.watermark.0
            )
            .into());
        }
        Ok(())
    }

    pub fn get(&self, key: ResourceKey, prefix: ResourceSeq) -> Result<&Resource, Error> {
        self.entries
            .get(&key)
            .filter(|value| value.born <= prefix && value.freed.is_none_or(|seq| prefix < seq))
            .ok_or_else(|| format!("missing render resource {key:?} at prefix {}", prefix.0).into())
    }

    pub fn canvas(&self, id: NodeId, prefix: ResourceSeq) -> Option<&Resource> {
        self.canvases
            .get(&id)?
            .iter()
            .rev()
            .find(|version| version.born <= prefix && version.freed.is_none_or(|seq| prefix < seq))
    }

    pub fn canvas_bitmap(&self, id: NodeId, prefix: ResourceSeq) -> Option<&wgpu::Texture> {
        self.canvas(id, prefix)?
            .levels
            .first()
            .map(|item| &item.texture)
    }

    pub fn drain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effects: &Effects,
        receiver: &mpsc::Receiver<ResourceMessage>,
    ) -> Result<ResourceSeq, Error> {
        self.apply(device, queue, effects, receiver.try_iter())
    }

    pub fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effects: &Effects,
        messages: impl IntoIterator<Item = ResourceMessage>,
    ) -> Result<ResourceSeq, Error> {
        let mut encoder = None;
        let result = messages
            .into_iter()
            .try_for_each(|message| self.apply_one(device, queue, effects, &mut encoder, message));
        self.flush_uploads(queue, &mut encoder);
        result.map(|()| self.watermark)
    }

    fn flush_uploads(&mut self, queue: &wgpu::Queue, encoder: &mut Option<wgpu::CommandEncoder>) {
        if let Some(encoder) = encoder.take() {
            self.belt.finish();
            queue.submit([encoder.finish()]);
            self.belt.recall();
        }
    }

    fn apply_one(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effects: &Effects,
        encoder: &mut Option<wgpu::CommandEncoder>,
        message: ResourceMessage,
    ) -> Result<(), Error> {
        let ResourceMessage { seq, update } = message;
        if self.watermark.0.checked_add(1) != Some(seq.0) {
            return Err(format!(
                "resource queue gap after {}, received {}",
                self.watermark.0, seq.0
            )
            .into());
        }
        let (key, bitmap, bearing) = match update {
            ResourceUpdate::PutFont {
                id,
                bytes,
                index,
                size_bits,
                coords,
            } => {
                if self.fonts.contains_key(&id) {
                    return Err(format!("duplicate immutable canvas font {id:?}").into());
                }
                let font =
                    squark_canvas::Font::new(bytes, index, f32::from_bits(size_bits), coords)?;
                self.fonts.insert(id, font);
                self.watermark = seq;
                return Ok(());
            }
            ResourceUpdate::PutGlyph {
                key,
                bitmap,
                bearing,
            } => (ResourceKey::Glyph(key), bitmap, bearing),
            ResourceUpdate::PutImage { key, bitmap } => (ResourceKey::Image(key), bitmap, [0, 0]),
            ResourceUpdate::PutGradient { key, ramp } => (ResourceKey::Gradient(key), ramp, [0, 0]),
            ResourceUpdate::Free {
                key: ResourceKey::Canvas(id),
            } => {
                if let Some(version) = self
                    .canvases
                    .get_mut(&id)
                    .and_then(|items| items.last_mut())
                    && version.freed.is_none()
                {
                    version.freed = Some(seq);
                }
                self.canvas_states.remove(&id);
                if let Some(renderer) = &mut self.canvas_renderer {
                    renderer.forget(squark_canvas::Source::Canvas(id));
                }
                self.watermark = seq;
                return Ok(());
            }
            ResourceUpdate::Free { key } => {
                let value = self
                    .entries
                    .get_mut(&key)
                    .filter(|value| value.freed.is_none())
                    .ok_or_else(|| format!("free of missing render resource {key:?}"))?;
                value.freed = Some(seq);
                if let ResourceKey::Image(key) = key
                    && let Ok(id) = u32::try_from(key.0)
                    && let Some(renderer) = &mut self.canvas_renderer
                {
                    renderer.forget(squark_canvas::Source::Decoded(id));
                }
                self.watermark = seq;
                return Ok(());
            }
            ResourceUpdate::ResizeCanvas { id, size } => {
                self.resize_canvas(device, id, size, seq);
                self.watermark = seq;
                return Ok(());
            }
            ResourceUpdate::SubmitCanvas { id, stream } => {
                // Images uploaded earlier in the prefix must reach the GPU
                // before the canvas samples them.
                self.flush_uploads(queue, encoder);
                self.submit_canvas(device, queue, effects, id, seq, &stream)?;
                self.watermark = seq;
                return Ok(());
            }
        };
        if self.entries.contains_key(&key) {
            return Err(format!("duplicate immutable render resource {key:?}").into());
        }
        let glyph = matches!(key, ResourceKey::Glyph(_));
        let coverage = bitmap.format == BitmapFormat::R8Coverage;
        let gradient = matches!(key, ResourceKey::Gradient(_));
        if (!glyph && coverage) || (gradient && bitmap.size[1] != 1) {
            return Err(format!("invalid bitmap format or size for {key:?}").into());
        }
        validate_bitmap(device, &bitmap, glyph)?;
        let mut levels = Vec::new();
        if !bitmap.size.contains(&0) {
            let encoder =
                encoder.get_or_insert_with(|| device.create_command_encoder(&Default::default()));
            let page = if glyph { 2048 } else { 4096 };
            let mut bitmap = bitmap;
            loop {
                let atlas = glyph || gradient || bitmap.size.iter().all(|&n| n <= 512);
                match self.upload(device, encoder, &bitmap, atlas.then_some(page)) {
                    Ok(place) => levels.push(place),
                    Err(err) => {
                        self.release(levels);
                        return Err(err);
                    }
                }
                if glyph || gradient || bitmap.size == [1, 1] {
                    break;
                }
                bitmap = half_size(&bitmap);
            }
        }
        self.entries.insert(
            key,
            Resource {
                levels,
                bearing,
                coverage,
                born: seq,
                freed: None,
            },
        );
        self.watermark = seq;
        Ok(())
    }

    fn resize_canvas(
        &mut self,
        device: &wgpu::Device,
        id: NodeId,
        size: [u32; 2],
        seq: ResourceSeq,
    ) {
        self.canvas_states
            .insert(id, squark_canvas::Context::default());
        let target = crate::validate_extent(device, size)
            .is_ok()
            .then(|| texture(device, size, crate::SCENE_FORMAT));
        self.add_canvas_version(id, seq, target);
    }

    fn add_canvas_version(&mut self, id: NodeId, seq: ResourceSeq, bitmap: Option<wgpu::Texture>) {
        if let Some(renderer) = &mut self.canvas_renderer {
            renderer.forget(squark_canvas::Source::Canvas(id));
        }
        let versions = self.canvases.entry(id).or_default();
        if let Some(previous) = versions.last_mut().filter(|v| v.freed.is_none()) {
            previous.freed = Some(seq);
        }
        let levels = bitmap
            .map(|texture| Placement {
                view: texture.create_view(&Default::default()),
                rect: [0, 0, texture.width(), texture.height()],
                texture,
                slot: None,
            })
            .into_iter()
            .collect();
        versions.push(Resource {
            levels,
            bearing: [0, 0],
            coverage: false,
            born: seq,
            freed: None,
        });
    }

    fn submit_canvas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        effects: &Effects,
        id: NodeId,
        seq: ResourceSeq,
        stream: &[u8],
    ) -> Result<(), Error> {
        let inputs = CanvasInputs {
            entries: &self.entries,
            fonts: &self.fonts,
            canvases: &self.canvases,
            prefix: self.watermark,
            device,
            queue,
            effects,
        };
        let Some(bitmap) = inputs.canvas(id) else {
            return Ok(());
        };
        let renderer = match &mut self.canvas_renderer {
            Some(renderer) => renderer,
            slot => slot.insert(squark_canvas::Renderer::new(device, queue)?),
        };
        let state = self.canvas_states.entry(id).or_default();
        if let Some(texture) = renderer.submit(id, state, &bitmap.texture, stream, &inputs)? {
            self.add_canvas_version(id, seq, Some(texture));
        }
        Ok(())
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        bitmap: &Bitmap,
        page: Option<u32>,
    ) -> Result<Placement, Error> {
        let format = match bitmap.format {
            BitmapFormat::R8Coverage => wgpu::TextureFormat::R8Unorm,
            BitmapFormat::Rgba8SrgbPremultiplied => crate::SCENE_FORMAT,
        };
        let [width, height] = bitmap.size;
        let limit = device.limits().max_texture_dimension_2d;
        let atlas_side = page
            .map(|side| side.min(limit))
            .filter(|&side| width <= side && height <= side);
        let (texture, placement) = if let Some(side) = atlas_side {
            self.place(device, format, bitmap.size, side)?
        } else {
            let texture = texture(device, bitmap.size, format);
            let view = texture.create_view(&Default::default());
            (
                texture.clone(),
                Placement {
                    texture,
                    view,
                    rect: [0, 0, width, height],
                    slot: None,
                },
            )
        };
        let bytes = width * bytes_per_pixel(bitmap.format);
        let stride = bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let length = u64::from(stride) * u64::from(height);
        if length > device.limits().max_buffer_size {
            self.release([placement]);
            return Err("bitmap exceeds the GPU staging buffer limit".into());
        }
        let slice = self.belt.allocate(
            wgpu::BufferSize::new(length).expect("non-empty bitmap"),
            wgpu::BufferSize::new(u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)).unwrap(),
        );
        {
            let mut mapped = slice.get_mapped_range_mut()?;
            for row in 0..height as usize {
                let src = row * bitmap.stride as usize;
                let dst = row * stride as usize;
                mapped
                    .slice(dst..dst + bytes as usize)
                    .copy_from_slice(&bitmap.pixels[src..src + bytes as usize]);
            }
        }
        let [x, y, ..] = placement.rect;
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: slice.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: slice.offset(),
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            extent(width, height),
        );
        Ok(placement)
    }

    fn place(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        [width, height]: [u32; 2],
        side: u32,
    ) -> Result<(wgpu::Texture, Placement), Error> {
        let size = size2(width as i32, height as i32);
        let found = self
            .pages
            .iter_mut()
            .enumerate()
            .filter(|(_, page)| page.format == format && page.side == side)
            .find_map(|(index, page)| page.allocator.allocate(size).map(|slot| (index, slot)));
        let (index, allocation) = if let Some(found) = found {
            found
        } else {
            let texture = texture(device, [side, side], format);
            let view = texture.create_view(&Default::default());
            let mut allocator = AtlasAllocator::new(size2(side as i32, side as i32));
            let allocation = allocator
                .allocate(size)
                .ok_or("atlas entry exceeds its page")?;
            let index = self.pages.len();
            self.pages.push(Page {
                texture,
                view,
                allocator,
                format,
                side,
            });
            (index, allocation)
        };
        let page = &self.pages[index];
        let origin = allocation.rectangle.min;
        Ok((
            page.texture.clone(),
            Placement {
                texture: page.texture.clone(),
                view: page.view.clone(),
                rect: [origin.x as u32, origin.y as u32, width, height],
                slot: Some((index, allocation.id)),
            },
        ))
    }

    fn release(&mut self, placements: impl IntoIterator<Item = Placement>) {
        for placement in placements {
            if let Some((page, slot)) = placement.slot {
                self.pages[page].allocator.deallocate(slot);
            }
        }
    }

    pub fn retire_before(
        &mut self,
        device: &wgpu::Device,
        watermark: ResourceSeq,
    ) -> Result<(), Error> {
        self.verify_prefix(watermark)?;
        let keys: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, value)| value.freed.is_some_and(|seq| seq <= watermark))
            .map(|(key, _)| *key)
            .collect();
        if !keys.is_empty() {
            device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })?;
            for key in keys {
                let resource = self.entries.remove(&key).expect("retired key is present");
                self.release(resource.levels);
            }
        }
        self.canvases.retain(|_, versions| {
            versions.retain(|version| version.freed.is_none_or(|seq| watermark < seq));
            !versions.is_empty()
        });
        self.floor = watermark;
        Ok(())
    }
}

struct CanvasInputs<'a> {
    entries: &'a HashMap<ResourceKey, Resource>,
    fonts: &'a HashMap<FontInstanceId, squark_canvas::Font>,
    canvases: &'a HashMap<NodeId, Vec<Resource>>,
    prefix: ResourceSeq,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    effects: &'a Effects,
}

impl CanvasInputs<'_> {
    fn canvas(&self, id: NodeId) -> Option<&Placement> {
        self.canvases
            .get(&id)?
            .iter()
            .rev()
            .find(|version| self.live(version))?
            .levels
            .first()
    }

    fn live(&self, version: &Resource) -> bool {
        version.born <= self.prefix && version.freed.is_none_or(|seq| self.prefix < seq)
    }
}

impl squark_canvas::Inputs for CanvasInputs<'_> {
    fn font(&self, id: FontInstanceId) -> Option<squark_canvas::Font> {
        self.fonts.get(&id).cloned()
    }

    fn image(&self, source: squark_canvas::Source) -> Option<squark_canvas::Image> {
        let (placed, premultiplied) = match source {
            squark_canvas::Source::Canvas(id) => (self.canvas(id)?, false),
            squark_canvas::Source::Decoded(id) => {
                let entry = self
                    .entries
                    .get(&ResourceKey::Image(ImageKey(u64::from(id))))?;
                if !self.live(entry) {
                    return None;
                }
                (entry.levels.first()?, true)
            }
        };
        let [x, y, width, height] = placed.rect;
        Some(squark_canvas::Image {
            texture: placed.texture.clone(),
            origin: [x, y],
            size: [width, height],
            premultiplied,
        })
    }

    fn colorize_text(&self, mask: wgpu::Texture, paint: squark_canvas::TextPaint) -> wgpu::Texture {
        self.effects
            .canvas_text_coverage(self.device, self.queue, mask, paint)
    }

    fn filter(
        &self,
        source: wgpu::Texture,
        filters: &[squark_types::fragment::Filter],
    ) -> wgpu::Texture {
        self.effects
            .canvas_filter(self.device, self.queue, source, filters)
    }
}

fn bytes_per_pixel(format: BitmapFormat) -> u32 {
    match format {
        BitmapFormat::R8Coverage => 1,
        BitmapFormat::Rgba8SrgbPremultiplied => 4,
    }
}

fn validate_bitmap(device: &wgpu::Device, bitmap: &Bitmap, allow_empty: bool) -> Result<(), Error> {
    let [width, height] = bitmap.size;
    let limit = device.limits().max_texture_dimension_2d;
    if width > limit
        || height > limit
        || (bitmap.size.contains(&0) && !(allow_empty && bitmap.pixels.is_empty()))
    {
        return Err(format!("invalid bitmap dimensions {width}×{height}").into());
    }
    if bitmap.size.contains(&0) {
        return Ok(());
    }
    let row = width * bytes_per_pixel(bitmap.format);
    let required = u64::from(height - 1) * u64::from(bitmap.stride) + u64::from(row);
    if bitmap.stride < row || (bitmap.pixels.len() as u64) < required {
        return Err("bitmap stride or storage is too short".into());
    }
    Ok(())
}

pub(super) fn texture(
    device: &wgpu::Device,
    [w, h]: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("squark resource texture"),
        size: extent(w, h),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn half_size(bitmap: &Bitmap) -> Bitmap {
    let [width, height] = bitmap.size;
    let [w, h] = [(width / 2).max(1), (height / 2).max(1)];
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 4];
            let mut count = 0;
            for sy in y * height / h..(y + 1) * height / h {
                for sx in x * width / w..(x + 1) * width / w {
                    let at = sy as usize * bitmap.stride as usize + sx as usize * 4;
                    for (sum, value) in sum.iter_mut().zip(&bitmap.pixels[at..at + 4]) {
                        *sum += u32::from(*value);
                    }
                    count += 1;
                }
            }
            pixels.extend(sum.map(|value| ((value + count / 2) / count) as u8));
        }
    }
    Bitmap {
        size: [w, h],
        stride: w * 4,
        format: bitmap.format,
        pixels: pixels.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StaticRenderer, readback_rgba};
    use std::sync::Arc;

    fn shaped_draw(mode: u32) -> Arc<[u8]> {
        let mut stream = Vec::from(b"JCV1");
        stream.extend(60u32.to_le_bytes());
        stream.extend(1u32.to_le_bytes());
        stream.extend([24u8, 0, 0, 0]);
        stream.extend(40u32.to_le_bytes());
        stream.extend(mode.to_le_bytes());
        stream.extend(1u32.to_le_bytes());
        stream.extend(1u64.to_le_bytes());
        stream.extend(1u32.to_le_bytes());
        stream.extend(36u32.to_le_bytes()); // 'A' in Liberation Mono.
        stream.extend(8.0f64.to_le_bytes());
        stream.extend(32.0f64.to_le_bytes());
        stream.into()
    }

    #[test]
    fn canvas_fills_and_strokes_glyphs_from_a_published_font() {
        if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
            return;
        }
        let mut gpu = pollster::block_on(StaticRenderer::headless()).unwrap();
        let canvas = NodeId::new(1, 0).unwrap();
        let (sender, updates) = squark_types::render::resource_channel();
        sender
            .send(ResourceUpdate::PutFont {
                id: FontInstanceId(1),
                bytes: Arc::from(include_bytes!(
                    "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.ttf"
                ).as_slice()),
                index: 0,
                size_bits: 36.0f32.to_bits(),
                coords: Arc::new([]),
            })
            .unwrap();
        let render = |gpu: &mut StaticRenderer, mode| {
            sender
                .send(ResourceUpdate::ResizeCanvas {
                    id: canvas,
                    size: [40, 40],
                })
                .unwrap();
            let prefix = sender
                .send(ResourceUpdate::SubmitCanvas {
                    id: canvas,
                    stream: shaped_draw(mode),
                })
                .unwrap();
            assert_eq!(gpu.drain_resources(&updates).unwrap(), prefix);
            let drawn = &gpu.resources.canvas(canvas, prefix).unwrap().levels[0].texture;
            readback_rgba(gpu.device(), gpu.queue(), drawn).unwrap()
        };
        let fill = render(&mut gpu, 0);
        let stroke = render(&mut gpu, 1);
        let interior = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] == 255)
                .count()
        };
        let edges = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| (1..255).contains(&p[3]))
                .count()
        };
        assert!(edges(&fill) > 20 && edges(&stroke) > 20);
        assert!(interior(&fill) > interior(&stroke) + 20);
    }
}
