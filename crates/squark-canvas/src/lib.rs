// SPDX-License-Identifier: MIT
//! Decode and execute document-thread Canvas 2D commands on the render device.

mod hairline;
mod wire;

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use skrifa::raw::{FileRef, TableProvider};
use squark_types::{NodeId, fragment::Filter, render::FontInstanceId};
use vello::{
    Glyph, Scene,
    kurbo::{Affine, BezPath, Cap, Join, Rect, Shape, Stroke, Vec2},
    peniko::{
        Blob, Brush, Color, Compose, Fill, FontData, ImageAlphaType, ImageBrush, ImageData,
        ImageFormat, ImageQuality, ImageSampler,
    },
};

use wire::{Command, Draw, Run};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Source {
    Canvas(NodeId),
    Decoded(u32),
}

/// GPU image at the ordered draw's source version. Decoded images may occupy
/// part of an atlas page, while canvas snapshots own their entire texture.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub texture: wgpu::Texture,
    pub origin: [u32; 2],
    pub size: [u32; 2],
    pub premultiplied: bool,
}

#[derive(Clone, Debug)]
pub struct Font {
    data: FontData,
    size: f32,
    coords: Arc<[i16]>,
    /// Color glyphs already carry their own paint and bypass the grayscale correction.
    color: bool,
}

impl Font {
    pub fn new(bytes: Arc<[u8]>, index: u32, size: f32, coords: Arc<[i16]>) -> Result<Self, Error> {
        if !size.is_finite() || size <= 0.0 {
            return Err("invalid canvas font size".into());
        }
        let face = match FileRef::new(&bytes)? {
            FileRef::Collection(fonts) => fonts.get(index)?,
            FileRef::Font(font) if index == 0 => font,
            FileRef::Font(_) => return Err("canvas font index is outside its collection".into()),
        };
        let color = face.colr().is_ok() || face.sbix().is_ok() || face.cbdt().is_ok();
        Ok(Self {
            data: FontData::new(Blob::new(Arc::new(bytes)), index),
            size,
            coords,
            color,
        })
    }
}

#[derive(Debug)]
pub enum TextPaint {
    Solid([f32; 4]),
    Texture(wgpu::Texture),
}

pub trait Inputs {
    fn image(&self, source: Source) -> Option<Image>;

    fn font(&self, id: FontInstanceId) -> Option<Font>;

    /// Apply the document renderer's grayscale coverage transfer to an
    /// isolated glyph mask, using the text paint's sRGB color per pixel.
    fn colorize_text(&self, mask: wgpu::Texture, paint: TextPaint) -> wgpu::Texture;

    /// Filter an isolated drawing operation in bitmap coordinates. Texture
    /// input and output have straight RGBA8 alpha, as used by Vello.
    fn filter(&self, source: wgpu::Texture, filters: &[Filter]) -> wgpu::Texture;
}

/// Mutable drawing context. The resident bitmap and draw state advance
/// together only after a validated submit finishes encoding.
#[derive(Clone, Debug, Default)]
pub struct Context {
    state: State,
    stack: Vec<State>,
    path: BezPath,
}

#[derive(Clone, Debug)]
struct State {
    matrix: Affine,
    fill: Brush,
    stroke: Brush,
    outline: Stroke,
    alpha: f32,
    compose: Compose,
    quality: ImageQuality,
    clips: Vec<(Arc<BezPath>, Fill)>,
    filters: Arc<[Filter]>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            matrix: Affine::IDENTITY,
            fill: Color::BLACK.into(),
            stroke: Color::BLACK.into(),
            outline: Stroke::new(1.0)
                .with_caps(Cap::Butt)
                .with_join(Join::Miter)
                .with_miter_limit(10.0),
            alpha: 1.0,
            compose: Compose::SrcOver,
            quality: ImageQuality::Medium,
            clips: Vec::new(),
            filters: Arc::new([]),
        }
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    vello: vello::Renderer,
    missing_fonts: HashSet<FontInstanceId>,
    cached_images: HashMap<Source, (Image, ImageData)>,
}

impl fmt::Debug for Renderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CanvasRenderer").finish_non_exhaustive()
    }
}

/// Vello options for a renderer on `device`.
///
/// WARP, Direct3D 12's software adapter ("Microsoft Basic Render Driver", the
/// only D3D12 adapter in VMs without a D3D12 GPU driver), jumps to a null
/// address while executing Vello's `flatten` compute shader, with either the
/// FXC or the DXC shader compiler. On WARP, Vello therefore runs the stages
/// before fine rasterization on the CPU, which is where WARP runs them anyway.
pub fn vello_options(device: &wgpu::Device) -> vello::RendererOptions {
    let info = device.adapter_info();
    vello::RendererOptions {
        antialiasing_support: vello::AaSupport::area_only(),
        use_cpu: vello_stages_on_cpu(info.backend, info.device_type),
        ..Default::default()
    }
}

fn vello_stages_on_cpu(backend: wgpu::Backend, device_type: wgpu::DeviceType) -> bool {
    backend == wgpu::Backend::Dx12 && device_type == wgpu::DeviceType::Cpu
}

impl Renderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, Error> {
        let vello = vello::Renderer::new(device, vello_options(device))?;
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            vello,
            missing_fonts: HashSet::new(),
            cached_images: HashMap::new(),
        })
    }

    /// Replay the stream into a fresh bitmap version. State-only submits
    /// return no replacement and keep the last bitmap version visible.
    pub fn submit(
        &mut self,
        id: NodeId,
        context: &mut Context,
        bitmap: &wgpu::Texture,
        stream: &[u8],
        inputs: &impl Inputs,
    ) -> Result<Option<wgpu::Texture>, Error> {
        let commands = wire::decode(stream)?;
        let mut next = context.clone();
        let mut playback = Playback {
            renderer: self,
            inputs,
            id,
            bitmap: bitmap.clone(),
            scene: Scene::new(),
            overrides: Vec::new(),
            images: HashMap::new(),
            pending: false,
            changed: false,
        };
        playback.play(commands, &mut next)?;
        playback.finish_batch()?;
        *context = next;
        Ok(playback.changed.then(|| playback.bitmap.clone()))
    }

    /// Forget a source whose bitmap was replaced or freed by the queue.
    pub fn forget(&mut self, source: Source) {
        if let Some((_, image)) = self.cached_images.remove(&source) {
            self.vello.unregister_texture(image);
        }
    }

    fn cached_image(&mut self, source: Source, pixels: Image) -> ImageData {
        if let Some((current, image)) = self.cached_images.get(&source)
            && *current == pixels
        {
            return image.clone();
        }
        self.forget(source);
        let image = self.register(&pixels);
        self.cached_images.insert(source, (pixels, image.clone()));
        image
    }

    fn register(&mut self, image: &Image) -> ImageData {
        let [width, height] = image.size;
        let image_data = ImageData {
            data: Blob::new(Arc::new([])),
            format: ImageFormat::Rgba8,
            alpha_type: if image.premultiplied {
                ImageAlphaType::AlphaPremultiplied
            } else {
                ImageAlphaType::Alpha
            },
            width,
            height,
        };
        self.vello.override_image(
            &image_data,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: image.texture.clone(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: image.origin[0],
                    y: image.origin[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            }),
        );
        image_data
    }

    fn render(&mut self, scene: &Scene, size: [u32; 2]) -> Result<wgpu::Texture, Error> {
        let [width, height] = size;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squark canvas bitmap"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.vello.render_to_texture(
            &self.device,
            &self.queue,
            scene,
            &texture.create_view(&Default::default()),
            &vello::RenderParams {
                base_color: Color::TRANSPARENT,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )?;
        // Vello 0.12 takes a fast path for patchless scenes (such as solid
        // paths or text coverage masks) that returns an empty atlas size and
        // replaces its persistent image atlas with a 1x1 texture without
        // invalidating ImageCache residency. Re-mark cached overrides dirty so
        // the next scene that samples them copies them back into the new atlas.
        if scene.encoding().resources.patches.is_empty() {
            for (_, image) in self.cached_images.values() {
                self.vello.mark_override_image_dirty(image);
            }
        }
        Ok(texture)
    }
}

struct Playback<'a, I> {
    renderer: &'a mut Renderer,
    inputs: &'a I,
    id: NodeId,
    bitmap: wgpu::Texture,
    scene: Scene,
    overrides: Vec<ImageData>,
    images: HashMap<Source, ImageData>,
    pending: bool,
    changed: bool,
}

impl<I: Inputs> Playback<'_, I> {
    fn play(&mut self, commands: Vec<Command<'_>>, context: &mut Context) -> Result<(), Error> {
        let mut paths = HashMap::new();
        let mut paints = HashMap::new();
        for command in commands {
            let state = &mut context.state;
            match command {
                Command::Save => context.stack.push(state.clone()),
                Command::Restore => {
                    if let Some(frame) = context.stack.pop() {
                        *state = frame;
                    }
                }
                Command::Begin => context.path.truncate(0),
                Command::Path(command) => command.append(&mut context.path, state.matrix),
                Command::Transform(matrix) => state.matrix = matrix,
                Command::DefinePath(id, path) => {
                    if paths.insert(id, path).is_some() {
                        return Err("duplicate JCV1 path ID".into());
                    }
                }
                Command::DefinePaint(id, paint) => {
                    if paints.insert(id, paint).is_some() {
                        return Err("duplicate JCV1 paint ID".into());
                    }
                }
                Command::SetPaint(stroke, id) => {
                    let paint = paints.get(&id).ok_or("undefined JCV1 paint")?;
                    if stroke {
                        state.stroke = paint.clone()
                    } else {
                        state.fill = paint.clone()
                    }
                }
                Command::DrawPath(draw, id, rule) => {
                    let path = if id == 0 {
                        &context.path
                    } else {
                        paths.get(&id).ok_or("undefined JCV1 path")?
                    };
                    if draw == Draw::Clip {
                        let clip = if id == 0 {
                            path.clone()
                        } else {
                            state.matrix * path
                        };
                        state.clips.push((Arc::new(clip), rule));
                    } else if state.invertible() {
                        // The default path stores points at creation time. Strokes
                        // use the active transform for line width and dash geometry.
                        if id == 0 {
                            self.shape(state, draw, rule, &(state.matrix.inverse() * path))?;
                        } else {
                            self.shape(state, draw, rule, path)?;
                        }
                    }
                }
                Command::DrawRect(draw, rect) if state.invertible() => {
                    if draw == Draw::Clear {
                        self.clear(state, rect);
                    } else if draw == Draw::Stroke && (rect.width() == 0.0 || rect.height() == 0.0)
                    {
                        let mut path = BezPath::new();
                        path.move_to((rect.x0, rect.y0));
                        path.line_to((rect.x1, rect.y1));
                        self.shape(state, draw, Fill::NonZero, &path)?;
                    } else {
                        self.shape(state, draw, Fill::NonZero, &rect)?;
                    }
                }
                Command::Sampling(alpha, compose, quality) => {
                    state.alpha = alpha;
                    state.compose = compose;
                    state.quality = quality;
                }
                Command::Stroke(outline) => state.outline = outline,
                Command::Image(source, src, dst) if state.invertible() => {
                    self.draw_image(state, source, src, dst)?
                }
                Command::Text(stroke, runs) if state.invertible() => {
                    self.text(state, stroke, runs)?
                }
                Command::Pixels(origin, size, rgba) => self.put_pixels(origin, size, rgba),
                Command::Filters(filters) => state.filters = filters.into(),
                Command::DrawRect(..)
                | Command::Image(..)
                | Command::Text(..)
                | Command::IgnoreFilter => {}
            }
        }
        Ok(())
    }

    fn size(&self) -> [u32; 2] {
        [self.bitmap.width(), self.bitmap.height()]
    }

    fn bounds(&self) -> Rect {
        let [w, h] = self.size().map(f64::from);
        Rect::new(0.0, 0.0, w, h)
    }

    fn register_bitmap(&mut self, texture: wgpu::Texture) -> ImageData {
        let image = self.renderer.register(&Image {
            size: [texture.width(), texture.height()],
            texture,
            origin: [0, 0],
            premultiplied: false,
        });
        self.overrides.push(image.clone());
        image
    }

    fn begin_batch(&mut self) {
        if !self.pending {
            let bitmap = self.register_bitmap(self.bitmap.clone());
            self.scene.draw_image(
                image_brush(&bitmap, ImageQuality::Low, 1.0),
                Affine::IDENTITY,
            );
            self.pending = true;
        }
    }

    fn finish_batch(&mut self) -> Result<(), Error> {
        if self.pending {
            self.bitmap = self.renderer.render(&self.scene, self.size())?;
            self.scene.reset();
            self.pending = false;
            self.changed = true;
        }
        Ok(())
    }

    fn draw(&mut self, state: &State, operation: impl FnOnce(&mut Scene)) -> Result<(), Error> {
        self.begin_batch();
        let bounds = self.bounds();
        let scene = &mut self.scene;
        push_clips(scene, state);
        if state.compose != Compose::SrcOver {
            push_composite(scene, state.compose, Affine::IDENTITY, &bounds);
        }
        if state.filters.is_empty() {
            operation(scene);
        } else {
            // No clip or destination participates in the filter input. The
            // resulting bitmap is clipped and composited as one draw below.
            let mut isolated = Scene::new();
            operation(&mut isolated);
            let pixels = self.renderer.render(&isolated, self.size())?;
            let filtered = self.inputs.filter(pixels, &state.filters);
            let image = self.register_bitmap(filtered);
            self.scene.draw_image(
                image_brush(&image, ImageQuality::Low, 1.0),
                Affine::IDENTITY,
            );
        }
        if state.compose != Compose::SrcOver {
            self.scene.pop_layer();
        }
        pop_clips(&mut self.scene, state);
        Ok(())
    }

    fn shape(
        &mut self,
        state: &State,
        draw: Draw,
        rule: Fill,
        shape: &impl Shape,
    ) -> Result<(), Error> {
        if draw == Draw::Stroke
            && let Some((image, transform)) = hairline::rasterize(
                shape,
                &state.outline,
                state.matrix,
                &state.stroke,
                state.alpha,
                self.size(),
            )
        {
            return self.draw(state, |scene| {
                scene.draw_image(image_brush(&image, ImageQuality::Low, 1.0), transform);
            });
        }
        self.draw(state, |scene| {
            if draw == Draw::Stroke {
                scene.stroke(
                    &state.outline,
                    state.matrix,
                    &state.stroke.clone().multiply_alpha(state.alpha),
                    None,
                    shape,
                );
            } else {
                scene.fill(
                    rule,
                    state.matrix,
                    &state.fill.clone().multiply_alpha(state.alpha),
                    None,
                    shape,
                );
            }
        })
    }

    fn clear(&mut self, state: &State, rect: Rect) {
        if rect.width() == 0.0 || rect.height() == 0.0 {
            return;
        }
        let bounds = self.bounds();
        if state.clips.is_empty()
            && state.matrix == Affine::IDENTITY
            && rect.x0.min(rect.x1) <= bounds.x0
            && rect.y0.min(rect.y1) <= bounds.y0
            && rect.x0.max(rect.x1) >= bounds.x1
            && rect.y0.max(rect.y1) >= bounds.y1
        {
            self.scene.reset();
            self.pending = true;
            return;
        }
        self.begin_batch();
        // Erase using the rasterized rectangle's alpha, including antialiasing
        // and the current clip. Vello's Clear operator covers its whole layer.
        push_composite(&mut self.scene, Compose::DestOut, Affine::IDENTITY, &bounds);
        push_clips(&mut self.scene, state);
        self.scene
            .fill(Fill::NonZero, state.matrix, Color::WHITE, None, &rect);
        pop_clips(&mut self.scene, state);
        self.scene.pop_layer();
    }

    fn draw_image(
        &mut self,
        state: &State,
        source: Source,
        src: Rect,
        dst: Rect,
    ) -> Result<(), Error> {
        if src.is_zero_area() || dst.is_zero_area() {
            return Ok(());
        }
        let image = if source == Source::Canvas(self.id) {
            self.finish_batch()?;
            self.register_bitmap(self.bitmap.clone())
        } else if let Some(image) = self.images.get(&source) {
            image.clone()
        } else if let Some(data) = self.inputs.image(source) {
            let image = self.renderer.cached_image(source, data);
            self.images.insert(source, image.clone());
            image
        } else {
            return Ok(());
        };
        let scale = (dst.width() / src.width(), dst.height() / src.height());
        let map = Affine::translate((dst.x0, dst.y0))
            * Affine::scale_non_uniform(scale.0, scale.1)
            * Affine::translate((-src.x0, -src.y0));
        let viable = src.intersect(Rect::new(
            0.0,
            0.0,
            f64::from(image.width),
            f64::from(image.height),
        ));
        if !viable.is_zero_area() {
            let geometry = map.transform_rect_bbox(viable);
            let brush = image_brush(&image, state.quality, state.alpha);
            self.draw(state, |scene| {
                scene.fill(Fill::NonZero, state.matrix, brush, Some(map), &geometry)
            })?;
        }
        Ok(())
    }

    fn text(&mut self, state: &State, stroke: bool, runs: Vec<Run>) -> Result<(), Error> {
        let runs: Vec<_> = runs
            .into_iter()
            .filter_map(|run| {
                if let Some(font) = self.inputs.font(run.font) {
                    Some((font, run.glyphs))
                } else {
                    if self.renderer.missing_fonts.insert(run.font) {
                        eprintln!(
                            "squark-canvas: unavailable shaped font instance {}",
                            run.font.0
                        );
                    }
                    None
                }
            })
            .collect();
        if runs.is_empty() {
            return Ok(());
        }
        let brush = if stroke { &state.stroke } else { &state.fill };
        let mut text = Scene::new();
        for group in runs.chunk_by(|a, b| a.0.color == b.0.color) {
            if group[0].0.color {
                draw_glyphs(&mut text, state, stroke, brush, state.alpha, group);
                continue;
            }
            let mut coverage = Scene::new();
            let white = Brush::Solid(Color::WHITE);
            draw_glyphs(&mut coverage, state, stroke, &white, 1.0, group);
            let mask = self.renderer.render(&coverage, self.size())?;
            let paint = match brush {
                Brush::Solid(color) => {
                    TextPaint::Solid(color.multiply_alpha(state.alpha).components)
                }
                _ => {
                    let mut layer = Scene::new();
                    let bounds = state.matrix.inverse().transform_rect_bbox(self.bounds());
                    let brush = brush.clone().multiply_alpha(state.alpha);
                    layer.fill(Fill::NonZero, state.matrix, &brush, None, &bounds);
                    TextPaint::Texture(self.renderer.render(&layer, self.size())?)
                }
            };
            let texture = self.inputs.colorize_text(mask, paint);
            let image = self.register_bitmap(texture);
            text.draw_image(
                image_brush(&image, ImageQuality::Low, 1.0),
                Affine::IDENTITY,
            );
        }
        self.draw(state, |scene| scene.append(&text, None))
    }

    fn put_pixels(&mut self, [x, y]: [i32; 2], [width, height]: [u32; 2], rgba: &[u8]) {
        if width == 0 || height == 0 {
            return;
        }
        let offset = Vec2::new(f64::from(x), f64::from(y));
        let rect = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let translation = Affine::translate(offset);
        let pixels = ImageData {
            data: Blob::new(Arc::new(rgba.to_vec())),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        self.clear(&State::default(), rect + offset);
        self.scene
            .draw_image(image_brush(&pixels, ImageQuality::Low, 1.0), translation);
    }
}

impl<I> Drop for Playback<'_, I> {
    fn drop(&mut self) {
        for override_image in self.overrides.drain(..) {
            self.renderer.vello.unregister_texture(override_image);
        }
    }
}

impl State {
    fn invertible(&self) -> bool {
        self.matrix.determinant() != 0.0
    }
}

fn draw_glyphs(
    scene: &mut Scene,
    state: &State,
    stroke: bool,
    brush: &Brush,
    alpha: f32,
    runs: &[(Font, Vec<Glyph>)],
) {
    for (font, glyphs) in runs {
        let glyphs_builder = scene
            .draw_glyphs(&font.data)
            .font_size(font.size)
            .transform(state.matrix)
            .normalized_coords(&font.coords)
            .hint(true)
            .brush(brush)
            .brush_alpha(alpha);
        if stroke {
            glyphs_builder.draw(&state.outline, glyphs.iter().copied());
        } else {
            glyphs_builder.draw(Fill::NonZero, glyphs.iter().copied());
        }
    }
}

fn push_composite(scene: &mut Scene, compose: Compose, transform: Affine, clip: &Rect) {
    scene.push_layer(Fill::NonZero, compose, 1.0, transform, clip);
}

fn push_clips(scene: &mut Scene, state: &State) {
    for (path, rule) in &state.clips {
        scene.push_clip_layer(*rule, Affine::IDENTITY, path.as_ref());
    }
}

fn pop_clips(scene: &mut Scene, state: &State) {
    for _ in &state.clips {
        scene.pop_layer();
    }
}

fn image_brush(image: &ImageData, quality: ImageQuality, alpha: f32) -> ImageBrush<&ImageData> {
    ImageBrush {
        image,
        sampler: ImageSampler::new().with_quality(quality).with_alpha(alpha),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vello_runs_its_pre_fine_stages_on_the_cpu_only_on_warp() {
        use wgpu::{Backend, DeviceType};
        assert!(vello_stages_on_cpu(Backend::Dx12, DeviceType::Cpu));
        for device_type in [
            DeviceType::IntegratedGpu,
            DeviceType::DiscreteGpu,
            DeviceType::VirtualGpu,
            DeviceType::Other,
        ] {
            assert!(!vello_stages_on_cpu(Backend::Dx12, device_type));
        }
        // Lavapipe and other software Vulkan or Metal adapters keep the GPU
        // stages, so Linux and macOS rendering is unchanged.
        for backend in [Backend::Vulkan, Backend::Metal] {
            assert!(!vello_stages_on_cpu(backend, DeviceType::Cpu));
            assert!(!vello_stages_on_cpu(backend, DeviceType::DiscreteGpu));
        }
    }
}
