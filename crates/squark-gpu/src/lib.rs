// SPDX-License-Identifier: MIT
//! GPU rendering, presentation and readback through one native wgpu backend
//! per platform: Vulkan on Linux, Metal on macOS, Direct3D 12 on Windows.

mod primitives;
mod startup;
mod static_scene;
mod window;

use std::{sync::mpsc, time::Duration};

use primitives::Primitives;
use squark_paint::display_items::Frame;
pub use startup::StartupSpan;
pub use static_scene::{CanvasPixels, StaticRenderer, TileStats};
pub use window::{StaticWindowRenderer, WindowRenderer, WindowSurface};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// WGSL source for `text_coverage(mask, srgb)`, shared by text renderers.
pub const TEXT_COVERAGE_WGSL: &str = include_str!("../shaders/text_coverage.wgsl");

const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The backend Strange Quark renders through on this platform. Cargo.toml
/// enables the matching wgpu feature.
const fn native_backend() -> wgpu::Backends {
    if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN
    }
}

/// Human-readable name of [`native_backend`], for errors and logs.
pub const fn native_backend_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Metal"
    } else if cfg!(windows) {
        "Direct3D 12"
    } else {
        "Vulkan"
    }
}

fn native_instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = native_backend();
    wgpu::Instance::new(descriptor)
}

#[derive(Debug)]
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    primitives: Primitives,
    adapter_info: wgpu::AdapterInfo,
}

impl Renderer {
    pub async fn headless() -> Result<Self, Error> {
        let adapter = Self::adapter(&native_instance(), None).await?;
        Self::new(&adapter).await
    }

    async fn adapter(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<wgpu::Adapter, Error> {
        Ok(instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                ..Default::default()
            })
            .await?)
    }

    async fn new(adapter: &wgpu::Adapter) -> Result<Self, Error> {
        let (device, queue) = connect(adapter).await?;
        let primitives = Primitives::new(&device);
        Ok(Self {
            device,
            queue,
            primitives,
            adapter_info: adapter.get_info(),
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_info.name
    }

    fn render_scene(&self, frame: &Frame, [width, height]: [u32; 2]) -> wgpu::Texture {
        let scene = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squark scene"),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });

        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.primitives.draw(
            &self.device,
            &self.queue,
            &mut encoder,
            &scene.create_view(&Default::default()),
            frame,
            [width, height],
        );
        self.queue.submit([encoder.finish()]);
        scene
    }

    /// Draw an opaque display-item frame and return top-down, tightly packed RGBA8.
    pub fn render_rgba(&self, frame: &Frame, size: [u32; 2]) -> Result<Vec<u8>, Error> {
        validate_extent(&self.device, size)?;
        let scene = self.render_scene(frame, size);
        readback_rgba(&self.device, &self.queue, &scene)
    }
}

async fn connect(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), Error> {
    Ok(adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await?)
}

fn validate_extent(device: &wgpu::Device, [width, height]: [u32; 2]) -> Result<(), Error> {
    let limit = device.limits().max_texture_dimension_2d;
    if width == 0 || height == 0 || width > limit || height > limit {
        return Err(format!("invalid render extent {width}×{height} (limit {limit})").into());
    }
    Ok(())
}

fn readback_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &wgpu::Texture,
) -> Result<Vec<u8>, Error> {
    readback_region(
        device,
        queue,
        scene,
        [0, 0],
        [scene.width(), scene.height()],
    )
}

fn readback_region(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    [x, y]: [u32; 2],
    [width, height]: [u32; 2],
) -> Result<Vec<u8>, Error> {
    let row_bytes = width * 4;
    let padded_row = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer_size = u64::from(padded_row) * u64::from(height);
    if buffer_size > device.limits().max_buffer_size {
        return Err("readback exceeds the GPU staging buffer limit".into());
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("squark readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            origin: wgpu::Origin3d { x, y, z: 0 },
            ..texture.as_image_copy()
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        extent(width, height),
    );

    let submission = queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    let timeout = Duration::from_secs(10);
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: Some(timeout),
    })?;
    receiver.recv_timeout(timeout)??;

    let mapped = slice.get_mapped_range()?;
    let mut rgba = Vec::with_capacity(row_bytes as usize * height as usize);
    for row in mapped.chunks_exact(padded_row as usize) {
        rgba.extend_from_slice(&row[..row_bytes as usize]);
    }
    drop(mapped);
    staging.unmap();
    Ok(rgba)
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn vello_accepts_our_wgpu_device() {
        let _make_renderer =
            |device: &wgpu::Device| vello::Renderer::new(device, vello::RendererOptions::default());
    }
}

/// Selection among the native-backend adapters visible to the runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdapterPreference {
    #[default]
    Auto,
    Software,
    Hardware,
}

impl std::str::FromStr for AdapterPreference {
    type Err = &'static str;

    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "auto" => Ok(Self::Auto),
            "software" => Ok(Self::Software),
            "hardware" => Ok(Self::Hardware),
            _ => Err("expected auto, software, or hardware"),
        }
    }
}

async fn select_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    preference: AdapterPreference,
) -> Result<wgpu::Adapter, Error> {
    if preference == AdapterPreference::Auto {
        return Renderer::adapter(instance, surface).await;
    }
    use wgpu::DeviceType as Device;
    let adapters = instance.enumerate_adapters(native_backend()).await;
    adapters
        .into_iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .filter_map(|adapter| {
            let priority = match (preference, adapter.get_info().device_type) {
                (AdapterPreference::Software, Device::Cpu) => 0,
                (AdapterPreference::Hardware, Device::DiscreteGpu) => 0,
                (AdapterPreference::Hardware, Device::IntegratedGpu) => 1,
                (AdapterPreference::Hardware, Device::VirtualGpu) => 2,
                _ => return None,
            };
            Some((priority, adapter))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, adapter)| adapter)
        .ok_or_else(|| {
            format!(
                "no compatible {preference:?} {} adapter",
                native_backend_name()
            )
            .into()
        })
}
