// SPDX-License-Identifier: MIT

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::json;
use squark_assets::Pack;
use squark_compositor::HitSnapshots;
use squark_gpu::{StaticWindowRenderer, WindowSurface};
use squark_js::{DocumentShutdown, document::DocumentTask};
use squark_paint::HitSnapshot;
use squark_types::{
    Size,
    host::{HostValue, Input, InputTask},
    render::{FrameId, FrameTick, Viewport},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Ime, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use crate::{
    GpuAdapter, HostError,
    canvas::PendingCanvasRequests,
    clock::{DEFAULT_REFRESH, FrameClock, FrameWake, TickChannel},
    control::{self, Call, Reply, Request, Server},
    document::{
        DocumentControl, DocumentThread, HostRuntime, desktop_media, initialize_v8,
        join_document_thread, manifest, open_pack, request_document_stop, run_packed,
    },
    failure,
    fullscreen::FullscreenTracker,
    ime::{CursorArea, Edit, ImeState},
    input::InputRouter,
    pipeline::{self, RenderPipeline},
    services::{
        ClipboardMode, FileDialogMode, FileDialogQueue, FullscreenMode, MemoryClipboard,
        RecordedFullscreen,
    },
    timeline::Timeline,
};

#[derive(Clone, Debug)]
pub struct WindowRequest {
    pub input: PathBuf,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub title: Option<String>,
    pub gpu_adapter: GpuAdapter,
    pub exit_after_first_frame: bool,
    pub test_control: Option<PathBuf>,
}

pub fn run_window(request: WindowRequest) -> Result<(), HostError> {
    crate::process_start();
    let setup_start = squark_gpu::StartupSpan::new("pack-v8-and-event-loop");
    let pack = Arc::new(open_pack(&request.input)?);
    let settings = WindowSettings::load(manifest(&pack)?, &request)?;
    initialize_v8();
    let events = EventLoop::<Event>::with_user_event()
        .build()
        .map_err(|error| failure("WindowError", error))?;
    drop(setup_start);
    let mut app = App {
        proxy: events.create_proxy(),
        clipboard: MemoryClipboard::default(),
        file_dialogs: FileDialogQueue::default(),
        recorded_fullscreen: RecordedFullscreen::default(),
        fullscreen: FullscreenTracker::default(),
        request,
        pack,
        settings,
        window: None,
        server: None,
        threads: None,
        viewport: ViewportState::default(),
        input: InputRouter::default(),
        ime: ImeState::default(),
        control_ime_pending: false,
        redraw_at: None,
        occluded: false,
        shown: false,
        error: None,
    };
    let result = events
        .run_app(&mut app)
        .map_err(|error| failure("WindowError", error));
    let shutdown = app.threads.take().map_or(Ok(()), Threads::stop);
    result?;
    shutdown?;
    app.error.map_or(Ok(()), Err)
}

#[derive(Debug, Default, Deserialize)]
struct WindowManifest {
    app_name: Option<String>,
    window: Option<WindowDefaults>,
}

#[derive(Debug, Default, Deserialize)]
struct WindowDefaults {
    title: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug)]
struct WindowSettings {
    app_id: String,
    title: String,
    size: LogicalSize<u32>,
}

impl WindowSettings {
    fn load(
        manifest: Option<serde_json::Value>,
        request: &WindowRequest,
    ) -> Result<Self, HostError> {
        let manifest: WindowManifest = manifest
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| failure("PackError", error))?
            .unwrap_or_default();
        let window = manifest.window.unwrap_or_default();
        let app_id = manifest
            .app_name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "squark-runtime".into());
        let title = request
            .title
            .clone()
            .or(window.title)
            .unwrap_or_else(|| app_id.clone());
        let width = request.width.or(window.width).unwrap_or(800);
        let height = request.height.or(window.height).unwrap_or(600);
        if width == 0 || height == 0 {
            return Err(failure(
                "ViewportError",
                "window dimensions must be positive",
            ));
        }
        Ok(Self {
            app_id,
            title,
            size: LogicalSize::new(width, height),
        })
    }
}

enum Event {
    FirstPresent,
    RequestRedraw(Instant),
    Presented,
    ImeArea(Option<CursorArea>),
    Fullscreen { id: u32, enter: bool },
    Control(Call),
    Failed(HostError),
}

#[derive(Debug)]
enum Command {
    Viewport(Option<Viewport>),
    Redraw {
        at: Instant,
        refresh: Duration,
    },
    Occluded,
    Demand,
    Input {
        task: InputTask,
        mouse: Option<PhysicalPosition<f64>>,
        snapshot: Option<Arc<HitSnapshot>>,
        reply: Option<mpsc::Sender<Reply>>,
    },
    WaitIdle {
        frames: u32,
        reply: mpsc::Sender<Reply>,
    },
    NextPresented(PresentationProbe),
    Screenshot(Capture),
    ReadCanvas(crate::canvas::ReadCanvas),
    Committed,
    Stop,
}

#[derive(Default)]
struct ViewportState {
    active: Option<Viewport>,
    epoch: u64,
}

impl ViewportState {
    fn update(&mut self, size: PhysicalSize<u32>, scale: f64) -> Option<Command> {
        if size.width == 0 || size.height == 0 || !scale.is_finite() || scale <= 0.0 {
            return self.active.take().map(|_| Command::Viewport(None));
        }
        let device = [size.width, size.height];
        if self
            .active
            .is_some_and(|previous| previous.device == device && previous.scale == scale)
        {
            return None;
        }
        self.epoch += 1;
        let viewport = Viewport {
            css: Size {
                width: (f64::from(size.width) / scale) as f32,
                height: (f64::from(size.height) / scale) as f32,
            },
            device,
            scale,
            epoch: self.epoch,
        };
        self.active = Some(viewport);
        Some(Command::Viewport(Some(viewport)))
    }
}

struct App {
    proxy: EventLoopProxy<Event>,
    request: WindowRequest,
    pack: Arc<Pack>,
    settings: WindowSettings,
    window: Option<Arc<Window>>,
    server: Option<Server>,
    threads: Option<Threads>,
    clipboard: MemoryClipboard,
    file_dialogs: FileDialogQueue,
    recorded_fullscreen: RecordedFullscreen,
    fullscreen: FullscreenTracker,
    viewport: ViewportState,
    input: InputRouter,
    ime: ImeState,
    control_ime_pending: bool,
    redraw_at: Option<Instant>,
    occluded: bool,
    shown: bool,
    error: Option<HostError>,
}

impl App {
    fn start(&mut self, events: &ActiveEventLoop) -> Result<(), HostError> {
        let system = WindowSystem::of(events);
        let attrs = system.app_id(
            Window::default_attributes()
                .with_title(&self.settings.title)
                .with_inner_size(self.settings.size)
                .with_visible(!system.presents_while_hidden),
            &self.settings.app_id,
        );
        let window_start = squark_gpu::StartupSpan::new("create-window");
        let window = Arc::new(
            events
                .create_window(attrs)
                .map_err(|error| failure("WindowError", error))?,
        );
        drop(window_start);
        self.threads = Some(Threads::start(
            window.clone(),
            Arc::clone(&self.pack),
            &self.request,
            &self.proxy,
            ControlStores {
                clipboard: self.clipboard.clone(),
                file_dialogs: self.file_dialogs.clone(),
                fullscreen: self.recorded_fullscreen.clone(),
            },
            system,
        )?);
        self.window = Some(window.clone());
        self.resize(window.inner_size(), window.scale_factor());
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>, scale: f64) {
        if let Some(command) = self.viewport.update(size, scale) {
            self.send(command);
        }
        let fullscreen = self
            .window
            .as_ref()
            .is_some_and(|window| window.fullscreen().is_some());
        if let (Some(fullscreen), Some(threads)) =
            (self.fullscreen.observe(fullscreen), &self.threads)
        {
            let _ = threads
                .tasks
                .send(DocumentTask::FullscreenChanged { fullscreen });
        }
    }

    /// Applies a document fullscreen request on the event-loop thread, then
    /// replies; the resize arrives as the usual viewport task.
    fn set_fullscreen(&mut self, id: u32, enter: bool) {
        let result = match &self.window {
            Some(window) => {
                window.set_fullscreen(enter.then_some(Fullscreen::Borderless(None)));
                self.fullscreen.request(enter);
                Ok(HostValue::Done)
            }
            None => Err(failure("WindowError", "the window has closed")),
        };
        if let Some(threads) = &self.threads {
            let _ = threads.tasks.send(DocumentTask::HostReply {
                request: id,
                result,
            });
        }
    }

    fn native_input(&mut self, event: &WindowEvent) {
        let Some(threads) = &self.threads else { return };
        let snapshot = threads
            .hits
            .latest()
            .filter(|hit| Some(hit.viewport) == self.viewport.active);
        if self.shown
            && let Some(mut task) = self.input.event(
                event,
                Instant::now(),
                self.viewport.active,
                snapshot.as_deref(),
            )
        {
            if let Input::Key { composing, .. } = &mut task.input {
                *composing = self.ime.composing();
            }
            let _ = threads.commands.send(Command::Input {
                task,
                mouse: self.input.mouse_position(),
                snapshot: snapshot.clone(),
                reply: None,
            });
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
        ) && let Some(window) = &self.window
        {
            self.input
                .refresh_cursor(window, self.viewport.active, snapshot.as_deref());
        }
    }

    fn ime_area(&mut self, area: Option<CursorArea>) {
        let control_pending = self.control_ime_pending;
        let was_allowed = self.ime.area.is_some();
        if let Some(edit) = self.ime.set_area(area, Instant::now()) {
            self.send_ime(edit);
        }
        if control_pending {
            self.control_ime_pending = self.ime.pending_empty();
        }
        if let Some(window) = &self.window {
            if was_allowed != area.is_some() {
                window.set_ime_allowed(area.is_some());
            }
            if let Some(area) = area {
                area.apply(window);
            }
        }
    }

    fn native_ime(&mut self, ime: Ime) {
        self.control_ime_pending = false;
        if matches!(ime, Ime::Enabled)
            && let (Some(area), Some(window)) = (self.ime.area, &self.window)
        {
            // A fresh text-input binding must receive the area even when it
            // did not change while the window was unfocused.
            area.apply(window);
        }
        for edit in self.ime.event(ime, Instant::now()).into_iter().flatten() {
            self.send_ime(edit);
        }
    }

    fn flush_ime(&mut self) {
        self.control_ime_pending = false;
        if let Some(edit) = self.ime.flush() {
            self.send_ime(edit);
        }
    }

    fn send_ime(&mut self, edit: Edit) {
        let _ = self.queue_ime(edit);
    }

    fn queue_ime(&mut self, edit: Edit) -> Result<(), HostError> {
        let Some(threads) = &self.threads else {
            return Err(failure("ControlError", "runtime closed"));
        };
        let task = self.input.ime(edit);
        threads
            .commands
            .send(Command::Input {
                task,
                mouse: None,
                snapshot: None,
                reply: None,
            })
            .map_err(|_| failure("ThreadError", "render thread closed"))
    }

    fn control_ime(&mut self, input: control::ImeInput) -> Result<serde_json::Value, HostError> {
        let edits = self.ime.control_event(input.into_event(), Instant::now());
        for edit in edits.into_iter().flatten() {
            self.queue_ime(edit)?;
        }
        self.control_ime_pending = self.ime.pending_empty();
        Ok(json!({ "ok": true }))
    }

    fn start_control(&mut self) -> Result<(), HostError> {
        if let Some(path) = &self.request.test_control {
            let proxy = self.proxy.clone();
            let commands = self
                .threads
                .as_ref()
                .expect("window threads started")
                .commands
                .clone();
            self.server = Some(
                Server::start(path, move |call| {
                    if matches!(call.request, Request::NextPresented(_)) {
                        commands
                            .send(Command::NextPresented(PresentationProbe {
                                after: call.received_at,
                                reply: call.reply,
                            }))
                            .is_ok()
                    } else {
                        proxy.send_event(Event::Control(call)).is_ok()
                    }
                })
                .map_err(|error| failure("ControlError", error))?,
            );
        }
        Ok(())
    }

    fn control(
        &mut self,
        events: &ActiveEventLoop,
        Call {
            request,
            reply,
            received_at,
        }: Call,
    ) {
        if !matches!(&request, Request::Ime(_)) {
            self.flush_ime();
        }
        let Some((tasks, hits)) = self
            .threads
            .as_ref()
            .map(|threads| (threads.tasks.clone(), Arc::clone(&threads.hits)))
        else {
            control::send(reply, Err(failure("ControlError", "runtime closed")));
            return;
        };
        match request {
            Request::ClipboardRead {} => {
                control::send(reply, Ok(control::clipboard_read(&self.clipboard)));
            }
            Request::ClipboardWrite { items } => {
                let result = control::clipboard_write(items, &self.clipboard);
                control::send(reply, Ok(result));
            }
            Request::FileDialogQueue { kind, path } => {
                let result = control::file_dialog_queue(kind, path, &self.file_dialogs);
                control::send(reply, Ok(result));
            }
            Request::Fullscreen {} => {
                control::send(reply, Ok(control::fullscreen(&self.recorded_fullscreen)));
            }
            Request::LeaveFullscreen {} => {
                let result = control::leave_fullscreen(&self.recorded_fullscreen, &tasks);
                control::send(reply, result);
            }
            Request::Ime(input) => {
                control::send(reply, self.control_ime(input));
            }
            Request::Quit {} => {
                control::send(reply, Ok(json!({ "ok": true })));
                events.exit();
            }
            Request::Eval { .. } | Request::QueryRect { .. } => {
                control::dom_task(request, &tasks, reply);
            }
            Request::WaitIdle { frames } => self.send(Command::WaitIdle { frames, reply }),
            Request::NextPresented(_) => self.send(Command::NextPresented(PresentationProbe {
                after: received_at,
                reply,
            })),
            Request::Screenshot { path } => self.send(Command::Screenshot(Capture {
                path,
                reply,
                deadline: received_at + control::REQUEST_TIMEOUT,
            })),
            Request::Pointer(_) | Request::Wheel(_) | Request::Key(_) => {
                let Some(viewport) = self.viewport.active else {
                    control::send(
                        reply,
                        Err(failure("ViewportError", "window has no active viewport")),
                    );
                    return;
                };
                let snapshot = hits.latest().filter(|hit| hit.viewport == viewport);
                let task =
                    self.input
                        .synthetic(request, Instant::now(), viewport, snapshot.as_deref());
                self.send(Command::Input {
                    task,
                    mouse: self.input.mouse_position(),
                    snapshot,
                    reply: Some(reply),
                });
                self.refresh_cursor();
            }
        }
    }

    fn refresh_cursor(&mut self) {
        if let (Some(window), Some(threads)) = (&self.window, &self.threads) {
            let snapshot = threads.hits.latest();
            self.input
                .refresh_cursor(window, self.viewport.active, snapshot.as_deref());
        }
    }

    fn send(&self, command: Command) {
        if let Some(threads) = &self.threads {
            let _ = threads.commands.send(command);
        }
    }
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.start(events)
        {
            self.error = Some(error);
            events.exit();
        }
    }

    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.window.as_ref().is_none_or(|window| window.id() != id) {
            return;
        }
        if let WindowEvent::Ime(ime) = event {
            self.native_ime(ime);
            return;
        }
        if !self.control_ime_pending {
            self.flush_ime();
        }
        let window = self.window.as_ref().unwrap();
        match event {
            WindowEvent::CloseRequested => events.exit(),
            WindowEvent::Resized(size) => self.resize(size, window.scale_factor()),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.resize(window.inner_size(), scale_factor)
            }
            WindowEvent::ThemeChanged(theme) => {
                if let Some(threads) = &self.threads {
                    let _ = threads
                        .tasks
                        .send(DocumentTask::Media(desktop_media(Some(theme))));
                }
            }
            WindowEvent::RedrawRequested if !self.occluded => self.send(Command::Redraw {
                at: Instant::now(),
                refresh: refresh_interval(window),
            }),
            WindowEvent::Occluded(occluded) if self.shown => {
                self.occluded = occluded;
                if occluded {
                    self.redraw_at = None;
                    self.send(Command::Occluded);
                } else {
                    window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                if let Some(edit) = self.ime.disable(Instant::now()) {
                    self.send_ime(edit);
                }
                self.native_input(&event);
            }
            event => self.native_input(&event),
        }
    }

    fn user_event(&mut self, events: &ActiveEventLoop, event: Event) {
        match event {
            Event::FirstPresent => {
                let _window_map = squark_gpu::StartupSpan::new("show-window");
                if let Err(error) = self.start_control() {
                    self.error = Some(error);
                    events.exit();
                    return;
                }
                self.shown = true;
                self.redraw_at = None;
                if let Some(window) = &self.window {
                    window.set_visible(true);
                }
                self.refresh_cursor();
                eprintln!("squark-runtime presented its first frame");
                if self.request.exit_after_first_frame {
                    events.exit();
                }
            }
            Event::RequestRedraw(at) if !self.occluded => {
                self.redraw_at = Some(self.redraw_at.map_or(at, |pending| at.min(pending)));
            }
            Event::Presented => self.refresh_cursor(),
            Event::ImeArea(area) => self.ime_area(area),
            Event::Fullscreen { id, enter } => self.set_fullscreen(id, enter),
            Event::Control(call) => self.control(events, call),
            Event::RequestRedraw(_) => {}
            Event::Failed(error) => {
                self.error = Some(error);
                events.exit();
            }
        }
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if !self.control_ime_pending {
            self.flush_ime();
        }
        if let Some(at) = self.redraw_at {
            if at > Instant::now() {
                events.set_control_flow(ControlFlow::WaitUntil(at));
                return;
            }
            self.redraw_at = None;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        events.set_control_flow(ControlFlow::Wait);
    }
}

/// What test control reads and changes in place of the desktop.
struct ControlStores {
    clipboard: MemoryClipboard,
    file_dialogs: FileDialogQueue,
    fullscreen: RecordedFullscreen,
}

struct Threads {
    commands: mpsc::Sender<Command>,
    hits: Arc<HitSnapshots>,
    tasks: mpsc::Sender<DocumentTask>,
    shutdown: DocumentShutdown,
    pending_canvas: Arc<PendingCanvasRequests>,
    document: DocumentThread,
    render: JoinHandle<()>,
}

/// What the window system does for the host, decided once per event loop.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowSystem {
    /// No compositor frame callbacks pace presentation (X11, macOS and
    /// Windows), so the frame clock paces redraws from observed presents.
    self_paced: bool,
    /// The window system places the IME candidate window at the cursor
    /// area the host reports (everywhere but X11).
    ime_cursor_area: bool,
    /// A hidden window can present, so the host maps the window only once
    /// its first frame is on screen and never shows an empty one. Not on
    /// macOS: wgpu's Metal backend acquires no drawable for a window that
    /// isn't visible, so the window maps before its first frame.
    presents_while_hidden: bool,
}

impl WindowSystem {
    #[cfg(target_os = "linux")]
    fn of(events: &ActiveEventLoop) -> Self {
        use winit::platform::x11::ActiveEventLoopExtX11;
        let x11 = events.is_x11();
        Self {
            self_paced: x11,
            ime_cursor_area: !x11,
            presents_while_hidden: true,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn of(_events: &ActiveEventLoop) -> Self {
        Self {
            self_paced: true,
            ime_cursor_area: true,
            presents_while_hidden: !cfg!(target_os = "macos"),
        }
    }

    /// Sets the Wayland app ID, which compositors use to match the window
    /// to its desktop entry. Other window systems have no equivalent.
    #[cfg(target_os = "linux")]
    fn app_id(self, attrs: WindowAttributes, app_id: &str) -> WindowAttributes {
        use winit::platform::wayland::WindowAttributesExtWayland;
        // On Linux only X11 paces itself; the other window system is Wayland.
        let wayland = !self.self_paced;
        if wayland {
            attrs.with_name(app_id, "")
        } else {
            attrs
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn app_id(self, attrs: WindowAttributes, _app_id: &str) -> WindowAttributes {
        attrs
    }
}

impl Threads {
    fn start(
        window: Arc<Window>,
        pack: Arc<Pack>,
        request: &WindowRequest,
        proxy: &EventLoopProxy<Event>,
        stores: ControlStores,
        system: WindowSystem,
    ) -> Result<Self, HostError> {
        // macOS and winit's Windows backend hand out the window's native
        // handle only on the main thread. Elsewhere the render thread makes
        // the surface, off the startup path.
        let surface = if cfg!(any(target_os = "macos", windows)) {
            Some(WindowSurface::new(window.clone()).map_err(|error| failure("GpuError", error))?)
        } else {
            None
        };
        let ControlStores {
            clipboard,
            file_dialogs,
            fullscreen: recorded_fullscreen,
        } = stores;
        let (document, render, hits) = pipeline::connect();
        let pending_canvas = document.pending_canvas();
        let shutdown = DocumentShutdown::default();
        let document_thread = Arc::new(Mutex::new(None));
        let (commands, requests) = mpsc::channel();
        let (tasks, document_tasks) = mpsc::channel();
        tasks
            .send(DocumentTask::Media(desktop_media(window.theme())))
            .map_err(|error| failure("ThreadError", error))?;
        let signal = commands.clone();
        let (wake, demand) = FrameWake::pair(move || {
            let _ = signal.send(Command::Demand);
        });

        let test_control = request.test_control.is_some();
        let clipboard = if test_control {
            ClipboardMode::Memory(clipboard)
        } else {
            ClipboardMode::System
        };
        let canvas = commands.clone();
        let document =
            document.with_canvas(move |request| canvas.send(Command::ReadCanvas(request)).is_ok());
        let file_dialogs = if test_control {
            FileDialogMode::Queued(file_dialogs)
        } else {
            FileDialogMode::System
        };
        // Test-control runs record fullscreen instead of taking over the
        // desktop.
        let fullscreen = if test_control {
            FullscreenMode::Recorded(recorded_fullscreen)
        } else {
            let proxy = proxy.clone();
            FullscreenMode::Window(Box::new(move |id, enter| {
                proxy.send_event(Event::Fullscreen { id, enter }).is_ok()
            }))
        };
        let replies = commands.clone();
        let wake_tasks = tasks.clone();
        let document_shutdown = shutdown.clone();
        let document = spawn("squark-document", proxy, move |_| {
            run_packed(
                pack,
                (document_tasks, wake_tasks),
                document,
                HostRuntime {
                    test_control,
                    clipboard,
                    file_dialogs,
                    fullscreen,
                },
                DocumentControl {
                    frame_demand: demand,
                    shutdown: document_shutdown,
                },
                Instant::now,
                || {
                    let _ = replies.send(Command::Committed);
                },
            )
        })?;
        *document_thread.lock().unwrap() = Some(document);
        let preference = request.gpu_adapter;
        let channel = TickChannel::new(tasks.clone(), wake);
        let render_services = RenderServices {
            document: Arc::clone(&document_thread),
            shutdown: shutdown.clone(),
            tasks: tasks.clone(),
            pending_canvas: Arc::clone(&pending_canvas),
            pipeline: render,
            gpu: None,
            surface,
        };
        let render = spawn("strange-quark", proxy, move |proxy| {
            run_render(
                window,
                preference,
                requests,
                channel,
                render_services,
                proxy,
                system,
            )
        });
        match render {
            Ok(render) => Ok(Self {
                commands,
                hits,
                tasks,
                shutdown,
                pending_canvas,
                document: document_thread,
                render,
            }),
            Err(error) => {
                request_document_stop(&shutdown, &tasks, &pending_canvas);
                let _ = join_document_thread(&document_thread);
                Err(error)
            }
        }
    }

    fn stop(self) -> Result<(), HostError> {
        request_document_stop(&self.shutdown, &self.tasks, &self.pending_canvas);
        let document = join_document_thread(&self.document);
        let _ = self.commands.send(Command::Stop);
        let render = self.render.join();
        if render.is_err() {
            return Err(failure("ThreadError", "a runtime worker panicked"));
        }
        document
    }
}

struct RenderServices {
    document: DocumentThread,
    shutdown: DocumentShutdown,
    tasks: mpsc::Sender<DocumentTask>,
    pending_canvas: Arc<PendingCanvasRequests>,
    pipeline: RenderPipeline,
    gpu: Option<StaticWindowRenderer>,
    /// A surface made on the main thread, for window systems that need it
    /// made there. The render thread makes one otherwise.
    surface: Option<WindowSurface>,
}

impl Drop for RenderServices {
    fn drop(&mut self) {
        request_document_stop(&self.shutdown, &self.tasks, &self.pending_canvas);
        if join_document_thread(&self.document).is_err() {
            eprintln!("squark-runtime: document thread failed while stopping render services");
        }
    }
}

fn spawn(
    name: &'static str,
    proxy: &EventLoopProxy<Event>,
    work: impl FnOnce(&EventLoopProxy<Event>) -> Result<(), HostError> + Send + 'static,
) -> Result<JoinHandle<()>, HostError> {
    let proxy = proxy.clone();
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let error =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&proxy))) {
                    Ok(Ok(())) => return,
                    Ok(Err(error)) => error,
                    Err(_) => failure("ThreadError", format_args!("{name} panicked")),
                };
            let _ = proxy.send_event(Event::Failed(error));
        })
        .map_err(|error| failure("ThreadError", error))
}

fn refresh_interval(window: &Window) -> Duration {
    window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .filter(|&millihertz| millihertz != 0)
        .map_or(DEFAULT_REFRESH, |millihertz| {
            Duration::from_secs_f64(1000.0 / f64::from(millihertz))
        })
}

fn run_render(
    window: Arc<Window>,
    preference: GpuAdapter,
    requests: mpsc::Receiver<Command>,
    mut channel: TickChannel,
    mut services: RenderServices,
    proxy: &EventLoopProxy<Event>,
    system: WindowSystem,
) -> Result<(), HostError> {
    let mut clock = FrameClock::new(system.self_paced);
    let mut timeline = Timeline::new();
    clock.limit_refresh(timeline.cadence_limit()?);
    clock.refresh(refresh_interval(&window));

    // The document can bootstrap as soon as it knows the viewport. Canvas
    // reads can queue on this thread until the device is ready to service them.
    let Some(first) = start_first_tick(&requests, &mut channel, &mut clock) else {
        return Ok(());
    };
    let surface = match services.surface.take() {
        Some(surface) => surface,
        None => WindowSurface::new(window.clone()).map_err(|error| failure("GpuError", error))?,
    };
    services.gpu = Some(
        pollster::block_on(StaticWindowRenderer::new(
            surface,
            first.viewport.device,
            preference,
        ))
        .map_err(|error| failure("GpuError", error))?,
    );
    let RenderServices { pipeline, gpu, .. } = &mut services;
    let gpu = gpu.as_mut().unwrap();
    timeline.set_adapter(gpu.renderer_mut().adapter_name());
    let mut pending = Some(first);
    let mut last_tick = pending;
    let mut viewport = Some(first.viewport);
    let mut rendered = None;
    let mut presented = false;
    let mut retry_start = None;
    let mut document_redraw = false;
    let mut wake_after_latch = false;
    let mut idle = None;
    let mut presentations: VecDeque<PresentedFrame> = VecDeque::new();
    let mut probes: Vec<PresentationProbe> = Vec::new();
    let mut capture: Option<Capture> = None;
    let mut last_ime_area = None;
    loop {
        if capture.as_mut().is_some_and(|capture| {
            capture.complete(current_viewport_frame(pipeline, viewport), gpu)
        }) {
            capture = None;
        }
        let wake = [
            retry_start,
            pending
                .filter(|_| presented)
                .map(|tick| tick.commit_deadline),
            capture.as_ref().map(|capture| capture.deadline),
        ]
        .into_iter()
        .flatten()
        .min();
        let request = match wake {
            Some(at) => requests.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => requests
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        let mut request_frame = false;
        let committed = matches!(&request, Ok(Command::Committed));
        match request {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Command::Viewport(next)) => {
                retry_start = None;
                document_redraw = false;
                wake_after_latch = false;
                viewport = next;
                clock.reset();
                pending = next.map(|viewport| clock.tick(Instant::now(), viewport));
                last_tick = pending;
                channel.discard_pending();
                if pending.is_some_and(|tick| !channel.offer(tick)) {
                    break;
                }
            }
            Ok(Command::Occluded) => {
                retry_start = None;
                wake_after_latch = false;
                clock.reset();
                pending = None;
                channel.discard_pending();
            }
            Ok(Command::Redraw { at, refresh }) => {
                clock.refresh(refresh);
                if let Some(viewport) = viewport
                    && pending.is_none()
                {
                    let tick = clock.tick(at, viewport);
                    pending = Some(tick);
                    last_tick = pending;
                    // A redraw can be needed just to present a late commit.
                    // Ticking the document for that redraw can perpetuate a
                    // missed deadline even when nothing changed in the DOM.
                    let tick_document = std::mem::take(&mut document_redraw)
                        || channel.wake.requested()
                        || current_viewport_frame(pipeline, Some(viewport)).is_none();
                    if tick_document && !channel.offer(tick) {
                        break;
                    }
                }
            }
            Ok(Command::Demand) => request_frame = channel.wake.requested(),
            Ok(Command::Input {
                task,
                mouse,
                snapshot,
                reply,
            }) => {
                if matches!(&task.input, Input::Wheel { .. }) {
                    timeline.wheel(task.time);
                }
                let result = pipeline.dispatch_input(channel.sender(), task, mouse, snapshot);
                request_frame = result.is_ok();
                if let Some(reply) = reply {
                    control::send(reply, result.map(|()| json!({ "ok": true })));
                }
            }
            Ok(Command::WaitIdle { frames, reply }) => {
                if frames == 0
                    && let Some(frame) = pipeline.current()
                {
                    control::send(reply, Ok(json!({ "frameId": frame.id.0 })));
                } else {
                    idle = Some(IdleFrames::new(frames.max(1), reply));
                    request_frame = !channel.busy();
                }
            }
            Ok(Command::NextPresented(probe)) => {
                let now = Instant::now();
                if !probe.complete(presentations.iter(), now) {
                    probes.push(probe);
                    request_frame = !channel.busy();
                }
            }
            Ok(Command::Screenshot(next)) => {
                request_frame = current_viewport_frame(pipeline, viewport).is_none();
                capture = Some(next);
            }
            Ok(Command::ReadCanvas(request)) => pipeline.read_canvas(gpu.renderer_mut(), request),
            Ok(Command::Committed) => {
                if !channel.committed() {
                    break;
                }
                if pending.is_none()
                    && let Some(tick) = last_tick
                    && !wake_window(proxy, &clock, tick)
                {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        request_frame |= pipeline.input.poll();
        if request_frame {
            wake_after_latch = pending.is_some();
            if !wake_after_latch && let Some(tick) = last_tick {
                document_redraw = true;
                if !wake_window(proxy, &clock, tick) {
                    break;
                }
            }
        }
        let Some(tick) = pending else { continue };
        let ready = if presented {
            Instant::now() >= tick.commit_deadline
        } else {
            committed || retry_start.is_some_and(|at| Instant::now() >= at)
        };
        if !ready {
            continue;
        }
        let start = Instant::now();
        let mut trace = timeline.latch(tick);
        let latched = {
            let _startup = (!presented)
                .then(|| squark_gpu::StartupSpan::new("first-resource-drain-and-latch"));
            #[cfg(debug_assertions)]
            puffin::profile_scope!("latch and resolve properties");
            pipeline.adopt(gpu.renderer_mut(), tick, presented)?
        };
        let Some(latched) = latched else {
            timeline.finish_frame(trace, false);
            if presented {
                pending = None;
            }
            continue;
        };
        let frame = latched.frame;
        let publication = frame.publication;
        let frame_id = frame.id;
        let ime_area = if !system.ime_cursor_area {
            None
        } else {
            frame
                .frame
                .ime_area
                .and_then(|rect| CursorArea::new(rect, frame.viewport.scale))
        };
        timeline.frame(&mut trace, frame);
        let animating = latched.animating;
        pending = None;
        retry_start = None;
        if rendered != Some(frame.id) || latched.changed {
            let _startup =
                (!presented).then(|| squark_gpu::StartupSpan::new("first-render-encode"));
            #[cfg(debug_assertions)]
            puffin::profile_scope!("prepare and encode scene");
            gpu.render(frame)
                .map_err(|error| failure("GpuError", error))?;
            rendered = Some(frame.id);
        }
        let encode = start.elapsed();
        timeline.encoded(&mut trace);
        let submitted = {
            let _startup =
                (!presented).then(|| squark_gpu::StartupSpan::new("first-surface-present"));
            #[cfg(debug_assertions)]
            puffin::profile_scope!("blit and present");
            let compositor_paced = clock.compositor_paced();
            gpu.present(tick.viewport.device, || {
                if compositor_paced {
                    window.pre_present_notify();
                }
            })
            .map_err(|error| failure("GpuError", error))?
        };
        timeline.finish_frame(trace, submitted);
        if !submitted {
            clock.reset();
            let retry = Instant::now() + DEFAULT_REFRESH;
            if !presented {
                pending = Some(tick);
                retry_start = Some(retry);
                continue;
            }
            if proxy.send_event(Event::RequestRedraw(retry)).is_err() {
                break;
            }
            continue;
        }
        let presented_at = Instant::now();
        clock.presented(presented_at, encode);
        if let Some(publication) = publication
            && presentations
                .back()
                .is_none_or(|previous| previous.frame != frame_id)
        {
            while presentations.front().is_some_and(|frame| {
                presented_at.saturating_duration_since(frame.presented_at)
                    > control::REQUEST_TIMEOUT
            }) {
                presentations.pop_front();
            }
            presentations.push_back(PresentedFrame {
                frame: frame_id,
                committed_at: publication.committed,
                presented_at,
            });
        }
        let presentation =
            pipeline.mark_presented(tick.sequence, presented_at, channel.sender())?;
        if last_ime_area != ime_area {
            last_ime_area = ime_area;
            if proxy.send_event(Event::ImeArea(ime_area)).is_err() {
                break;
            }
        }
        probes.retain(|probe| !probe.complete(presentations.back(), presented_at));
        let mut request_frame = std::mem::take(&mut wake_after_latch)
            || presentation.scrolled
            || presentation.boundary_updated
            || animating
            || (!channel.busy() && !probes.is_empty());
        if let Some(wait) = &mut idle {
            let quiet = !request_frame && !pipeline.input.pending() && !channel.wake.requested();
            // Reusing a scene while an idle-probe tick is in flight does not
            // interrupt a run of quiet commits.
            if wait.presented(presentation.published_hits && !channel.busy(), quiet) {
                let wait = idle.take().unwrap();
                let outcome = if wait.remaining == 0 {
                    Ok(json!({ "frameId": rendered.unwrap().0 }))
                } else {
                    Err(failure("ControlError", "timed out waiting for idle frames"))
                };
                control::send(wait.reply, outcome);
            } else {
                request_frame |= !channel.busy();
            }
        }
        if !presented {
            presented = true;
            if proxy.send_event(Event::FirstPresent).is_err() {
                break;
            }
        } else if presentation.published_hits && proxy.send_event(Event::Presented).is_err() {
            break;
        }
        if request_frame {
            document_redraw = true;
            if !wake_window(proxy, &clock, tick) {
                break;
            }
        }
    }
    timeline.write()
}

fn start_first_tick(
    requests: &mpsc::Receiver<Command>,
    channel: &mut TickChannel,
    clock: &mut FrameClock,
) -> Option<FrameTick> {
    while let Ok(request) = requests.recv() {
        match request {
            Command::Viewport(Some(viewport)) => {
                let _startup = squark_gpu::StartupSpan::new("initial-frame-tick");
                let tick = clock.tick(Instant::now(), viewport);
                return channel.offer(tick).then_some(tick);
            }
            Command::Redraw { refresh, .. } => clock.refresh(refresh),
            Command::Stop => return None,
            // Input and test control open after the first presentation.
            _ => {}
        }
    }
    None
}

fn current_viewport_frame(
    pipeline: &RenderPipeline,
    viewport: Option<Viewport>,
) -> Option<&squark_paint::CommittedFrame> {
    pipeline
        .current()
        .filter(|frame| Some(frame.viewport) == viewport)
}

#[derive(Debug)]
struct Capture {
    path: Option<PathBuf>,
    reply: mpsc::Sender<Reply>,
    deadline: Instant,
}

impl Capture {
    fn complete(
        &mut self,
        frame: Option<&squark_paint::CommittedFrame>,
        gpu: &mut StaticWindowRenderer,
    ) -> bool {
        let reply = if let Some(frame) = frame {
            match gpu.renderer_mut().render_rgba(frame) {
                Ok(rgba) => Reply::Screenshot {
                    size: frame.viewport.device,
                    rgba,
                    path: self.path.take(),
                },
                Err(error) => Reply::Json(Err(failure("GpuError", error))),
            }
        } else if Instant::now() >= self.deadline {
            Reply::Json(Err(failure(
                "ControlError",
                "timed out waiting for a frame at the current viewport",
            )))
        } else {
            return false;
        };
        let _ = self.reply.send(reply);
        true
    }
}

#[derive(Clone, Copy)]
struct PresentedFrame {
    frame: FrameId,
    committed_at: Instant,
    presented_at: Instant,
}

#[derive(Debug)]
struct PresentationProbe {
    after: Instant,
    reply: mpsc::Sender<Reply>,
}

impl PresentationProbe {
    fn complete<'a>(
        &self,
        frames: impl IntoIterator<Item = &'a PresentedFrame>,
        now: Instant,
    ) -> bool {
        let result = if let Some(frame) = frames.into_iter().find(|f| f.committed_at > self.after) {
            Ok(control::presented(frame.frame, frame.presented_at))
        } else if now.saturating_duration_since(self.after) >= control::REQUEST_TIMEOUT {
            Err(failure(
                "ControlError",
                "timed out waiting for a new presentation",
            ))
        } else {
            return false;
        };
        let _ = self.reply.send(Reply::Json(result));
        true
    }
}

struct IdleFrames {
    frames: u32,
    remaining: u32,
    deadline: Instant,
    reply: mpsc::Sender<Reply>,
}

impl IdleFrames {
    fn new(frames: u32, reply: mpsc::Sender<Reply>) -> Self {
        Self {
            frames,
            remaining: frames,
            deadline: Instant::now() + control::REQUEST_TIMEOUT,
            reply,
        }
    }

    fn presented(&mut self, new_frame: bool, quiet: bool) -> bool {
        if !quiet {
            self.remaining = self.frames;
        } else if new_frame {
            self.remaining -= 1;
        }
        self.remaining == 0 || Instant::now() >= self.deadline
    }
}

fn wake_window(proxy: &EventLoopProxy<Event>, clock: &FrameClock, tick: FrameTick) -> bool {
    proxy
        .send_event(Event::RequestRedraw(
            clock.next_redraw(tick, Instant::now()),
        ))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_manifest_supplies_app_identity_and_individually_overridable_defaults() {
        let mut request = WindowRequest {
            input: PathBuf::new(),
            width: None,
            height: None,
            title: None,
            gpu_adapter: GpuAdapter::Auto,
            exit_after_first_frame: false,
            test_control: None,
        };
        let legacy = WindowSettings::load(None, &request).unwrap();
        assert_eq!(legacy.title, "squark-runtime");
        assert_eq!(legacy.size, LogicalSize::new(800, 600));

        let manifest = json!({
            "app_name": "workbench",
            "window": { "title": "My Workbench", "width": 1200, "height": 700 },
        });
        let defaults = WindowSettings::load(Some(manifest.clone()), &request).unwrap();
        assert_eq!(defaults.app_id, "workbench");
        assert_eq!(defaults.title, "My Workbench");
        assert_eq!(defaults.size, LogicalSize::new(1200, 700));

        request.title = Some("Testing".into());
        request.width = Some(500);
        let cli = WindowSettings::load(Some(manifest), &request).unwrap();
        assert_eq!(cli.app_id, "workbench");
        assert_eq!(cli.title, "Testing");
        assert_eq!(cli.size, LogicalSize::new(500, 700));
    }

    #[test]
    fn presentation_probe_ignores_older_commits_even_if_they_present_later() {
        let after = crate::process_start() + Duration::from_millis(20);
        let (reply, response) = mpsc::channel();
        let probe = PresentationProbe { after, reply };
        let old = PresentedFrame {
            frame: FrameId(13),
            committed_at: after - Duration::from_millis(1),
            presented_at: after + Duration::from_millis(2),
        };
        assert!(!probe.complete([&old], old.presented_at));
        assert!(response.try_recv().is_err());
        let next = PresentedFrame {
            frame: FrameId(14),
            committed_at: after + Duration::from_millis(1),
            presented_at: after + Duration::from_millis(5),
        };
        let later = PresentedFrame {
            frame: FrameId(15),
            committed_at: after + Duration::from_millis(6),
            presented_at: after + Duration::from_millis(9),
        };
        assert!(probe.complete([&old, &next, &later], later.presented_at));
        let Reply::Json(Ok(result)) = response.try_recv().unwrap() else {
            panic!("expected a successful presentation");
        };
        assert_eq!(result["frameId"], 14);
        assert_eq!(result["presentedAtMs"], 25.0);
    }

    #[test]
    fn viewport_changes_advance_the_epoch_for_pixel_size_and_dpr() {
        let mut view = ViewportState::default();
        let pixels = PhysicalSize::new(640, 480);
        assert!(matches!(
            view.update(pixels, 1.0),
            Some(Command::Viewport(Some(Viewport { epoch: 1, .. })))
        ));
        assert!(view.update(pixels, 1.0).is_none());
        let Some(Command::Viewport(Some(resized))) = view.update(pixels, 2.0) else {
            panic!("a new scale requires layout");
        };
        assert_eq!(
            resized.css,
            Size {
                width: 320.0,
                height: 240.0
            }
        );
        assert_eq!(resized.epoch, 2);
        assert!(matches!(
            view.update(PhysicalSize::new(0, 0), 2.0),
            Some(Command::Viewport(None))
        ));
        assert!(matches!(
            view.update(pixels, 2.0),
            Some(Command::Viewport(Some(Viewport { epoch: 3, .. })))
        ));
    }

    #[test]
    fn retina_scale_change_preserves_css_size_and_dedupes_following_resize() {
        let mut view = ViewportState::default();
        let Some(Command::Viewport(Some(one_x))) = view.update(PhysicalSize::new(800, 600), 1.0)
        else {
            panic!("initial 1x viewport");
        };
        assert_eq!(one_x.device, [800, 600]);
        assert_eq!(
            one_x.css,
            Size {
                width: 800.0,
                height: 600.0
            }
        );
        assert_eq!(one_x.scale, 1.0);

        // Moving onto a 2x Retina display fires ScaleFactorChanged with the
        // new physical size followed immediately by WindowEvent::Resized.
        let Some(Command::Viewport(Some(retina))) = view.update(PhysicalSize::new(1600, 1200), 2.0)
        else {
            panic!("2x scale change must update device pixels and scale");
        };
        assert_eq!(retina.device, [1600, 1200]);
        assert_eq!(
            retina.css,
            Size {
                width: 800.0,
                height: 600.0
            }
        );
        assert_eq!(retina.scale, 2.0);
        assert_eq!(retina.epoch, 2);
        assert!(view.update(PhysicalSize::new(1600, 1200), 2.0).is_none());
    }
}
