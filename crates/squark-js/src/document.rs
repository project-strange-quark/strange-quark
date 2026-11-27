// SPDX-License-Identifier: MIT

//! One V8 realm on a document thread. Host messages and replies own their data.

use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

use squark_types::{
    NodeId, Rect, ScrollOffset,
    engine::{
        DocumentEngine, FaceId, IntersectionEntry, MediaState, Query, QueryValue, ResizeDelivery,
        SelectMode,
    },
    host::{HostError, HostValue, InputTask},
    pack::{DOCUMENT_HEADER_SIZE, DocumentHeader, ScriptKind},
    render::{FrameDemand, FrameId, FramePainter, FrameTick, Viewport},
};
use url::Url;
use v8::{Function, Global, Local, Object, PinScope, Value};

use crate::{
    DocumentShutdown,
    bindings::{self, Bridge, FrameState, get, set, with_bridge},
    bitmap,
    bootstrap::{self, PackedDocument},
    canvas, clone_value, crypto, css, data, fonts,
    input::{self, QueuedInput},
    loader::{self, RUNTIME_ENTRY, RUNTIME_JOURNAL, ScriptSources},
    native, navigation, network, observer, platform, scroll,
    task_loop::{TaskLoop, call, install_task_host},
    wasm, worker,
};

pub use crate::{
    css::{AnimationEvent, AnimationEventType},
    input::InputResult,
};

const MAX_TASKS: usize = 64;
const TASK_BUDGET: Duration = Duration::from_millis(4);

#[derive(Clone, Copy, Debug)]
pub struct InitialViewport {
    pub tick: FrameTick,
    pub media: MediaState,
}

impl InitialViewport {
    /// The host sends preferences before its first frame. Consume that prefix
    /// before constructing the realm, leaving later tasks on the queue.
    pub fn receive(tasks: &Receiver<DocumentTask>) -> Option<Self> {
        let mut media = MediaState::default();
        loop {
            match tasks.recv().ok()? {
                DocumentTask::Media(next) => media = next,
                DocumentTask::Frame(tick) => return Some(Self { tick, media }),
                _ => return None,
            }
        }
    }
}

impl From<FrameTick> for InitialViewport {
    fn from(tick: FrameTick) -> Self {
        Self {
            tick,
            media: MediaState::default(),
        }
    }
}

#[derive(Debug)]
pub struct DocumentOptions {
    pub url: String,
    pub initial_tick: FrameTick,
    pub time_origin: Instant,
    pub test_control: bool,
    pub frame_demand: FrameDemand,
}

#[derive(Debug)]
pub enum DocumentError {
    Closed,
    Setup(String),
    JavaScript(String),
    Engine(squark_types::engine::EngineError),
    Paint(squark_types::render::PaintError),
    TestControlDisabled,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("document is closed"),
            Self::Setup(error) => write!(f, "document setup: {error}"),
            Self::JavaScript(error) => write!(f, "document JavaScript: {error}"),
            Self::Engine(error) => error.fmt(f),
            Self::Paint(error) => error.fmt(f),
            Self::TestControlDisabled => f.write_str("document test control is disabled"),
        }
    }
}

impl std::error::Error for DocumentError {}

#[derive(Debug)]
pub enum DocumentTask {
    Frame(FrameTick),
    Media(MediaState),
    ScrollSync {
        frame: FrameId,
        tick: u64,
        offsets: Box<[ScrollOffset]>,
    },
    AnimationEvent(AnimationEvent),
    ResourceLayoutChanged,
    NetworkReady,
    /// A native module replied or emitted an event (coalesced).
    NativeReady,
    /// A worker posted a message, failed or closed (coalesced).
    WorkerReady,
    ImageResult {
        target: NodeId,
        generation: u32,
        size: Option<[u32; 2]>,
    },
    HostReply {
        request: u32,
        result: Result<HostValue, HostError>,
    },
    /// The host left (or entered) fullscreen without a document request.
    FullscreenChanged {
        fullscreen: bool,
    },
    FontResult {
        face: FaceId,
        result: Result<(), String>,
    },
    Input {
        task: InputTask,
        reply: Option<Sender<Result<InputResult, DocumentError>>>,
    },
    Eval {
        script: String,
        reply: Sender<Result<String, DocumentError>>,
    },
    QueryRect {
        selector: String,
        reply: Sender<Result<Option<Rect>, DocumentError>>,
    },
    Shutdown,
}

impl From<FrameTick> for DocumentTask {
    fn from(tick: FrameTick) -> Self {
        Self::Frame(tick)
    }
}

impl DocumentTask {
    fn reject_closed(self) {
        match self {
            Self::Input {
                reply: Some(reply), ..
            } => {
                let _ = reply.send(Err(DocumentError::Closed));
            }
            Self::Eval { reply, .. } => {
                let _ = reply.send(Err(DocumentError::Closed));
            }
            Self::QueryRect { reply, .. } => {
                let _ = reply.send(Err(DocumentError::Closed));
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct FrameResult {
    pub frame: FrameId,
    pub resize: ResizeDelivery,
    pub intersections: Vec<IntersectionEntry>,
}

struct StartupScript {
    kind: ScriptKind,
    node: NodeId,
    source: Vec<u16>,
    base: String,
}

struct Realm {
    context: Global<v8::Context>,
    tasks: TaskLoop,
    flush: Global<Function>,
    finish: Global<Function>,
    resize: Global<Function>,
    sync_scroll: Global<Function>,
    dispatch_scroll: Global<Function>,
    media: Global<Function>,
    animation: Global<Function>,
    deliver_resize: Global<Function>,
    queue_intersections: Global<Function>,
    pending_records: Global<Function>,
    network: Global<Function>,
    load_script: Global<Function>,
    font_resource: Global<Function>,
    image_resource: Global<Function>,
    host_reply: Global<Function>,
    fullscreen_change: Global<Function>,
    drain_indexed_db: Global<Function>,
    flush_local_storage: Global<Function>,
    network_queued: Arc<AtomicBool>,
    native: Option<Global<Function>>,
    native_queued: Arc<AtomicBool>,
    workers: Option<Global<Function>>,
    worker_queued: Arc<AtomicBool>,
    demand: FrameDemand,
    media_changed: bool,
    in_frame: bool,
    input: Global<Function>,
    viewport_changed: bool,
    document: Global<Object>,
    scripts: VecDeque<StartupScript>,
    pending: Option<Global<v8::Promise>>,
    started: bool,
    document_id: NodeId,
    origin: Instant,
    clock: Rc<dyn Fn() -> Instant>,
    test_control: bool,
}

pub struct DocumentRunner<'engine> {
    realm: Realm,
    shutdown: DocumentShutdown,
    // V8 handles must drop before the isolate.
    isolate: v8::OwnedIsolate,
    // The isolate holds a pointer to this stable allocation.
    _bridge: Box<RefCell<Bridge<'engine>>>,
}

impl fmt::Debug for DocumentRunner<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentRunner")
            .field("document_id", &self.realm.document_id)
            .field("started", &self.realm.started)
            .finish_non_exhaustive()
    }
}

impl<'engine> DocumentRunner<'engine> {
    /// Read the root ID when constructing the engine for this document.
    pub fn document_id(packed: &[u8]) -> Result<NodeId, DocumentError> {
        let (header, _) = packed
            .split_first_chunk::<DOCUMENT_HEADER_SIZE>()
            .ok_or_else(|| setup("truncated document.bin header"))?;
        Ok(DocumentHeader::decode(header)
            .map_err(setup)?
            .document_node_id)
    }

    /// Creates the isolate on the calling thread. The host initializes the V8
    /// platform once and supplies an engine that has replayed the packed batches.
    pub fn new(
        options: DocumentOptions,
        packed: &[u8],
        sources: ScriptSources,
        engine: Box<dyn DocumentEngine + 'engine>,
        painter: Box<dyn FramePainter + 'engine>,
        clock: impl Fn() -> Instant + 'static,
    ) -> Result<Self, DocumentError> {
        Self::new_with_media(
            options,
            packed,
            sources,
            engine,
            painter,
            clock,
            MediaState::default(),
        )
    }

    pub fn new_with_media(
        options: DocumentOptions,
        packed: &[u8],
        mut sources: ScriptSources,
        engine: Box<dyn DocumentEngine + 'engine>,
        painter: Box<dyn FramePainter + 'engine>,
        clock: impl Fn() -> Instant + 'static,
        initial_media: MediaState,
    ) -> Result<Self, DocumentError> {
        let shutdown = sources.shutdown.clone();
        shutdown.check()?;
        let packed = PackedDocument::read(packed).map_err(setup)?;
        let url = Url::parse(&options.url).map_err(setup)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(setup("packed document URL must be HTTP or HTTPS"));
        }
        let clock: Rc<dyn Fn() -> Instant> = Rc::new(clock);
        let bridge;
        let mut isolate = v8::Isolate::new(Default::default());
        let isolate_handle = isolate.thread_safe_handle();
        let realm = {
            v8::scope!(let scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let task_clock = Rc::clone(&clock);
            let origin = options.time_origin;
            let host =
                install_task_host(scope, move || millis(task_clock(), origin)).map_err(setup)?;
            network::install(scope, host, url.clone(), &sources, true)
                .ok_or_else(|| setup("failed to install networking bindings"))?;
            canvas::install(scope, host, sources.canvas.take())
                .ok_or_else(|| setup("failed to install canvas bindings"))?;
            bitmap::install(scope, host)
                .ok_or_else(|| setup("failed to install bitmap binding"))?;
            data::install(scope, host, &url, sources.blobs.clone())
                .ok_or_else(|| setup("failed to install blob bindings"))?;
            crypto::install(scope, host)
                .ok_or_else(|| setup("failed to install crypto bindings"))?;
            clone_value::install(scope, host)
                .ok_or_else(|| setup("failed to install clone bindings"))?;
            fonts::install(scope, host).ok_or_else(|| setup("failed to install font bindings"))?;
            platform::install(scope, host, sources.host_requests.take())
                .ok_or_else(|| setup("failed to install host services"))?;
            let dev = sources.dev;
            native::install(scope, host, sources.native.take())
                .ok_or_else(|| setup("failed to install native module bindings"))?;
            let console = bindings::console_enabled(dev, options.test_control);
            worker::install(scope, host, &sources, console)
                .ok_or_else(|| setup("failed to install worker bindings"))?;
            loader::install(scope, url, sources);
            bindings::install(scope, host, console)
                .ok_or_else(|| setup("failed to install native bindings"))?;
            navigation::install(scope, host)
                .ok_or_else(|| setup("failed to install URL bindings"))?;
            input::install(scope, host).ok_or_else(|| setup("failed to install inputResult"))?;
            wasm::install(scope, host)
                .ok_or_else(|| setup("failed to install WebAssembly task tracking"))?;
            observer::install(scope, host)
                .ok_or_else(|| setup("failed to install observer bindings"))?;
            css::install(scope, host)
                .ok_or_else(|| setup("failed to install stylesheet bindings"))?;
            let global = context.global(scope);
            set(scope, global, "__squark", host.into())
                .ok_or_else(|| setup("failed to install __squark"))?;
            set_viewport(scope, global, options.initial_tick.viewport)?;
            let api = runtime_module(scope, RUNTIME_ENTRY)?;
            let journal_module = runtime_module(scope, RUNTIME_JOURNAL)?;
            let constructor = function(scope, journal_module, "Journal")?;
            let sink = get(scope, host, "flush").ok_or_else(|| setup("missing journal sink"))?;
            let journal = constructor
                .new_instance(scope, &[sink])
                .ok_or_else(|| setup("could not create Journal"))?;
            let replay = function(scope, api, "bootstrapDocument")?;
            let (document, decoder) =
                bootstrap::replay_document(scope, &packed, replay, journal).map_err(setup)?;
            bridge = Box::new(RefCell::new(Bridge {
                shutdown: shutdown.clone(),
                engine,
                painter,
                decoder,
                dirty: false,
                staging: Default::default(),
                scroll_dirty: false,
                scroll_delivery: Vec::new(),
                input: FrameState {
                    tick: options.initial_tick,
                    media: initial_media,
                    scroll: Default::default(),
                },
            }));
            // SAFETY: The bridge is boxed before registration and drops after
            // the isolate, including if the rest of initialization fails.
            unsafe { bindings::register(scope, &bridge) };
            with_bridge(scope, |bridge| {
                bridge
                    .engine
                    .set_media_state(&bridge.input.tick.viewport, &bridge.input.media);
            });
            let dom_options = Object::new(scope);
            let url = v8::String::new(scope, &options.url).ok_or_else(|| setup("document URL"))?;
            set(scope, dom_options, "url", url.into()).ok_or_else(|| setup("document URL"))?;
            let install = function(scope, api, "installDOM")?;
            call(
                scope,
                install,
                &[global.into(), document.into(), dom_options.into()],
            )
            .map_err(setup)?;
            let install_native = function(scope, api, "installDocumentBindings")?;
            call(
                scope,
                install_native,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let packed_links = css::packed_stylesheets(scope, &packed)
                .ok_or_else(|| setup("packed stylesheet links"))?;
            let install_cssom = function(scope, api, "installCSSOM")?;
            call(
                scope,
                install_cssom,
                &[document.into(), host.into(), packed_links.into()],
            )
            .map_err(setup)?;
            let install_scroll = function(scope, api, "installScroll")?;
            let scroll =
                call(scope, install_scroll, &[global.into(), document.into()]).map_err(setup)?;
            let scroll = Local::<Object>::try_from(scroll).map_err(setup)?;
            let sync_scroll = function(scope, scroll, "sync")?;
            let dispatch_scroll = function(scope, scroll, "dispatch")?;
            let install_media = function(scope, api, "installMedia")?;
            let media = call(scope, install_media, &[global.into(), host.into()]).map_err(setup)?;
            let media = Local::<Function>::try_from(media).map_err(setup)?;
            let install_observers = function(scope, api, "installObservers")?;
            let observers =
                call(scope, install_observers, &[global.into(), host.into()]).map_err(setup)?;
            let observers = Local::<Object>::try_from(observers).map_err(setup)?;
            let deliver_resize = function(scope, observers, "deliverResize")?;
            let queue_intersections = function(scope, observers, "queueIntersection")?;
            let animation = function(scope, api, "dispatchAnimationEvent")?;
            let pending_records = function(scope, api, "hasPendingRecords")?;
            let install_input = function(scope, api, "installInput")?;
            let input =
                call(scope, install_input, &[document.into(), host.into()]).map_err(setup)?;
            let input = Local::<Function>::try_from(input).map_err(setup)?;
            let timing = function(scope, api, "installTiming")?;
            let timing = call(scope, timing, &[global.into(), host.into()]).map_err(setup)?;
            let timing = Local::<Object>::try_from(timing).map_err(setup)?;
            let tasks = TaskLoop::new(scope, timing).map_err(setup)?;
            let install_workers = function(scope, api, "installWorkers")?;
            let workers = call(
                scope,
                install_workers,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let workers = Local::<Function>::try_from(workers)
                .ok()
                .map(|workers| Global::new(scope, workers));
            let install_navigation = function(scope, api, "installNavigation")?;
            call(
                scope,
                install_navigation,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let install_network = function(scope, api, "installNetworking")?;
            let network = call(
                scope,
                install_network,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let network = Local::<Object>::try_from(network).map_err(setup)?;
            let flush_local_storage = function(scope, network, "flushLocalStorage")?;
            let load_script = function(scope, network, "loadScript")?;
            let fetch_module = function(scope, network, "fetchModule")?;
            loader::install_dev_fetch(scope, fetch_module);
            let network = function(scope, network, "dispatch")?;
            for name in [
                "installConsole",
                "installFiles",
                "installCrypto",
                "installFormData",
                "installWasm",
                "installCSSNamespace",
            ] {
                let install = function(scope, api, name)?;
                call(scope, install, &[global.into(), host.into()]).map_err(setup)?;
            }
            let install_indexed_db = function(scope, api, "installIndexedDB")?;
            let drain_indexed_db =
                call(scope, install_indexed_db, &[global.into(), host.into()]).map_err(setup)?;
            let drain_indexed_db = Local::<Function>::try_from(drain_indexed_db).map_err(setup)?;
            let install_platform = function(scope, api, "installPlatform")?;
            let host_reply = call(
                scope,
                install_platform,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let host_reply = Local::<Function>::try_from(host_reply).map_err(setup)?;
            let install_native = function(scope, api, "installNative")?;
            let native = call(scope, install_native, &[global.into(), host.into()])
                .map_err(setup)?;
            let native = Local::<Function>::try_from(native)
                .ok()
                .map(|native| Global::new(scope, native));
            let image_resource = function(scope, api, "dispatchImageResult")?;
            let fullscreen_change = function(scope, api, "dispatchFullscreenChange")?;
            clone_value::prepare(scope).ok_or_else(|| setup("failed to stash clone prototypes"))?;
            let install_fonts = function(scope, api, "installFonts")?;
            let font_resource = call(
                scope,
                install_fonts,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let font_resource = Local::<Function>::try_from(font_resource).map_err(setup)?;
            let install_canvas = function(scope, api, "installCanvas")?;
            call(
                scope,
                install_canvas,
                &[global.into(), document.into(), host.into()],
            )
            .map_err(setup)?;
            let flush = function(scope, api, "flushDocument")?;
            let finish = function(scope, api, "finishDocumentStartup")?;
            let resize = function(scope, api, "notifyWindowResize")?;
            let scripts = packed
                .scripts()
                .filter(|(entry, _, _)| entry.kind != ScriptKind::ExternalStylesheet)
                .map(|(entry, source, base)| StartupScript {
                    kind: entry.kind,
                    node: entry.node_id,
                    source: source.units().collect(),
                    base: String::from_utf16_lossy(&base.units().collect::<Vec<_>>()),
                })
                .collect();
            let mut realm = Realm {
                context: Global::new(scope, context),
                tasks,
                flush: Global::new(scope, flush),
                finish: Global::new(scope, finish),
                resize: Global::new(scope, resize),
                sync_scroll: Global::new(scope, sync_scroll),
                dispatch_scroll: Global::new(scope, dispatch_scroll),
                media: Global::new(scope, media),
                animation: Global::new(scope, animation),
                deliver_resize: Global::new(scope, deliver_resize),
                queue_intersections: Global::new(scope, queue_intersections),
                pending_records: Global::new(scope, pending_records),
                network: Global::new(scope, network),
                load_script: Global::new(scope, load_script),
                font_resource: Global::new(scope, font_resource),
                image_resource: Global::new(scope, image_resource),
                host_reply: Global::new(scope, host_reply),
                fullscreen_change: Global::new(scope, fullscreen_change),
                drain_indexed_db: Global::new(scope, drain_indexed_db),
                flush_local_storage: Global::new(scope, flush_local_storage),
                network_queued: Arc::default(),
                native,
                native_queued: Arc::default(),
                workers,
                worker_queued: Arc::default(),
                demand: options.frame_demand,
                media_changed: false,
                in_frame: false,
                input: Global::new(scope, input),
                viewport_changed: false,
                document: Global::new(scope, document),
                scripts,
                pending: None,
                started: false,
                document_id: packed.header.document_node_id,
                origin,
                clock,
                test_control: options.test_control,
            };
            shutdown.attach(isolate_handle)?;
            let startup = realm.advance_startup(scope);
            if !shutdown.is_closed() {
                startup?;
                realm.request_frame_if_needed(scope);
            }
            realm
        };
        Ok(Self {
            realm,
            shutdown,
            isolate,
            _bridge: bridge,
        })
    }

    /// Wake the document queue when squark-net delivers a packet. The owner
    /// must send Shutdown even if its other document senders are dropped.
    pub fn set_network_wakeup(&mut self, sender: Sender<DocumentTask>) {
        self.with_realm(|scope, realm| {
            let queued = Arc::clone(&realm.network_queued);
            let network_sender = sender.clone();
            network::set_waker(scope, move || {
                if !queued.swap(true, Ordering::AcqRel) {
                    let _ = network_sender.send(DocumentTask::NetworkReady);
                }
            });
            let queued = Arc::clone(&realm.native_queued);
            let native_sender = sender.clone();
            native::set_waker(scope, move || {
                if !queued.swap(true, Ordering::AcqRel) {
                    let _ = native_sender.send(DocumentTask::NativeReady);
                }
            });
            let queued = Arc::clone(&realm.worker_queued);
            worker::set_waker(scope, move || {
                if !queued.swap(true, Ordering::AcqRel) {
                    let _ = sender.send(DocumentTask::WorkerReady);
                }
            });
        });
    }

    /// Receive frame ticks and test-control work on the document thread. A
    /// producer can close the channel, or send Shutdown for an explicit stop.
    pub fn run(
        mut self,
        receiver: Receiver<DocumentTask>,
        mut committed: impl FnMut(FrameResult),
    ) -> Result<(), DocumentError> {
        let mut messages = VecDeque::new();
        let result = self.run_queue(&receiver, &mut messages, &mut committed);
        self.shutdown.close();
        self.with_realm(|scope, _| {
            native::close(scope);
            worker::close_all(scope);
        });
        messages
            .into_iter()
            .chain(receiver.try_iter())
            .for_each(DocumentTask::reject_closed);
        if matches!(result, Ok(()) | Err(DocumentError::Closed)) {
            self.finish_indexed_db();
        }
        match result {
            Err(DocumentError::Closed) => Ok(()),
            result => result,
        }
    }

    fn finish_indexed_db(&mut self) {
        let Some(deadline) = self.shutdown.deadline() else {
            return;
        };
        if Instant::now() >= deadline {
            return;
        }
        // Run only trusted storage drains. Explicit microtask policy keeps
        // the page's tasks, event handlers and promises suspended.
        self.isolate.cancel_terminate_execution();
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let isolate = self.isolate.thread_safe_handle();
        let Ok(watchdog) = std::thread::Builder::new()
            .name("squark-idb-shutdown".into())
            .spawn(move || {
                if matches!(
                    finished.recv_timeout(deadline.saturating_duration_since(Instant::now())),
                    Err(RecvTimeoutError::Timeout)
                ) {
                    isolate.terminate_execution();
                }
            })
        else {
            return;
        };
        self.with_realm(|scope, realm| {
            let drain = Local::new(scope, &realm.drain_indexed_db);
            let _ = call(scope, drain, &[]);
            let flush = Local::new(scope, &realm.flush_local_storage);
            let _ = call(scope, flush, &[]);
        });
        drop(done);
        let _ = watchdog.join();
    }

    fn run_queue(
        &mut self,
        receiver: &Receiver<DocumentTask>,
        messages: &mut VecDeque<DocumentTask>,
        committed: &mut impl FnMut(FrameResult),
    ) -> Result<(), DocumentError> {
        let mut last_sequence = None;
        let mut pending: Option<QueuedInput> = None;
        loop {
            self.shutdown.check()?;
            let wait = self.next_wakeup();
            let first = match wait {
                Some(wait) => receiver.recv_timeout(wait),
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match first {
                Ok(message) => {
                    messages.push_back(message);
                    messages.extend(receiver.try_iter().take(MAX_TASKS - 1));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            if messages
                .iter()
                .any(|task| matches!(task, DocumentTask::Shutdown))
            {
                return Ok(());
            }
            self.shutdown.check()?;
            let tick = messages
                .iter()
                .filter_map(|message| match message {
                    DocumentTask::Frame(tick)
                        if last_sequence.is_none_or(|last| last < tick.sequence) =>
                    {
                        Some(*tick)
                    }
                    _ => None,
                })
                .max_by_key(|tick| tick.sequence);
            let media = messages.iter().rev().find_map(|message| match message {
                DocumentTask::Media(media) => Some(*media),
                _ => None,
            });
            if tick.is_some() || media.is_some() {
                self.update_viewport(tick, media)?;
            }
            while !messages.is_empty() {
                self.shutdown.check()?;
                let message = messages.pop_front().unwrap();
                if let DocumentTask::Input { task, reply } = message {
                    let mut next = QueuedInput::new(task, reply);
                    if pending
                        .as_mut()
                        .is_some_and(|previous| previous.coalesce(&mut next))
                    {
                        continue;
                    }
                    if let Some(previous) = pending.take() {
                        self.deliver_input(previous);
                    }
                    if next.is_deferred_move() {
                        pending = Some(next);
                    } else {
                        self.deliver_input(next);
                    }
                    continue;
                }
                if let Some(previous) = pending.take() {
                    self.deliver_input(previous);
                }
                match message {
                    DocumentTask::Frame(_) | DocumentTask::Media(_) => {}
                    DocumentTask::ScrollSync {
                        frame,
                        tick,
                        offsets,
                    } => self.scroll_sync(frame, tick, &offsets)?,
                    DocumentTask::AnimationEvent(event) => self.animation_event(event)?,
                    DocumentTask::ResourceLayoutChanged => self.resource_layout_changed(),
                    DocumentTask::NetworkReady => {
                        self.realm.network_queued.store(false, Ordering::Release)
                    }
                    DocumentTask::NativeReady => {
                        self.realm.native_queued.store(false, Ordering::Release)
                    }
                    DocumentTask::WorkerReady => {
                        self.realm.worker_queued.store(false, Ordering::Release)
                    }
                    DocumentTask::ImageResult {
                        target,
                        generation,
                        size,
                    } => self.image_result(target, generation, size)?,
                    DocumentTask::HostReply { request, result } => {
                        self.host_reply(request, result)?
                    }
                    DocumentTask::FullscreenChanged { fullscreen } => {
                        self.fullscreen_changed(fullscreen)?
                    }
                    DocumentTask::FontResult { face, result } => self.font_result(face, result)?,
                    DocumentTask::Eval { script, reply } => {
                        let _ = reply.send(self.eval(&script));
                    }
                    DocumentTask::QueryRect { selector, reply } => {
                        let _ = reply.send(self.query_rect(&selector));
                    }
                    DocumentTask::Shutdown | DocumentTask::Input { .. } => unreachable!(),
                }
            }
            if let Some(tick) = tick {
                if let Some(previous) = pending.take() {
                    self.deliver_input(previous);
                }
                committed(self.commit_frame(tick)?);
                if last_sequence.is_none() && !self.shutdown.is_closed() {
                    self.with_realm(|scope, _| loader::persist_startup_cache(scope));
                }
                last_sequence = Some(tick.sequence);
            } else {
                if pending.is_some() {
                    self.realm.demand.request_frame();
                }
                self.run_ready()?;
            }
        }
    }

    /// A bounded timer and posted-task turn, also used between frames.
    pub fn run_ready(&mut self) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            realm.ready(scope, (realm.clock)() + TASK_BUDGET)?;
            realm.dispatch_media(scope)?;
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    fn update_viewport(
        &mut self,
        tick: Option<FrameTick>,
        media: Option<MediaState>,
    ) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            if tick.is_some() {
                realm.demand.begin_tick();
                realm.in_frame = true;
            }
            let (viewport, resized, preference_changed) = with_bridge(scope, |bridge| {
                let previous = bridge.input.tick.viewport;
                let old_media = bridge.input.media;
                if let Some(tick) = tick {
                    bridge.input.tick = tick;
                }
                if let Some(media) = media {
                    bridge.input.media = media;
                }
                let viewport = bridge.input.tick.viewport;
                let preference_changed = old_media != bridge.input.media;
                let resized = previous.css != viewport.css || previous.scale != viewport.scale;
                if previous != viewport || preference_changed {
                    bridge
                        .engine
                        .set_media_state(&viewport, &bridge.input.media);
                }
                bridge.dirty |= preference_changed;
                (viewport, resized, preference_changed)
            });
            realm.viewport_changed |= resized;
            realm.media_changed |= resized || preference_changed;
            let global = scope.get_current_context().global(scope);
            set_viewport(scope, global, viewport)
        })
    }

    pub fn scroll_sync(
        &mut self,
        frame: FrameId,
        tick: u64,
        offsets: &[ScrollOffset],
    ) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            realm.flush(scope)?;
            with_bridge(scope, |bridge| {
                bridge.update_scroll()?;
                bridge
                    .scroll_delivery
                    .extend(bridge.input.scroll.update(frame, tick, offsets));
                Ok(())
            })?;
            if realm.deliver_scroll_offsets(scope)? && !realm.in_frame {
                realm.demand.request_frame();
            }
            Ok(())
        })
    }

    pub fn image_result(
        &mut self,
        target: NodeId,
        generation: u32,
        size: Option<[u32; 2]>,
    ) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            if realm.dispatch_image_result(scope, target, generation, size)? {
                with_bridge(scope, |bridge| bridge.dirty = true);
            }
            realm.tasks.checkpoint_after_startup(scope);
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    pub fn host_reply(
        &mut self,
        request: u32,
        result: Result<HostValue, HostError>,
    ) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            let dispatch = Local::new(scope, &realm.host_reply);
            let args = platform::reply(scope, request, result)
                .ok_or_else(|| setup("cannot encode host service reply"))?;
            call(scope, dispatch, &args).map_err(js_error)?;
            realm.tasks.checkpoint_after_startup(scope);
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    /// Apply a fullscreen change the host made without a document request.
    pub fn fullscreen_changed(&mut self, fullscreen: bool) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            let dispatch = Local::new(scope, &realm.fullscreen_change);
            let document = Local::new(scope, &realm.document);
            let fullscreen = v8::Boolean::new(scope, fullscreen);
            call(scope, dispatch, &[document.into(), fullscreen.into()]).map_err(js_error)?;
            realm.tasks.checkpoint_after_startup(scope);
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    /// Deliver a font completion from the resource queue on the document thread.
    pub fn font_result(
        &mut self,
        face: FaceId,
        result: Result<(), String>,
    ) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            realm.dispatch_font_resource(scope, face, Some(result))?;
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    pub fn resource_layout_changed(&mut self) {
        if self.shutdown.is_closed() {
            return;
        }
        self.with_realm(|scope, realm| {
            with_bridge(scope, |bridge| bridge.dirty = true);
            if !realm.in_frame {
                realm.demand.request_frame();
            }
        });
    }

    pub fn animation_event(&mut self, event: AnimationEvent) -> Result<(), DocumentError> {
        self.enter(|scope, realm| {
            realm.dispatch_animation_event(scope, event)?;
            realm.tasks.checkpoint_after_startup(scope);
            realm.advance_startup(scope)?;
            realm.request_frame_if_needed(scope);
            Ok(())
        })
    }

    /// Dispatch one host task and resolve its default-action decision.
    pub fn input(&mut self, task: &InputTask) -> Result<InputResult, DocumentError> {
        self.dispatch_input(task, true)
    }

    fn dispatch_input(
        &mut self,
        task: &InputTask,
        cancelable: bool,
    ) -> Result<InputResult, DocumentError> {
        self.enter(|scope, realm| {
            let dispatcher = Local::new(scope, &realm.input);
            let decision = input::dispatch(scope, dispatcher, task, cancelable, realm.origin);
            realm.tasks.checkpoint_after_startup(scope);
            realm.advance_startup(scope)?;
            realm.request_frame_if_needed(scope);
            decision
        })
    }

    fn deliver_input(&mut self, mut queued: QueuedInput) {
        queued.finish();
        let outcome = self.dispatch_input(&queued.task, queued.reply.is_some());
        if let Some(reply) = queued.reply {
            let _ = reply.send(outcome);
        } else if let Err(error) = outcome
            && !matches!(error, DocumentError::Closed)
        {
            self.with_realm(|scope, _| report_message(scope, &error.to_string()));
        }
    }

    pub fn frame(&mut self, tick: FrameTick) -> Result<FrameResult, DocumentError> {
        self.update_viewport(Some(tick), None)?;
        self.commit_frame(tick)
    }

    fn commit_frame(&mut self, tick: FrameTick) -> Result<FrameResult, DocumentError> {
        self.enter(|scope, realm| {
            realm.ready(
                scope,
                tick.commit_deadline.min((realm.clock)() + TASK_BUDGET),
            )?;
            if std::mem::take(&mut realm.viewport_changed) {
                let global = scope.get_current_context().global(scope);
                let resize = Local::new(scope, &realm.resize);
                call(scope, resize, &[global.into()]).map_err(js_error)?;
                realm.tasks.checkpoint_after_startup(scope);
            }
            realm.deliver_scroll_offsets(scope)?;
            let scroll = Local::new(scope, &realm.dispatch_scroll);
            call(scope, scroll, &[]).map_err(js_error)?;
            realm.tasks.checkpoint_after_startup(scope);
            realm.dispatch_media(scope)?;
            let time = millis(tick.vsync, realm.origin);
            if let Err(error) = realm.tasks.run_animation(scope, time) {
                report_message(scope, &error.to_string());
            }
            realm.advance_startup(scope)?;
            realm.layout(scope)?;
            let resize = realm.deliver_resize_observers(scope)?;
            let intersections = with_bridge(scope, |bridge| {
                bridge.engine.intersection_entries(tick.vsync)
            });
            let frame = with_bridge(scope, |bridge| {
                let (fragments, mirror) = bridge.engine.snapshot();
                bridge
                    .painter
                    .commit(&tick, fragments, mirror)
                    .map_err(DocumentError::Paint)
            })?;
            if !intersections.is_empty() {
                let dispatch = Local::new(scope, &realm.queue_intersections);
                let entries = observer::intersection_entries(scope, intersections.clone())
                    .ok_or_else(|| setup("cannot encode intersection entries"))?;
                call(scope, dispatch, &[entries]).map_err(js_error)?;
            }
            let animation_events =
                with_bridge(scope, |bridge| bridge.engine.take_animation_events());
            if !animation_events.is_empty() {
                for event in animation_events {
                    realm.dispatch_animation_event(scope, event)?;
                }
                realm.tasks.checkpoint_after_startup(scope);
            }
            let image_events = with_bridge(scope, |bridge| bridge.engine.take_image_events());
            if !image_events.is_empty() {
                for event in image_events {
                    realm.dispatch_image_result(
                        scope,
                        event.target,
                        event.id,
                        event.natural_size,
                    )?;
                }
                realm.tasks.checkpoint_after_startup(scope);
                realm.advance_startup(scope)?;
            }
            let (font_starts, font_completions) = with_bridge(scope, |bridge| {
                (
                    bridge.engine.take_font_load_starts(),
                    bridge.engine.take_font_completions(),
                )
            });
            for face in font_starts {
                realm.dispatch_font_resource(scope, face, None)?;
            }
            if !font_completions.is_empty() {
                for completion in font_completions {
                    realm.dispatch_font_resource(
                        scope,
                        completion.face,
                        Some(completion.result),
                    )?;
                }
                realm.advance_startup(scope)?;
            }
            realm.in_frame = false;
            realm.request_frame_if_needed(scope);
            Ok(FrameResult {
                frame,
                resize,
                intersections,
            })
        })
    }

    pub fn eval(&mut self, script: &str) -> Result<String, DocumentError> {
        self.enter(|scope, realm| {
            realm.check_test_control()?;
            let outcome = {
                v8::tc_scope!(let scope, scope);
                (|| {
                    let source = v8::String::new(scope, script)?;
                    let value = loader::classic(scope, source, "<dom.eval>")?;
                    if value.is_undefined() {
                        return Some("null".into());
                    }
                    Some(v8::json::stringify(scope, value)?.to_rust_string_lossy(scope))
                })()
                .ok_or_else(|| exception(scope.exception(), scope))
            };
            realm.tasks.checkpoint_after_startup(scope);
            realm.advance_startup(scope)?;
            realm.request_frame_if_needed(scope);
            outcome
        })
    }

    pub fn query_rect(&mut self, selector: &str) -> Result<Option<Rect>, DocumentError> {
        self.enter(|scope, realm| {
            realm.check_test_control()?;
            realm.flush(scope)?;
            let result = with_bridge(scope, |bridge| {
                let node = bridge.query(Query::Selector {
                    scope: realm.document_id,
                    selector,
                    mode: SelectMode::First,
                })?;
                match node {
                    QueryValue::Node(None) => Ok(None),
                    QueryValue::Node(Some(node)) => {
                        match bridge.query(Query::BoundingRect(node))? {
                            QueryValue::Rect(rect) => Ok(Some(rect)),
                            _ => Err(setup(
                                "engine returned a different kind of result for BoundingRect",
                            )),
                        }
                    }
                    _ => Err(setup(
                        "engine returned a different kind of result for Selector",
                    )),
                }
            });
            realm.request_frame_if_needed(scope);
            result
        })
    }

    fn next_wakeup(&mut self) -> Option<Duration> {
        self.with_realm(|scope, realm| {
            if realm.tasks.has_posted_tasks(scope) {
                return Some(Duration::ZERO);
            }
            let now = millis((realm.clock)(), realm.origin);
            let timer = realm.tasks.next_timer_deadline(scope).and_then(|deadline| {
                Duration::try_from_secs_f64((deadline - now).max(0.0) / 1000.0).ok()
            });
            timer
                .into_iter()
                .chain(network::wakeup(scope))
                .chain(native::wakeup(scope))
                .chain(worker::wakeup(scope))
                .chain(wasm::wakeup(scope))
                .min()
        })
    }

    fn enter<T>(
        &mut self,
        run: impl FnOnce(&mut PinScope<'_, '_>, &mut Realm) -> Result<T, DocumentError>,
    ) -> Result<T, DocumentError> {
        self.shutdown.check()?;
        let result = self.with_realm(run);
        self.shutdown.check()?;
        result
    }

    fn with_realm<T>(&mut self, run: impl FnOnce(&mut PinScope<'_, '_>, &mut Realm) -> T) -> T {
        v8::scope!(let scope, &mut self.isolate);
        let context = Local::new(scope, &self.realm.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        run(scope, &mut self.realm)
    }
}

impl Realm {
    fn dispatch_image_result(
        &self,
        scope: &mut PinScope<'_, '_>,
        target: NodeId,
        generation: u32,
        size: Option<[u32; 2]>,
    ) -> Result<bool, DocumentError> {
        let dispatch = Local::new(scope, &self.image_resource);
        let document = Local::new(scope, &self.document);
        let id = v8::Integer::new_from_unsigned(scope, target.raw());
        let generation = v8::Integer::new_from_unsigned(scope, generation);
        let dimensions = if let Some([width, height]) = size {
            let width = v8::Integer::new_from_unsigned(scope, width);
            let height = v8::Integer::new_from_unsigned(scope, height);
            v8::Array::new_with_elements(scope, &[width.into(), height.into()]).into()
        } else {
            v8::null(scope).into()
        };
        let accepted = call(
            scope,
            dispatch,
            &[document.into(), id.into(), generation.into(), dimensions],
        )
        .map_err(js_error)?;
        Ok(accepted.is_true())
    }

    fn advance_startup(&mut self, scope: &mut PinScope<'_, '_>) -> Result<(), DocumentError> {
        while !self.started {
            if let Some(promise) = &self.pending {
                let promise = Local::new(scope, promise);
                match promise.state() {
                    v8::PromiseState::Pending => break,
                    v8::PromiseState::Rejected => {
                        let error = promise.result(scope);
                        report(scope, error);
                    }
                    v8::PromiseState::Fulfilled => {}
                }
                self.pending = None;
            }
            if let Some(script) = self.scripts.pop_front() {
                self.pending = self.start_script(scope, script)?;
            } else {
                let finish = Local::new(scope, &self.finish);
                let document = Local::new(scope, &self.document);
                call(scope, finish, &[document.into()]).map_err(js_error)?;
                self.started = true;
            }
            self.tasks.checkpoint_after_startup(scope);
        }
        Ok(())
    }

    fn start_script(
        &self,
        scope: &mut PinScope<'_, '_>,
        script: StartupScript,
    ) -> Result<Option<Global<v8::Promise>>, DocumentError> {
        if script.kind == ScriptKind::ExternalModule
            && loader::can_fetch_dev_script(scope, &script.source)
        {
            return Ok(execute_script(scope, script));
        }
        if matches!(
            script.kind,
            ScriptKind::ExternalClassic | ScriptKind::ExternalModule
        ) && !loader::has_packed_script(scope, &script.source)
        {
            let load = Local::new(scope, &self.load_script);
            let node = v8::Integer::new_from_unsigned(scope, script.node.raw());
            let src =
                v8::String::new_from_two_byte(scope, &script.source, v8::NewStringType::Normal)
                    .ok_or_else(|| setup("could not decode script URL"))?;
            let module = v8::Boolean::new(scope, script.kind == ScriptKind::ExternalModule);
            let result =
                call(scope, load, &[node.into(), src.into(), module.into()]).map_err(js_error)?;
            let promise = Local::<v8::Promise>::try_from(result).map_err(setup)?;
            promise.mark_as_handled();
            Ok(Some(Global::new(scope, promise)))
        } else {
            Ok(execute_script(scope, script))
        }
    }

    fn ready(
        &mut self,
        scope: &mut PinScope<'_, '_>,
        deadline: Instant,
    ) -> Result<(), DocumentError> {
        let platform = v8::V8::get_current_platform();
        for _ in 0..MAX_TASKS {
            if !v8::Platform::pump_message_loop(&platform, scope, false) {
                break;
            }
            self.tasks.checkpoint_after_startup(scope);
            if (self.clock)() >= deadline {
                break;
            }
        }
        for _ in 0..MAX_TASKS {
            let Some(event) = network::next(scope) else {
                break;
            };
            let dispatch = Local::new(scope, &self.network);
            if let Err(error) = call(scope, dispatch, &[event]) {
                report_message(scope, &error.to_string());
            }
            self.tasks.checkpoint_after_startup(scope);
            if (self.clock)() >= deadline {
                break;
            }
        }
        if let Some(dispatch) = &self.native {
            let dispatch = Local::new(scope, dispatch);
            for _ in 0..MAX_TASKS {
                let Some(message) = native::next(scope) else {
                    break;
                };
                if let Err(error) = call(scope, dispatch, &[message]) {
                    report_message(scope, &error.to_string());
                }
                self.tasks.checkpoint_after_startup(scope);
                if (self.clock)() >= deadline {
                    break;
                }
            }
        }
        if let Some(dispatch) = &self.workers {
            let dispatch = Local::new(scope, dispatch);
            for _ in 0..MAX_TASKS {
                let Some(packet) = worker::next(scope) else {
                    break;
                };
                if let Err(error) = call(scope, dispatch, &[packet]) {
                    report_message(scope, &error.to_string());
                }
                self.tasks.checkpoint_after_startup(scope);
                if (self.clock)() >= deadline {
                    break;
                }
            }
        }
        let deadline = millis(deadline, self.origin);
        if let Err(error) = self.tasks.run_ready(scope, deadline, MAX_TASKS) {
            report_message(scope, &error.to_string());
        }
        if loader::advance_dev(scope) {
            self.tasks.checkpoint_after_startup(scope);
        }
        self.advance_startup(scope)
    }

    fn flush(&self, scope: &mut PinScope<'_, '_>) -> Result<(), DocumentError> {
        let flush = Local::new(scope, &self.flush);
        let document = Local::new(scope, &self.document);
        call(scope, flush, &[document.into()]).map_err(js_error)?;
        Ok(())
    }

    fn deliver_scroll_offsets(&self, scope: &mut PinScope<'_, '_>) -> Result<bool, DocumentError> {
        let accepted = with_bridge(scope, |bridge| std::mem::take(&mut bridge.scroll_delivery));
        if accepted.is_empty() {
            return Ok(false);
        }
        let sync = Local::new(scope, &self.sync_scroll);
        let samples = scroll::samples(scope, accepted)
            .ok_or_else(|| setup("cannot encode scroll samples"))?;
        call(scope, sync, &[samples]).map_err(js_error)?;
        Ok(true)
    }

    fn dispatch_media(&mut self, scope: &mut PinScope<'_, '_>) -> Result<(), DocumentError> {
        if std::mem::take(&mut self.media_changed) {
            let media = Local::new(scope, &self.media);
            call(scope, media, &[]).map_err(js_error)?;
            self.tasks.checkpoint_after_startup(scope);
        }
        Ok(())
    }

    fn request_frame_if_needed(&self, scope: &mut PinScope<'_, '_>) {
        if self.in_frame {
            return;
        }
        if observer::pending(scope)
            || with_bridge(scope, |bridge| {
                bridge.dirty
                    || !bridge.scroll_delivery.is_empty()
                    || bridge.engine.has_running_animations()
            })
            || self.tasks.has_animation_callbacks(scope)
        {
            self.demand.request_frame();
            return;
        }
        let check = Local::new(scope, &self.pending_records);
        let document = Local::new(scope, &self.document);
        if call(scope, check, &[document.into()]).is_ok_and(|value| value.is_true()) {
            self.demand.request_frame();
        }
    }

    fn layout(&self, scope: &mut PinScope<'_, '_>) -> Result<(), DocumentError> {
        self.flush(scope)?;
        observer::apply_registrations(scope);
        with_bridge(scope, |bridge| {
            bridge.layout()?;
            bridge.dirty = false;
            Ok(())
        })
    }

    fn dispatch_animation_event(
        &self,
        scope: &mut PinScope<'_, '_>,
        event: AnimationEvent,
    ) -> Result<(), DocumentError> {
        let dispatch = Local::new(scope, &self.animation);
        let document = Local::new(scope, &self.document);
        let event =
            css::animation(scope, event).ok_or_else(|| setup("cannot encode animation event"))?;
        call(scope, dispatch, &[document.into(), event]).map_err(js_error)?;
        Ok(())
    }

    fn dispatch_font_resource(
        &self,
        scope: &mut PinScope<'_, '_>,
        face: FaceId,
        result: Option<Result<(), String>>,
    ) -> Result<(), DocumentError> {
        if matches!(result.as_ref(), Some(Ok(()))) {
            with_bridge(scope, |bridge| bridge.dirty = true);
        }
        let dispatch = Local::new(scope, &self.font_resource);
        let args = fonts::resource(scope, face, result.as_ref())
            .ok_or_else(|| setup("cannot encode font resource task"))?;
        call(scope, dispatch, &args).map_err(js_error)?;
        self.tasks.checkpoint_after_startup(scope);
        Ok(())
    }

    fn deliver_resize_observers(
        &self,
        scope: &mut PinScope<'_, '_>,
    ) -> Result<ResizeDelivery, DocumentError> {
        let mut delivered = ResizeDelivery::default();
        loop {
            let depth = delivered.next_depth;
            let ResizeDelivery {
                entries,
                next_depth,
                skipped,
            } = with_bridge(scope, |bridge| bridge.engine.resize_entries(depth));
            if entries.is_empty() {
                delivered.skipped = skipped;
                break;
            }
            let dispatch = Local::new(scope, &self.deliver_resize);
            delivered.entries.extend(entries.iter().cloned());
            let entries = observer::resize_entries(scope, entries)
                .ok_or_else(|| setup("cannot encode resize entries"))?;
            call(scope, dispatch, &[entries]).map_err(js_error)?;
            self.tasks.checkpoint_after_startup(scope);
            self.layout(scope)?;
            if next_depth <= depth {
                delivered.skipped = true;
                break;
            }
            delivered.next_depth = next_depth;
        }
        if delivered.skipped {
            report_message(
                scope,
                "ResizeObserver loop completed with undelivered notifications.",
            );
            self.tasks.checkpoint_after_startup(scope);
        }
        Ok(delivered)
    }

    fn check_test_control(&self) -> Result<(), DocumentError> {
        if self.test_control {
            Ok(())
        } else {
            Err(DocumentError::TestControlDisabled)
        }
    }
}

fn execute_script(
    scope: &mut PinScope<'_, '_>,
    script: StartupScript,
) -> Option<Global<v8::Promise>> {
    let (pending, error) = {
        v8::tc_scope!(let scope, scope);
        let evaluation = loader::execute(
            scope,
            script.kind,
            script.node.raw(),
            &script.source,
            &script.base,
        );
        match evaluation {
            Some(value) => (
                value.map(|promise| {
                    promise.mark_as_handled();
                    Global::new(scope, promise)
                }),
                None,
            ),
            None => (None, scope.exception()),
        }
    };
    if let Some(error) = error {
        report(scope, error);
    }
    pending
}

fn report_message(scope: &mut PinScope<'_, '_>, message: &str) {
    if scope.is_execution_terminating() {
        return;
    }
    if let Some(message) = v8::String::new(scope, message) {
        let error = v8::Exception::error(scope, message);
        let global = scope.get_current_context().global(scope);
        if let Ok(report) = function(scope, global, "reportError") {
            let _ = call(scope, report, &[error]);
        }
    }
}

fn report<'s>(scope: &mut PinScope<'s, '_>, error: Local<'s, Value>) {
    if scope.is_execution_terminating() {
        return;
    }
    let global = scope.get_current_context().global(scope);
    if let Some(host) =
        get(scope, global, "__squark").and_then(|host| Local::<Object>::try_from(host).ok())
        && let Ok(report) = function(scope, host, "reportScriptError")
    {
        let message = v8::Exception::create_message(scope, error);
        let source = message
            .get_script_resource_name(scope)
            .filter(|value| value.is_string())
            .unwrap_or_else(|| v8::String::empty(scope).into());
        let line = message.get_line_number(scope).unwrap_or_default();
        let column = if line == 0 {
            0
        } else {
            message.get_start_column() + 1
        };
        let line = v8::Integer::new_from_unsigned(scope, line as u32);
        let column = v8::Integer::new_from_unsigned(scope, column as u32);
        let text = message.get(scope);
        let _ = call(
            scope,
            report,
            &[error, source, line.into(), column.into(), text.into()],
        );
    }
}

pub(crate) fn runtime_module<'s>(
    scope: &mut PinScope<'s, '_>,
    name: &'static str,
) -> Result<Local<'s, Object>, DocumentError> {
    v8::tc_scope!(let scope, scope);
    let module = loader::load(scope, Url::parse(name).unwrap())
        .ok_or_else(|| exception(scope.exception(), scope))?;
    let promise =
        loader::evaluate(scope, module).ok_or_else(|| exception(scope.exception(), scope))?;
    match promise.state() {
        v8::PromiseState::Fulfilled => {
            Local::try_from(module.get_module_namespace()).map_err(setup)
        }
        v8::PromiseState::Rejected => {
            let error = promise.result(scope);
            Err(setup(error.to_rust_string_lossy(scope)))
        }
        v8::PromiseState::Pending => Err(setup(
            "squark-dom runtime module must evaluate synchronously",
        )),
    }
}

fn set_viewport(
    scope: &mut PinScope<'_, '_>,
    global: Local<'_, Object>,
    viewport: Viewport,
) -> Result<(), DocumentError> {
    for (name, value) in [
        ("innerWidth", viewport.css.width.into()),
        ("innerHeight", viewport.css.height.into()),
        ("devicePixelRatio", viewport.scale),
    ] {
        let value = v8::Number::new(scope, value);
        set(scope, global, name, value.into())
            .ok_or_else(|| setup("could not install viewport"))?;
    }
    Ok(())
}

fn function<'s>(
    scope: &mut PinScope<'s, '_>,
    object: Local<'_, Object>,
    name: &str,
) -> Result<Local<'s, Function>, DocumentError> {
    get(scope, object, name)
        .and_then(|value| Local::try_from(value).ok())
        .ok_or_else(|| setup(format!("missing function {name}")))
}

pub(crate) fn millis(instant: Instant, origin: Instant) -> f64 {
    instant.saturating_duration_since(origin).as_secs_f64() * 1000.0
}

fn setup(error: impl fmt::Display) -> DocumentError {
    DocumentError::Setup(error.to_string())
}

fn js_error(error: impl fmt::Display) -> DocumentError {
    DocumentError::JavaScript(error.to_string())
}

fn exception(value: Option<Local<'_, Value>>, scope: &mut PinScope<'_, '_>) -> DocumentError {
    js_error(value.map_or_else(
        || "execution stopped".into(),
        |exception| exception.to_rust_string_lossy(scope),
    ))
}
