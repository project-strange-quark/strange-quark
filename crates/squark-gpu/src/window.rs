// SPDX-License-Identifier: MIT

use squark_paint::{CommittedFrame, display_items::Frame};
use wgpu::CurrentSurfaceTexture as Current;

use crate::{
    AdapterPreference, Error, Renderer, StaticRenderer, native_instance, select_adapter,
    validate_extent,
};

#[derive(Debug)]
pub struct WindowRenderer {
    presenter: SurfacePresenter,
    renderer: Renderer,
    scene: wgpu::TextureView,
}

impl WindowRenderer {
    pub async fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        frame: &Frame,
        scene_size: [u32; 2],
    ) -> Result<Self, Error> {
        Self::with_surface(WindowSurface::new(window)?, frame, scene_size).await
    }

    pub async fn with_surface(
        WindowSurface { instance, surface }: WindowSurface,
        frame: &Frame,
        scene_size: [u32; 2],
    ) -> Result<Self, Error> {
        let adapter = Renderer::adapter(&instance, Some(&surface)).await?;
        let renderer = Renderer::new(&adapter).await?;
        let presenter = SurfacePresenter::new(surface, &adapter, &renderer.device, scene_size)?;
        let scene = presenter.view(&renderer.render_scene(frame, scene_size));
        Ok(Self {
            presenter,
            renderer,
            scene,
        })
    }

    pub fn adapter_name(&self) -> &str {
        self.renderer.adapter_name()
    }

    /// Return true when a frame was submitted to the surface.
    pub fn present(
        &mut self,
        size: [u32; 2],
        before_present: impl FnOnce(),
    ) -> Result<bool, Error> {
        self.presenter.present(
            &self.renderer.device,
            &self.renderer.queue,
            size,
            &self.scene,
            before_present,
        )
    }
}

/// A window's surface and the instance that owns it.
///
/// Create it where the window system allows. macOS hands out a window's
/// native handle only on the main thread, so a host makes the surface there
/// and moves it to its render thread; elsewhere the render thread can make
/// it, off the startup path.
#[derive(Debug)]
pub struct WindowSurface {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
}

impl WindowSurface {
    pub fn new(window: impl Into<wgpu::SurfaceTarget<'static>>) -> Result<Self, Error> {
        let _span = crate::StartupSpan::new("instance-and-surface");
        let instance = native_instance();
        let surface = instance.create_surface(window)?;
        Ok(Self { instance, surface })
    }
}

#[derive(Debug)]
pub struct StaticWindowRenderer {
    presenter: SurfacePresenter,
    renderer: StaticRenderer,
    scene: Option<wgpu::TextureView>,
}

impl StaticWindowRenderer {
    pub async fn new(
        WindowSurface { instance, surface }: WindowSurface,
        size: [u32; 2],
        preference: AdapterPreference,
    ) -> Result<Self, Error> {
        let adapter_start = crate::StartupSpan::new("adapter");
        let adapter = select_adapter(&instance, Some(&surface), preference).await?;
        drop(adapter_start);
        let renderer = StaticRenderer::new(&adapter).await?;
        let _presenter = crate::StartupSpan::new("surface-pipeline");
        let presenter = SurfacePresenter::new(surface, &adapter, renderer.device(), size)?;
        Ok(Self {
            presenter,
            renderer,
            scene: None,
        })
    }

    pub fn renderer_mut(&mut self) -> &mut StaticRenderer {
        &mut self.renderer
    }

    pub fn render(&mut self, frame: &CommittedFrame) -> Result<(), Error> {
        let scene = self.renderer.render_cached(frame)?;
        self.scene = Some(self.presenter.view(&scene));
        Ok(())
    }

    pub fn present(
        &mut self,
        size: [u32; 2],
        before_present: impl FnOnce(),
    ) -> Result<bool, Error> {
        let Some(scene) = &self.scene else {
            return Ok(false);
        };
        self.presenter.present(
            self.renderer.device(),
            self.renderer.queue(),
            size,
            scene,
            before_present,
        )
    }
}

#[derive(Debug)]
struct SurfacePresenter {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blitter: wgpu::util::TextureBlitter,
    needs_configure: bool,
}

impl SurfacePresenter {
    fn new(
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        [width, height]: [u32; 2],
    ) -> Result<Self, Error> {
        let mut config = surface
            .get_default_config(adapter, width.max(1), height.max(1))
            .ok_or("surface has no default format")?;
        let capabilities = surface.get_capabilities(adapter);
        if let Some(format) = capabilities.formats.iter().find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        }) {
            config.format = *format;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        let blitter = wgpu::util::TextureBlitterBuilder::new(device, config.format)
            .sample_type(wgpu::FilterMode::Linear)
            .build();
        Ok(Self {
            surface,
            config,
            blitter,
            needs_configure: true,
        })
    }

    fn view(&self, scene: &wgpu::Texture) -> wgpu::TextureView {
        // If the surface encodes on write, sample through a decoding view.
        scene.create_view(&wgpu::TextureViewDescriptor {
            format: self
                .config
                .format
                .is_srgb()
                .then_some(wgpu::TextureFormat::Rgba8UnormSrgb),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        })
    }

    fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        [width, height]: [u32; 2],
        scene: &wgpu::TextureView,
        before_present: impl FnOnce(),
    ) -> Result<bool, Error> {
        if width == 0 || height == 0 {
            return Ok(false);
        }
        validate_extent(device, [width, height])?;
        if self.needs_configure || self.config.width != width || self.config.height != height {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(device, &self.config);
            self.needs_configure = false;
        }

        let output = match self.surface.get_current_texture() {
            Current::Success(output) => output,
            Current::Suboptimal(output) => {
                self.needs_configure = true;
                output
            }
            Current::Timeout | Current::Occluded => return Ok(false),
            Current::Outdated | Current::Lost => {
                self.needs_configure = true;
                return Ok(false);
            }
            Current::Validation => {
                return Err("surface acquisition failed validation".into());
            }
        };
        let target = output.texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        self.blitter.copy(device, &mut encoder, scene, &target);
        queue.submit([encoder.finish()]);
        before_present();
        queue.present(output);
        Ok(true)
    }
}
