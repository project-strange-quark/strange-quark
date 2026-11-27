// SPDX-License-Identifier: MIT
//! The native squark runtime and its offscreen screenshot entry point.

mod canvas;
mod clock;
mod control;
mod document;
mod fullscreen;
mod headless;
mod ime;
mod input;
mod native;
mod pipeline;
mod runtime;
mod services;
mod timeline;
mod wheel;
mod window;

pub use headless::run_headless_control;
pub use runtime::runtime_main;
pub use window::{WindowRequest, run_window};

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, mpsc},
    thread,
    time::Instant,
};

use services::{ClipboardMode, FileDialogMode, FileDialogQueue, MemoryClipboard};
pub use squark_gpu::AdapterPreference as GpuAdapter;
use squark_gpu::StaticRenderer;
use squark_js::{
    DocumentShutdown,
    document::{DocumentTask, InitialViewport},
};
pub use squark_types::host::HostError;
use squark_types::{
    Size,
    render::{FrameTick, Viewport},
};

pub type Png = Vec<u8>;

static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The exit code a page or module asked for with `exit(code)`.
pub fn requested_exit_code() -> i32 {
    EXIT_CODE.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn request_exit_code(code: i32) {
    EXIT_CODE.store(code, std::sync::atomic::Ordering::Release);
}

/// Start this executable again with the same arguments; the caller exits.
pub(crate) fn relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map(drop)
}

/// Initialize the runtime's process-relative monotonic clock at entry.
pub fn process_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

#[derive(Clone, Debug)]
pub struct ScreenshotRequest {
    /// A standalone .sqpak or an executable with a pack trailer.
    pub input: PathBuf,
    /// The CSS viewport size, in CSS pixels.
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
    pub gpu_adapter: GpuAdapter,
    /// An app runtime's native modules. Headless test-control runs start
    /// them; a single-frame screenshot does not.
    pub native: Option<Arc<dyn squark_types::native::NativeDispatcher>>,
}

/// Run the packed document's initial frame without a window system.
pub fn render_headless(request: ScreenshotRequest) -> Result<Png, HostError> {
    let viewport = screenshot_viewport(&request)?;
    let tick = static_tick(viewport);
    let pack = Arc::new(document::open_pack(&request.input)?);
    document::initialize_v8();
    let shutdown = DocumentShutdown::default();
    let (document, mut render, _) = pipeline::connect();
    let (send, readbacks) = mpsc::channel();
    let document = document.with_canvas(move |request| send.send(request).is_ok());
    let mut renderer = pollster::block_on(StaticRenderer::headless_on(request.gpu_adapter))
        .map_err(|error| failure("GpuError", error))?;
    thread::scope(|scope| {
        let document = thread::Builder::new()
            .name("squark-document".into())
            .spawn_scoped(scope, move || {
                let (host_tasks, _host_replies) = mpsc::channel();
                let (task_sender, _tasks) = mpsc::channel();
                let document_shutdown = shutdown.clone();
                let host_service = services::HostService::start(
                    host_tasks,
                    ClipboardMode::Memory(MemoryClipboard::default()),
                    FileDialogMode::Queued(FileDialogQueue::default()),
                    services::FullscreenMode::Recorded(Default::default()),
                    crate::native::RecordedWindow::default().sink(),
                )?;
                let result = (|| {
                    let mut runner = document::runner(
                        &pack,
                        document.painter,
                        document::DocumentChannels {
                            resources: document.resources,
                            host_requests: host_service.sender(),
                            canvas: document.canvas,
                            native: None,
                        },
                        InitialViewport {
                            tick,
                            media: document::desktop_media(None),
                        },
                        false,
                        document::DocumentControl {
                            shutdown: document_shutdown.clone(),
                            frame_demand: Default::default(),
                        },
                        move || tick.vsync,
                    )?;
                    runner.set_network_wakeup(task_sender.clone());
                    runner
                        .frame(tick)
                        .map(|_| ())
                        .map_err(|error| failure("DocumentError", error))
                })();
                document_shutdown.close();
                let _ = task_sender.send(DocumentTask::Shutdown);
                result
            })
            .map_err(|error| failure("ThreadError", error))?;
        for request in readbacks {
            render.read_canvas(&mut renderer, request);
        }
        document::join_scoped_document_thread(document)?
    })?;

    let latched = render
        .adopt(&mut renderer, tick, false)?
        .ok_or_else(|| failure("PaintError", "the static document did not commit a frame"))?;
    let rgba = renderer
        .render_rgba(latched.frame)
        .map_err(|error| failure("GpuError", error))?;
    encode_png(viewport.device, &rgba)
}

fn static_tick(viewport: Viewport) -> FrameTick {
    let now = Instant::now();
    let present_at = now + std::time::Duration::from_millis(16);
    FrameTick {
        sequence: viewport.epoch,
        vsync: now,
        present_at,
        commit_deadline: present_at,
        viewport,
    }
}

fn screenshot_viewport(request: &ScreenshotRequest) -> Result<Viewport, HostError> {
    let css = Size {
        width: request.width as f32,
        height: request.height as f32,
    };
    let device =
        [request.width, request.height].map(|side| (f64::from(side) * request.dpr).round());
    if !request.dpr.is_finite()
        || request.dpr <= 0.0
        || device
            .iter()
            .any(|&side| side < 1.0 || side > f64::from(u32::MAX))
    {
        return Err(failure(
            "ViewportError",
            "width, height and DPR must produce a positive physical extent",
        ));
    }
    Ok(Viewport {
        css,
        device: device.map(|side| side as u32),
        scale: request.dpr,
        epoch: 1,
    })
}

fn encode_png([width, height]: [u32; 2], rgba: &[u8]) -> Result<Png, HostError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder
        .write_header()
        .map_err(|error| failure("PngError", error))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| failure("PngError", error))?;
    writer
        .finish()
        .map_err(|error| failure("PngError", error))?;
    Ok(bytes)
}

fn failure(name: &str, detail: impl std::fmt::Display) -> HostError {
    HostError {
        name: name.into(),
        message: detail.to_string(),
    }
}
