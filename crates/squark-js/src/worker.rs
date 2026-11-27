// SPDX-License-Identifier: MIT

//! Dedicated workers. `new Worker` starts a thread with its own V8 isolate
//! and a slim global scope: timers, networking, blobs, crypto, structured
//! clone and the module loader, but no DOM, journal or engine. A document and
//! its workers exchange owned bytes, and neither thread enters the other's
//! isolate. Terminating a worker stops its JavaScript at once; its isolate is
//! freed when the thread ends, which the document never waits for while it
//! runs.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use url::Url;
use v8::{
    Function, FunctionCallbackArguments, Global, Local, Object, PinScope, ReturnValue, Value,
};

use crate::{
    bindings::{self, get, set, string, unsigned},
    clone_value::{self, Message},
    crypto, data,
    document::{millis, runtime_module},
    loader::{self, RUNTIME_WORKER, ScriptSources, SharedSources},
    navigation, network,
    task_loop::{TaskLoop, call, install_task_host},
    wasm,
};

const MAX_TASKS: usize = 64;
/// A worker has no frame deadline. It checks for new commands this often.
const TURN_BUDGET: Duration = Duration::from_millis(16);
/// requestAnimationFrame in a worker runs on a 60 Hz clock.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
/// V8 limits JavaScript to about 1 MiB of it; the rest is for native frames.
/// Only the pages a worker touches count toward its memory.
const STACK_SIZE: usize = 4 * 1024 * 1024;
/// How long a closing document waits for its workers' threads to end.
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Without a host wakeup, a document polls for worker packets this often.
const POLL: Duration = Duration::from_millis(10);

type Wake = dyn Fn() + Send + Sync;

/// What a worker thread tells its document.
#[derive(Debug)]
enum Report {
    Message(Message),
    /// An uncaught exception that the worker's own handlers did not cancel.
    Error {
        message: String,
        filename: String,
        lineno: u32,
        colno: u32,
    },
    /// The script did not load, or a module did not parse or link.
    Failed,
    Closed,
}

#[derive(Clone)]
struct Outbox {
    id: u32,
    sender: Sender<(u32, Report)>,
    wake: Arc<OnceLock<Box<Wake>>>,
}

impl Outbox {
    fn send(&self, report: Report) {
        if self.sender.send((self.id, report)).is_ok()
            && let Some(wake) = self.wake.get()
        {
            wake();
        }
    }
}

/// What a document tells a worker thread.
enum Command {
    Message(Message),
    /// The worker's network transport has a packet.
    Wake,
    Terminate,
}

/// Stops one worker from the document thread. The worker thread registers
/// its isolate before it runs any JavaScript.
#[derive(Default)]
struct Control(Mutex<(bool, Option<v8::IsolateHandle>)>);

impl Control {
    fn terminate(&self) {
        let mut state = self.0.lock().unwrap();
        state.0 = true;
        if let Some(isolate) = &state.1 {
            isolate.terminate_execution();
        }
    }

    fn attach(&self, isolate: v8::IsolateHandle) -> bool {
        let mut state = self.0.lock().unwrap();
        if !state.0 {
            state.1 = Some(isolate);
        }
        !state.0
    }

    fn is_terminated(&self) -> bool {
        self.0.lock().unwrap().0
    }
}

struct Handle {
    commands: Sender<Command>,
    control: Arc<Control>,
    thread: JoinHandle<()>,
}

impl Handle {
    fn stop(self) -> JoinHandle<()> {
        self.control.terminate();
        let _ = self.commands.send(Command::Terminate);
        self.thread
    }
}

/// The document's workers, kept in the document isolate.
pub(crate) struct Workers {
    sources: SharedSources,
    console: bool,
    handles: HashMap<u32, Handle>,
    sender: Sender<(u32, Report)>,
    receiver: Receiver<(u32, Report)>,
    local: VecDeque<(u32, Report)>,
    wake: Arc<OnceLock<Box<Wake>>>,
    stopping: Vec<JoinHandle<()>>,
}

impl Workers {
    /// Joins the threads that have ended, without waiting for the others.
    fn reap(&mut self) {
        let mut index = 0;
        while index < self.stopping.len() {
            if self.stopping[index].is_finished() {
                let _ = self.stopping.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }

    fn close(&mut self) {
        let threads: Vec<_> = self
            .handles
            .drain()
            .map(|(_, handle)| handle.stop())
            .chain(self.stopping.drain(..))
            .collect();
        let deadline = Instant::now() + JOIN_TIMEOUT;
        for thread in threads {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.close();
    }
}

/// Installs the document's `workerStart`, `workerPost` and `workerTerminate`.
pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    sources: &ScriptSources,
    console: bool,
) -> Option<()> {
    let (sender, receiver) = mpsc::channel();
    scope.set_slot(Workers {
        sources: sources.share(),
        console,
        handles: HashMap::new(),
        sender,
        receiver,
        local: VecDeque::new(),
        wake: Arc::default(),
        stopping: Vec::new(),
    });
    for (name, callback) in [
        ("workerStart", Function::new(scope, start)),
        ("workerPost", Function::new(scope, post)),
        ("workerTerminate", Function::new(scope, terminate)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn workers<'a>(scope: &'a mut PinScope<'_, '_>) -> &'a mut Workers {
    scope.get_slot_mut().expect("worker bindings installed")
}

/// Wake the document queue when a worker sends a packet.
pub(crate) fn set_waker(scope: &mut PinScope<'_, '_>, wake: impl Fn() + Send + Sync + 'static) {
    let _ = workers(scope).wake.set(Box::new(wake));
}

pub(crate) fn wakeup(scope: &mut PinScope<'_, '_>) -> Option<Duration> {
    let workers = workers(scope);
    if workers.local.is_empty()
        && let Ok(packet) = workers.receiver.try_recv()
    {
        workers.local.push_back(packet);
    }
    if !workers.local.is_empty() {
        Some(Duration::ZERO)
    } else {
        (workers.wake.get().is_none() && !workers.handles.is_empty()).then_some(POLL)
    }
}

/// The next worker packet for squark-dom, as `[id, kind, payload?]`.
/// Messages are deserialized here, in the document's isolate.
pub(crate) fn next<'s>(scope: &mut PinScope<'s, '_>) -> Option<Local<'s, Value>> {
    let state = workers(scope);
    let (id, report) = state
        .local
        .pop_front()
        .or_else(|| state.receiver.try_recv().ok())?;
    let (kind, payload) = match report {
        Report::Message(message) => match clone_value::deserialize_message(scope, message) {
            Some(value) => ("message", Some(value)),
            None => ("messageerror", None),
        },
        Report::Error {
            message,
            filename,
            lineno,
            colno,
        } => {
            let message = v8::String::new(scope, &message)?;
            let filename = v8::String::new(scope, &filename)?;
            let lineno = v8::Integer::new_from_unsigned(scope, lineno);
            let colno = v8::Integer::new_from_unsigned(scope, colno);
            let details = v8::Array::new_with_elements(
                scope,
                &[message.into(), filename.into(), lineno.into(), colno.into()],
            );
            ("error", Some(details.into()))
        }
        Report::Failed => ("failed", None),
        Report::Closed => {
            let state = workers(scope);
            if let Some(handle) = state.handles.remove(&id) {
                state.stopping.push(handle.thread);
            }
            state.reap();
            ("closed", None)
        }
    };
    let id = v8::Integer::new_from_unsigned(scope, id);
    let kind = v8::String::new(scope, kind)?;
    let mut packet = vec![id.into(), kind.into()];
    packet.extend(payload);
    Some(v8::Array::new_with_elements(scope, &packet).into())
}

/// Stops every worker when the document closes. Waits a bounded time for
/// their threads, so their isolates are gone before the document's.
pub(crate) fn close_all(scope: &mut PinScope<'_, '_>) {
    workers(scope).close();
}

fn start(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    let (Some(id), Some(url)) = (
        unsigned(args.get(0)),
        string(scope, args.get(1)).and_then(|url| Url::parse(&url).ok()),
    ) else {
        return;
    };
    let module = args.get(2).is_true();
    let name = string(scope, args.get(3)).unwrap_or_default();
    let state = workers(scope);
    let (commands, inbox) = mpsc::channel();
    let control = Arc::new(Control::default());
    let worker = Spawn {
        url,
        module,
        name,
        sources: state.sources.clone(),
        console: state.console,
        outbox: Outbox {
            id,
            sender: state.sender.clone(),
            wake: Arc::clone(&state.wake),
        },
        control: Arc::clone(&control),
        inbox,
        commands: commands.clone(),
    };
    let thread = std::thread::Builder::new()
        .name("squark-worker".into())
        .stack_size(STACK_SIZE)
        .spawn(move || worker.run());
    match thread {
        Ok(thread) => {
            state.handles.insert(
                id,
                Handle {
                    commands,
                    control,
                    thread,
                },
            );
        }
        Err(_) => state.local.push_back((id, Report::Failed)),
    }
    state.reap();
}

fn post(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    let Some(id) = unsigned(args.get(0)) else {
        return;
    };
    let Some(message) = clone_value::serialize_message(scope, args.get(1), args.get(2)) else {
        return;
    };
    if let Some(handle) = workers(scope).handles.get(&id) {
        let _ = handle.commands.send(Command::Message(message));
    }
}

fn terminate(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    let Some(id) = unsigned(args.get(0)) else {
        return;
    };
    let state = workers(scope);
    if let Some(handle) = state.handles.remove(&id) {
        let thread = handle.stop();
        state.stopping.push(thread);
    }
    state.local.retain(|(packet, _)| *packet != id);
    state.reap();
}

/// Everything a worker thread starts from. It owns no V8 handles.
struct Spawn {
    url: Url,
    module: bool,
    name: String,
    sources: SharedSources,
    console: bool,
    outbox: Outbox,
    control: Arc<Control>,
    inbox: Receiver<Command>,
    commands: Sender<Command>,
}

/// What the worker realm's bindings share, kept in the worker isolate.
struct WorkerState {
    outbox: Outbox,
    control: Arc<Control>,
    closing: bool,
    report: Option<Global<Function>>,
}

fn state<'a>(scope: &'a mut PinScope<'_, '_>) -> &'a mut WorkerState {
    scope.get_slot_mut().expect("worker realm installed")
}

impl Spawn {
    fn run(self) {
        let Spawn {
            url,
            module,
            name,
            sources,
            console,
            outbox,
            control,
            inbox,
            commands,
        } = self;
        let mut isolate = v8::Isolate::new(Default::default());
        if control.attach(isolate.thread_safe_handle()) {
            let setup = Setup {
                url,
                module,
                name,
                sources,
                console,
                outbox: outbox.clone(),
                control: Arc::clone(&control),
                commands,
            };
            match Realm::new(&mut isolate, setup) {
                Ok(mut realm) => realm.run(&mut isolate, &inbox),
                Err(error) if !control.is_terminated() => {
                    eprintln!("squark-js: worker setup failed: {error}");
                    outbox.send(Report::Failed);
                }
                Err(_) => {}
            }
        }
        outbox.send(Report::Closed);
    }
}

struct Setup {
    url: Url,
    module: bool,
    name: String,
    sources: SharedSources,
    console: bool,
    outbox: Outbox,
    control: Arc<Control>,
    commands: Sender<Command>,
}

/// One worker realm. Its handles drop before the isolate.
struct Realm {
    context: Global<v8::Context>,
    tasks: TaskLoop,
    network: Global<Function>,
    message: Global<Function>,
    message_error: Global<Function>,
    origin: Instant,
    /// A module entry until its evaluation settles.
    entry: Option<(Url, Global<v8::Promise>)>,
    /// Messages wait until the entry script has run, as in the HTML spec's
    /// port message queue.
    accepting: bool,
    frame: Option<Instant>,
    network_queued: Arc<AtomicBool>,
}

impl Realm {
    fn new(isolate: &mut v8::OwnedIsolate, setup: Setup) -> Result<Self, String> {
        let Setup {
            url,
            module,
            name,
            sources,
            console,
            outbox,
            control,
            commands,
        } = setup;
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let origin = Instant::now();
        let host = install_task_host(scope, move || millis(Instant::now(), origin))
            .map_err(|error| error.to_string())?;
        let sources = ScriptSources::from(sources);
        network::install(scope, host, url.clone(), &sources, false).ok_or("networking")?;
        data::install(scope, host, &url, sources.blobs.clone()).ok_or("blob bindings")?;
        crypto::install(scope, host).ok_or("crypto bindings")?;
        clone_value::install(scope, host).ok_or("clone bindings")?;
        navigation::install(scope, host).ok_or("URL bindings")?;
        wasm::install(scope, host).ok_or("WebAssembly task tracking")?;
        install_host(scope, host, console).ok_or("worker bindings")?;
        loader::install(scope, url.clone(), sources);
        scope.set_slot(WorkerState {
            outbox,
            control,
            closing: false,
            report: None,
        });
        let api = runtime_module(scope, RUNTIME_WORKER).map_err(|error| error.to_string())?;
        let install = function(scope, api, "installWorkerGlobal")?;
        let options = Object::new(scope);
        let url_value = v8::String::new(scope, url.as_str()).ok_or("worker URL")?;
        let name = v8::String::new(scope, &name).ok_or("worker name")?;
        let module_value = v8::Boolean::new(scope, module);
        set(scope, options, "url", url_value.into()).ok_or("worker URL")?;
        set(scope, options, "name", name.into()).ok_or("worker name")?;
        set(scope, options, "module", module_value.into()).ok_or("worker type")?;
        let global = context.global(scope);
        let callbacks = call(
            scope,
            install,
            &[global.into(), host.into(), options.into()],
        )
        .map_err(|error| error.to_string())?;
        let callbacks = Local::<Object>::try_from(callbacks).map_err(|error| error.to_string())?;
        let timing = get(scope, callbacks, "timing")
            .and_then(|timing| Local::<Object>::try_from(timing).ok())
            .ok_or("worker timing callbacks")?;
        let tasks = TaskLoop::new(scope, timing).map_err(|error| error.to_string())?;
        clone_value::prepare(scope).ok_or("clone prototypes")?;
        let fetch_module = function(scope, callbacks, "fetchModule")?;
        loader::install_dev_fetch(scope, fetch_module);
        let report = function(scope, callbacks, "report")?;
        state(scope).report = Some(Global::new(scope, report));
        let network_queued = Arc::new(AtomicBool::new(false));
        let queued = Arc::clone(&network_queued);
        network::set_waker(scope, move || {
            if !queued.swap(true, Ordering::AcqRel) {
                let _ = commands.send(Command::Wake);
            }
        });
        let network = function(scope, callbacks, "network")?;
        let message = function(scope, callbacks, "message")?;
        let message_error = function(scope, callbacks, "messageError")?;
        let mut realm = Self {
            context: Global::new(scope, context),
            tasks,
            network: Global::new(scope, network),
            message: Global::new(scope, message),
            message_error: Global::new(scope, message_error),
            origin,
            entry: None,
            accepting: false,
            frame: None,
            network_queued,
        };
        realm.start(scope, url, module);
        Ok(realm)
    }

    fn start(&mut self, scope: &mut PinScope<'_, '_>, url: Url, module: bool) {
        if module {
            let entry = {
                v8::tc_scope!(let scope, scope);
                loader::evaluate_entry(scope, url.clone())
                    .map(|promise| Global::new(scope, promise))
            };
            scope.perform_microtask_checkpoint();
            match entry {
                Some(promise) => {
                    self.entry = Some((url, promise));
                    self.advance_entry(scope);
                }
                None => fail(scope),
            }
        } else {
            match fetch_script(scope, &url, false) {
                Some((name, source)) => {
                    run_classic(scope, &source, &name);
                    self.accepting = true;
                }
                None => fail(scope),
            }
        }
    }

    fn advance_entry(&mut self, scope: &mut PinScope<'_, '_>) {
        let Some((url, promise)) = &self.entry else {
            return;
        };
        let promise = Local::new(scope, promise);
        match promise.state() {
            v8::PromiseState::Pending => return,
            v8::PromiseState::Fulfilled => self.accepting = true,
            v8::PromiseState::Rejected if loader::module_evaluated(scope, url) => {
                let error = promise.result(scope);
                report_exception(scope, error, None);
                self.accepting = true;
            }
            v8::PromiseState::Rejected => fail(scope),
        }
        self.entry = None;
    }

    fn run(&mut self, isolate: &mut v8::OwnedIsolate, inbox: &Receiver<Command>) {
        let mut pending = VecDeque::new();
        loop {
            let wait = {
                v8::scope!(let scope, isolate);
                let context = Local::new(scope, &self.context);
                let scope = &mut v8::ContextScope::new(scope, context);
                self.turn(scope, &mut pending);
                if stopped(scope) {
                    return;
                }
                self.next_wakeup(scope, &pending)
            };
            let mut command = match wait {
                Some(wait) if wait.is_zero() => inbox.try_recv().map_err(|error| match error {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                }),
                Some(wait) => inbox.recv_timeout(wait),
                None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            loop {
                match command {
                    Ok(Command::Message(message)) => pending.push_back(message),
                    Ok(Command::Wake) => self.network_queued.store(false, Ordering::Release),
                    Ok(Command::Terminate) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => break,
                }
                command = inbox.try_recv().map_err(|_| RecvTimeoutError::Timeout);
            }
        }
    }

    /// One bounded turn: V8's own tasks, network packets, the module graph,
    /// messages, then timers and posted tasks one at a time, and a frame.
    fn turn(&mut self, scope: &mut PinScope<'_, '_>, pending: &mut VecDeque<Message>) {
        let deadline = Instant::now() + TURN_BUDGET;
        let platform = v8::V8::get_current_platform();
        for _ in 0..MAX_TASKS {
            if !v8::Platform::pump_message_loop(&platform, scope, false) {
                break;
            }
            scope.perform_microtask_checkpoint();
        }
        for _ in 0..MAX_TASKS {
            if stopped(scope) {
                return;
            }
            let Some(event) = network::next(scope) else {
                break;
            };
            let dispatch = Local::new(scope, &self.network);
            invoke(scope, dispatch, &[event]);
        }
        if loader::advance_dev(scope) {
            scope.perform_microtask_checkpoint();
        }
        self.advance_entry(scope);
        while self.accepting && !stopped(scope) && Instant::now() < deadline {
            let Some(message) = pending.pop_front() else {
                break;
            };
            match clone_value::deserialize_message(scope, message) {
                Some(data) => {
                    let dispatch = Local::new(scope, &self.message);
                    invoke(scope, dispatch, &[data]);
                }
                None => {
                    let dispatch = Local::new(scope, &self.message_error);
                    invoke(scope, dispatch, &[]);
                }
            }
        }
        // Timers get their own budget, so a stream of messages cannot starve them.
        let deadline = millis(Instant::now() + TURN_BUDGET, self.origin);
        for _ in 0..MAX_TASKS {
            // close() discards the tasks after the one that called it.
            if stopped(scope) || self.tasks.run_ready_with(scope, deadline, 1, &mut invoke) == 0 {
                break;
            }
        }
        if !stopped(scope) {
            self.animate(scope);
        }
    }

    fn animate(&mut self, scope: &mut PinScope<'_, '_>) {
        if !self.tasks.has_animation_callbacks(scope) {
            self.frame = None;
            return;
        }
        let now = Instant::now();
        let frame = *self
            .frame
            .get_or_insert_with(|| next_frame(self.origin, now));
        if now < frame {
            return;
        }
        let time = millis(frame, self.origin);
        if let Err(error) = self.tasks.run_animation_with(scope, time, &mut invoke) {
            eprintln!("squark-js: worker animation frame failed: {error}");
        }
        self.frame = Some(next_frame(self.origin, now));
    }

    fn next_wakeup(
        &self,
        scope: &mut PinScope<'_, '_>,
        pending: &VecDeque<Message>,
    ) -> Option<Duration> {
        if (self.accepting && !pending.is_empty()) || self.tasks.has_posted_tasks(scope) {
            return Some(Duration::ZERO);
        }
        let now = Instant::now();
        let elapsed = millis(now, self.origin);
        let timer = self.tasks.next_timer_deadline(scope).and_then(|deadline| {
            Duration::try_from_secs_f64((deadline - elapsed).max(0.0) / 1000.0).ok()
        });
        let frame = self.frame.map(|frame| frame.saturating_duration_since(now));
        timer
            .into_iter()
            .chain(frame)
            .chain(network::wakeup(scope))
            .chain(wasm::wakeup(scope))
            .min()
    }
}

/// The first 60 Hz frame boundary after `now`.
fn next_frame(origin: Instant, now: Instant) -> Instant {
    let interval = FRAME_INTERVAL.as_nanos();
    let frames = now.saturating_duration_since(origin).as_nanos() / interval + 1;
    origin + Duration::from_nanos((frames * interval) as u64)
}

fn stopped(scope: &mut PinScope<'_, '_>) -> bool {
    let state = state(scope);
    state.closing || state.control.is_terminated()
}

/// The entry script did not load: fire `error` at the Worker and stop.
fn fail(scope: &mut PinScope<'_, '_>) {
    let state = state(scope);
    if !state.control.is_terminated() {
        state.outbox.send(Report::Failed);
    }
    state.closing = true;
}

fn install_host(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    console: bool,
) -> Option<()> {
    for (name, value) in [
        ("os", std::env::consts::OS),
        ("arch", std::env::consts::ARCH),
    ] {
        let value = v8::String::new(scope, value)?;
        set(scope, host, name, value.into())?;
    }
    let threads = std::thread::available_parallelism().map_or(1, |threads| threads.get());
    let threads = v8::Integer::new_from_unsigned(scope, u32::try_from(threads).unwrap_or(u32::MAX));
    set(scope, host, "hardwareConcurrency", threads.into())?;
    for (name, callback) in [
        ("logError", Function::new(scope, bindings::log_error)),
        ("workerPost", Function::new(scope, post_to_document)),
        ("workerClose", Function::new(scope, close)),
        ("workerReport", Function::new(scope, report_error)),
        ("loadScript", Function::new(scope, load_script)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    if console {
        let write = Function::new(scope, bindings::console_write)?;
        set(scope, host, "consoleWrite", write.into())?;
    }
    Some(())
}

fn function<'s>(
    scope: &mut PinScope<'s, '_>,
    object: Local<'_, Object>,
    name: &str,
) -> Result<Local<'s, Function>, String> {
    get(scope, object, name)
        .and_then(|value| Local::try_from(value).ok())
        .ok_or_else(|| format!("missing function {name}"))
}

/// Calls a task callback, reporting an uncaught exception with its location.
fn invoke<'s>(
    scope: &mut PinScope<'s, '_>,
    function: Local<'s, Function>,
    args: &[Local<'s, Value>],
) -> bool {
    let receiver = scope.get_current_context().global(scope);
    let failure = {
        v8::tc_scope!(let scope, scope);
        match function.call(scope, receiver.into(), args) {
            Some(_) => None,
            None => Some((scope.exception(), scope.message())),
        }
    };
    scope.perform_microtask_checkpoint();
    match failure {
        None => true,
        Some((exception, message)) => {
            if let Some(exception) = exception {
                report_exception(scope, exception, message);
            }
            false
        }
    }
}

fn run_classic(scope: &mut PinScope<'_, '_>, source: &str, name: &str) {
    let failure = {
        v8::tc_scope!(let scope, scope);
        let completed = v8::String::new(scope, source)
            .and_then(|code| loader::classic(scope, code, name))
            .is_some();
        (!completed).then(|| (scope.exception(), scope.message()))
    };
    scope.perform_microtask_checkpoint();
    if let Some((Some(exception), message)) = failure {
        report_exception(scope, exception, message);
    }
}

/// Fires `error` at the worker global and, unless a handler canceled it,
/// sends it to the Worker object, as HTML's "report an exception" does.
fn report_exception<'s>(
    scope: &mut PinScope<'s, '_>,
    exception: Local<'s, Value>,
    message: Option<Local<'s, v8::Message>>,
) {
    if scope.is_execution_terminating() {
        return;
    }
    // Chromium locates an Error where it was created, and any other thrown
    // value where it was thrown.
    let message = match message {
        Some(message) if !exception.is_native_error() => message,
        _ => v8::Exception::create_message(scope, exception),
    };
    let filename = message
        .get_script_resource_name(scope)
        .filter(|name| name.is_string())
        .map(|name| name.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let lineno = message.get_line_number(scope).unwrap_or_default();
    let colno = if lineno == 0 {
        0
    } else {
        message.get_start_column() + 1
    };
    let text = message.get(scope).to_rust_string_lossy(scope);
    let lineno = u32::try_from(lineno).unwrap_or(u32::MAX);
    let colno = u32::try_from(colno).unwrap_or(u32::MAX);
    let unhandled = match state(scope).report.clone() {
        Some(report) => {
            let report = Local::new(scope, report);
            let filename_value = v8::String::new(scope, &filename);
            let text_value = v8::String::new(scope, &text);
            let (Some(filename_value), Some(text_value)) = (filename_value, text_value) else {
                return;
            };
            let line = v8::Integer::new_from_unsigned(scope, lineno);
            let column = v8::Integer::new_from_unsigned(scope, colno);
            let args = [
                exception,
                filename_value.into(),
                line.into(),
                column.into(),
                text_value.into(),
            ];
            call(scope, report, &args).map_or(true, |handled| handled.is_true())
        }
        None => true,
    };
    if unhandled && !scope.is_execution_terminating() {
        state(scope).outbox.send(Report::Error {
            message: text,
            filename,
            lineno,
            colno,
        });
    }
}

fn post_to_document(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let Some(message) = clone_value::serialize_message(scope, args.get(0), args.get(1)) {
        state(scope).outbox.send(Report::Message(message));
    }
}

fn close(scope: &mut PinScope<'_, '_>, _: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    state(scope).closing = true;
}

fn report_error<'s>(
    scope: &mut PinScope<'s, '_>,
    args: FunctionCallbackArguments<'s>,
    _: ReturnValue<'_>,
) {
    report_exception(scope, args.get(0), None);
}

fn load_script(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(url) = string(scope, args.get(0)).and_then(|url| Url::parse(&url).ok()) else {
        return;
    };
    if let Some((name, source)) = fetch_script(scope, &url, true)
        && let Some(name) = v8::String::new(scope, &name)
        && let Some(source) = v8::String::new(scope, &source)
    {
        result.set(v8::Array::new_with_elements(scope, &[name.into(), source.into()]).into());
    }
}

/// Reads a classic script synchronously: an object URL, a data: URL, a
/// packed file, or an HTTP response. Returns its final URL and source.
fn fetch_script(scope: &mut PinScope<'_, '_>, url: &Url, import: bool) -> Option<(String, String)> {
    let (name, mime, bytes) = match url.scheme() {
        "blob" => {
            let blob = data::registry(scope).resolve(url.as_str())?;
            (url.to_string(), None, blob.bytes.to_vec())
        }
        "data" => {
            let data = data_url::DataUrl::process(url.as_str()).ok()?;
            (url.to_string(), None, data.decode_to_vec().ok()?.0)
        }
        "http" | "https" if network::is_packed(scope, url) => {
            let path = loader::pack_path(url).ok()?;
            let (mime, bytes) = loader::pack_entry(scope, &path)?;
            (url.to_string(), Some(mime), bytes)
        }
        "http" | "https" => {
            let control = Arc::clone(&state(scope).control);
            let (response, body) =
                network::fetch_blocking(scope, url.clone(), &|| control.is_terminated())?;
            if !(200..300).contains(&response.status) {
                return None;
            }
            let mime = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            (response.url, Some(mime), body)
        }
        _ => return None,
    };
    if let Some(mime) = mime
        && !script_mime(&mime, import)
    {
        return None;
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    Some((name, String::from_utf8_lossy(bytes).into_owned()))
}

/// importScripts needs a JavaScript MIME type. A classic worker script may
/// have another, except the kinds that are never scripts, as in Chromium.
fn script_mime(mime: &str, import: bool) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if import {
        loader::is_javascript(&essence)
    } else {
        !(essence.starts_with("image/")
            || essence.starts_with("audio/")
            || essence.starts_with("video/")
            || essence == "text/csv")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_sources() -> ScriptSources {
        let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/@strange-quark/dom/dist");
        assert!(
            dist.join("worker-global.js").is_file(),
            "Run npm run build --workspace @strange-quark/dom before the worker tests"
        );
        ScriptSources::new(
            |_| None,
            move |path| std::fs::read_to_string(dist.join(path.strip_prefix('/')?)).ok(),
        )
    }

    fn json(scope: &mut PinScope<'_, '_>, value: Local<'_, Value>) -> String {
        v8::json::stringify(scope, value)
            .unwrap()
            .to_rust_string_lossy(scope)
    }

    fn readable(scope: &mut PinScope<'_, '_>, text: &str) -> Message {
        let value = v8::String::new(scope, text).unwrap();
        let transfer = v8::undefined(scope);
        clone_value::serialize_message(scope, value.into(), transfer.into()).unwrap()
    }

    /// No script can post bytes that do not deserialize, so this feeds them
    /// to both ends of the channel directly.
    #[test]
    fn unreadable_messages_fire_messageerror_on_both_sides() {
        crate::initialize_test_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let host = Object::new(scope);
        install(scope, host, &runtime_sources(), false).unwrap();

        let message = readable(scope, "hi");
        workers(scope).local.extend([
            (7, Report::Message(Message::unreadable())),
            (7, Report::Message(message)),
        ]);
        let packet = next(scope).unwrap();
        assert_eq!(json(scope, packet), r#"[7,"messageerror"]"#);
        let packet = next(scope).unwrap();
        assert_eq!(json(scope, packet), r#"[7,"message","hi"]"#);

        let start = get(scope, host, "workerStart").unwrap();
        let start = Local::<Function>::try_from(start).unwrap();
        let script = "data:text/javascript,onmessage=e=>postMessage(['message',e.data]);\
                      onmessageerror=()=>postMessage(['messageerror'])";
        let args = [
            v8::Integer::new(scope, 8).into(),
            v8::String::new(scope, script).unwrap().into(),
            v8::Boolean::new(scope, false).into(),
            v8::String::new(scope, "").unwrap().into(),
        ];
        call(scope, start, &args).unwrap();
        let message = readable(scope, "again");
        let commands = workers(scope).handles[&8].commands.clone();
        commands
            .send(Command::Message(Message::unreadable()))
            .unwrap();
        commands.send(Command::Message(message)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut packets = Vec::new();
        while packets.len() < 2 {
            assert!(
                Instant::now() < deadline,
                "the worker did not answer: {packets:?}"
            );
            match next(scope) {
                Some(packet) => packets.push(json(scope, packet)),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(
            packets,
            [
                r#"[8,"message",["messageerror"]]"#,
                r#"[8,"message",["message","again"]]"#
            ]
        );
        close_all(scope);
        assert!(workers(scope).handles.is_empty());
    }

    #[test]
    fn frames_follow_a_sixty_hertz_clock() {
        let origin = Instant::now();
        assert_eq!(next_frame(origin, origin), origin + FRAME_INTERVAL);
        let later = origin + FRAME_INTERVAL * 3 + Duration::from_millis(1);
        assert_eq!(next_frame(origin, later), origin + FRAME_INTERVAL * 4);
    }

    #[test]
    fn import_scripts_needs_a_javascript_mime_type_but_worker_scripts_do_not() {
        for mime in ["text/javascript", "application/javascript; charset=utf-8"] {
            assert!(script_mime(mime, true));
            assert!(script_mime(mime, false));
        }
        assert!(!script_mime("text/plain", true));
        assert!(script_mime("text/plain", false));
        for mime in ["image/png", "audio/ogg", "video/mp4", "text/csv"] {
            assert!(!script_mime(mime, false), "{mime}");
        }
    }
}
