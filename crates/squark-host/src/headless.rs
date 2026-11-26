// SPDX-License-Identifier: MIT

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Instant,
};

use serde_json::json;
use squark_compositor::HitSnapshots;
use squark_gpu::StaticRenderer;
use squark_js::{DocumentShutdown, document::DocumentTask};
use squark_types::render::{FrameId, Viewport};

use crate::{
    HostError, ScreenshotRequest,
    canvas::ReadCanvas,
    clock::{DEFAULT_REFRESH, FrameClock, FrameWake},
    control::{self, Call, Reply, Request, Server},
    document::{
        HostRuntime, desktop_media, initialize_v8, join_scoped_document_thread, open_pack,
        request_document_stop, run_packed,
    },
    failure,
    ime::{CursorArea, Edit, ImeState},
    input::InputRouter,
    pipeline::{self, RenderPipeline},
    screenshot_viewport,
    services::{
        ClipboardMode, FileDialogMode, FileDialogQueue, FullscreenMode, MemoryClipboard,
        RecordedFullscreen,
    },
};

/// Run an offscreen document and serve the opted-in local test-control socket.
pub fn run_headless_control(request: ScreenshotRequest, socket: &Path) -> Result<(), HostError> {
    crate::process_start();
    let viewport = screenshot_viewport(&request)?;
    let pack = Arc::new(open_pack(&request.input)?);
    initialize_v8();
    let (document, render, hits) = pipeline::connect();
    let pending_canvas = document.pending_canvas();
    let shutdown = DocumentShutdown::default();
    let (tasks, document_tasks) = mpsc::channel();
    tasks
        .send(DocumentTask::Media(desktop_media(None)))
        .map_err(|error| failure("ThreadError", error))?;
    let clipboard = MemoryClipboard::default();
    let service_clipboard = clipboard.clone();
    let (submit, events) = mpsc::channel();
    let commit = submit.clone();
    let canvas = submit.clone();
    let document =
        document.with_canvas(move |request| canvas.send(Event::ReadCanvas(request)).is_ok());
    let file_dialogs = FileDialogQueue::default();
    let service_file_dialogs = file_dialogs.clone();
    let fullscreen = RecordedFullscreen::default();
    let service_fullscreen = fullscreen.clone();
    let signal = submit.clone();
    let (wake, demand) = FrameWake::pair(move || {
        let _ = signal.send(Event::Demand);
    });
    let gpu = pollster::block_on(StaticRenderer::headless_on(request.gpu_adapter))
        .map_err(|error| failure("GpuError", error))?;
    let floor = Arc::new(Mutex::new(Instant::now()));
    let document_clock = Arc::clone(&floor);
    let wake_tasks = tasks.clone();
    thread::scope(|scope| {
        let document_shutdown = shutdown.clone();
        let doc = thread::Builder::new()
            .name("squark-document".into())
            .spawn_scoped(scope, move || {
                if let Err(error) = run_packed(
                    pack,
                    (document_tasks, wake_tasks),
                    document,
                    HostRuntime {
                        test_control: true,
                        clipboard: ClipboardMode::Memory(service_clipboard),
                        file_dialogs: FileDialogMode::Queued(service_file_dialogs),
                        fullscreen: FullscreenMode::Recorded(service_fullscreen),
                    },
                    crate::document::DocumentControl {
                        frame_demand: demand,
                        shutdown: document_shutdown,
                    },
                    move || monotonic_now(&document_clock),
                    || {
                        let _ = commit.send(Event::Committed(Ok(())));
                    },
                ) {
                    let _ = commit.send(Event::Committed(Err(error)));
                }
            })
            .map_err(|error| failure("ThreadError", error))?;
        let mut session = Session {
            viewport,
            hits,
            render,
            gpu,
            tasks,
            events,
            pending_calls: VecDeque::new(),
            clock: FrameClock::new(false),
            wake,
            now: Instant::now(),
            floor,
            input: InputRouter::default(),
            ime: ImeState::default(),
            clipboard,
            file_dialogs,
            fullscreen,
        };
        let result = session.serve(socket, submit);
        request_document_stop(&shutdown, &session.tasks, &pending_canvas);
        let document = join_scoped_document_thread(doc);
        drop(session);
        result.and(document)
    })
}

enum Event {
    Call(Call),
    Demand,
    Committed(Result<(), HostError>),
    ReadCanvas(ReadCanvas),
}

struct Session {
    viewport: Viewport,
    hits: Arc<HitSnapshots>,
    render: RenderPipeline,
    gpu: StaticRenderer,
    tasks: mpsc::Sender<DocumentTask>,
    events: mpsc::Receiver<Event>,
    pending_calls: VecDeque<Call>,
    clock: FrameClock,
    wake: FrameWake,
    now: Instant,
    floor: Arc<Mutex<Instant>>,
    input: InputRouter,
    ime: ImeState,
    clipboard: MemoryClipboard,
    file_dialogs: FileDialogQueue,
    fullscreen: RecordedFullscreen,
}

impl Session {
    fn serve(&mut self, path: &Path, submit: mpsc::Sender<Event>) -> Result<(), HostError> {
        self.advance()?;
        let _server = Server::start(path, move |call| submit.send(Event::Call(call)).is_ok())
            .map_err(|error| failure("ControlError", error))?;
        loop {
            let event = if let Some(call) = self.pending_calls.pop_front() {
                Ok(Event::Call(call))
            } else if self.wake.requested() {
                self.events.recv_timeout(DEFAULT_REFRESH)
            } else {
                self.events
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            let Call {
                request,
                reply,
                received_at,
            } = match event {
                Ok(Event::Call(call)) => call,
                Ok(Event::Demand) => continue,
                Ok(Event::Committed(result)) => {
                    result?;
                    continue;
                }
                Ok(Event::ReadCanvas(request)) => {
                    self.render.read_canvas(&mut self.gpu, request);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.advance()?;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if !matches!(&request, Request::Ime(_))
                && let Some(edit) = self.ime.flush()
            {
                self.dispatch_ime(edit)?;
            }
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
                    control::send(reply, Ok(control::fullscreen(&self.fullscreen)));
                }
                Request::LeaveFullscreen {} => {
                    let result = control::leave_fullscreen(&self.fullscreen, &self.tasks);
                    control::send(reply, result);
                }
                Request::Ime(input) => {
                    let result = self.control_ime(input, monotonic_now(&self.floor));
                    control::send(reply, result);
                }
                Request::Quit {} => {
                    control::send(reply, Ok(json!({ "ok": true })));
                    break;
                }
                Request::Eval { .. } | Request::QueryRect { .. } => {
                    control::dom_task(request, &self.tasks, reply);
                }
                Request::Screenshot { path } => match self.screenshot(path) {
                    Ok(image) => {
                        let _ = reply.send(image);
                    }
                    Err(error) => control::send(reply, Err(error)),
                },
                Request::WaitIdle { frames } => {
                    let result = self
                        .wait_idle(frames)
                        .map(|frame| json!({ "frameId": frame.0 }));
                    control::send(reply, result);
                }
                Request::NextPresented(_) => {
                    let result = self.next_presented(received_at);
                    control::send(reply, result);
                }
                Request::Pointer(_) | Request::Key(_) | Request::Wheel(_) => {
                    let snapshot = self.hits.latest();
                    let task = self.input.synthetic(
                        request,
                        monotonic_now(&self.floor),
                        self.viewport,
                        snapshot.as_deref(),
                    );
                    let result = self
                        .render
                        .dispatch_input(&self.tasks, task, self.input.mouse_position(), snapshot)
                        .and_then(|()| self.advance().map(|_| json!({ "ok": true })));
                    control::send(reply, result);
                }
            }
        }
        Ok(())
    }

    fn next_presented(&mut self, after: Instant) -> Result<serde_json::Value, HostError> {
        // A frame driven by demand can finish while the socket call is in the queue.
        let frame = if let Some(frame) = self.render.current().filter(|frame| {
            frame
                .publication
                .is_some_and(|publication| publication.committed > after)
        }) {
            self.gpu
                .render_cached_rgba(frame)
                .map_err(|error| failure("GpuError", error))?;
            frame.id
        } else {
            self.advance_frame(true)?.0
        };
        Ok(control::presented(frame, Instant::now()))
    }

    fn advance(&mut self) -> Result<(FrameId, bool), HostError> {
        self.advance_frame(false)
    }

    fn advance_frame(&mut self, read_back: bool) -> Result<(FrameId, bool), HostError> {
        self.render.input.poll();
        let vsync = self.now.max(Instant::now());
        *self.floor.lock().unwrap() = vsync;
        let tick = self.clock.tick(vsync, self.viewport);
        self.now = vsync + DEFAULT_REFRESH;
        self.wake.begin_tick();
        self.tasks
            .send(DocumentTask::Frame(tick))
            .map_err(|_| failure("DocumentError", "document thread closed"))?;
        let deadline = Instant::now() + control::REQUEST_TIMEOUT;
        loop {
            let event = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| failure("DocumentError", error))?;
            match event {
                Event::Committed(result) => {
                    result?;
                    break;
                }
                Event::ReadCanvas(request) => self.render.read_canvas(&mut self.gpu, request),
                Event::Call(call) => self.pending_calls.push_back(call),
                Event::Demand => {}
            }
        }
        let latched = self
            .render
            .adopt(&mut self.gpu, tick, false)?
            .ok_or_else(|| failure("PaintError", "expected a headless frame"))?;
        let frame = latched.frame;
        let animating = latched.animating;
        let id = frame.id;
        let ime_area = frame
            .frame
            .ime_area
            .and_then(|rect| CursorArea::new(rect, frame.viewport.scale));
        let scene = self
            .gpu
            .render_cached(frame)
            .map_err(|error| failure("GpuError", error))?;
        if read_back {
            self.gpu
                .read_scene_rgba(&scene)
                .map_err(|error| failure("GpuError", error))?;
        }
        if ime_area != self.ime.area
            && let Some(edit) = self.ime.set_area(ime_area, monotonic_now(&self.floor))
        {
            self.dispatch_ime(edit)?;
        }
        let presented =
            self.render
                .mark_presented(tick.sequence, monotonic_now(&self.floor), &self.tasks)?;
        let changed = self.render.input.poll()
            || presented.scrolled
            || presented.boundary_updated
            || animating;
        if changed {
            self.wake.require_tick();
        }
        Ok((
            id,
            !changed && !self.render.input.pending() && !self.wake.requested(),
        ))
    }

    fn wait_idle(&mut self, frames: u32) -> Result<FrameId, HostError> {
        let deadline = Instant::now() + control::REQUEST_TIMEOUT;
        let mut remaining = frames;
        let mut last = self.render.current().unwrap().id;
        while remaining != 0 {
            if Instant::now() >= deadline {
                return Err(failure("ControlError", "timed out waiting for idle frames"));
            }
            let (frame, idle) = self.advance()?;
            last = frame;
            remaining = if idle { remaining - 1 } else { frames };
        }
        Ok(last)
    }

    fn screenshot(&mut self, path: Option<std::path::PathBuf>) -> Result<Reply, HostError> {
        let frame = self
            .render
            .current()
            .ok_or_else(|| failure("PaintError", "no presented headless frame"))?;
        let rgba = self
            .gpu
            .render_rgba(frame)
            .map_err(|error| failure("GpuError", error))?;
        Ok(Reply::Screenshot {
            size: frame.viewport.device,
            rgba,
            path,
        })
    }

    fn control_ime(
        &mut self,
        input: control::ImeInput,
        time: Instant,
    ) -> Result<serde_json::Value, HostError> {
        let edits = self.ime.control_event(input.into_event(), time);
        for edit in edits.into_iter().flatten() {
            self.dispatch_ime(edit)?;
        }
        Ok(json!({ "ok": true }))
    }

    fn dispatch_ime(&mut self, edit: Edit) -> Result<(), HostError> {
        let task = self.input.ime(edit);
        self.render.dispatch_input(&self.tasks, task, None, None)
    }
}

fn monotonic_now(floor: &Mutex<Instant>) -> Instant {
    Instant::now().max(*floor.lock().unwrap())
}
