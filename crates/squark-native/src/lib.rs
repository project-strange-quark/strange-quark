// SPDX-License-Identifier: MIT
//! Native modules for Strange Quark apps (architecture §13).
//!
//! A native module is a set of Rust commands that the app's JavaScript calls
//! by name, and events it pushes to JavaScript listeners. It is linked into
//! an app runtime, so it costs no extra process or IPC.
//!
//! ```ignore
//! use squark_native::{App, Module, State, export, handlers};
//!
//! #[export]
//! fn greet(name: String) -> String {
//!     format!("Hello, {name}!")
//! }
//!
//! #[export]
//! async fn watch(app: App, path: String) -> Result<(), String> {
//!     app.emit("watching", &path).map_err(|error| error.to_string())
//! }
//!
//! pub fn module() -> Module {
//!     Module::new("demo").commands(handlers![greet, watch])
//! }
//! ```
//!
//! JavaScript reaches it through the runtime's native API
//! (`invoke("demo", "greet", { name: "Ada" })`). A module runs only when the
//! app's `squark.config.ts` lists it under `native.modules`.
//!
//! Commands receive their arguments as one JSON object whose keys are the
//! parameter names in camelCase (`rename_all = "snake_case"` keeps them). A plain return value is the result; a
//! `Result` rejects the JavaScript promise with its error, serialized. Sync
//! commands run on a blocking thread and async commands on the runtime the
//! app's networking uses, so a module adds no threads of its own.

use std::{
    any::{Any, TypeId},
    collections::HashMap,
    fmt,
    future::Future,
    ops::Deref,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, RwLock},
};

use serde::Serialize;
pub use squark_native_macros::{export, handlers};
pub use squark_types::native::{CloseRequest, DialogRequest, WindowOp};
use squark_types::native::{
    NativeCall, NativeDispatcher, NativeEvents, NativeHost, NativeReply, NativeResult,
};

/// How a command runs: on a blocking thread, or as a future.
pub enum Invocation {
    Blocking(Box<dyn FnOnce() -> NativeResult + Send>),
    Async(Pin<Box<dyn Future<Output = NativeResult> + Send>>),
}

impl fmt::Debug for Invocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Blocking(_) => "Invocation::Blocking",
            Self::Async(_) => "Invocation::Async",
        })
    }
}

/// A command `#[export]` generates. Implement it by hand only for commands
/// whose arguments need custom parsing.
pub trait Command: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// `args` is the JSON object JavaScript passed.
    fn invoke(&self, app: App, args: String) -> Invocation;
}

/// Managed state handed to commands: `State<T>` parameters, or `App::state`.
pub struct State<T: ?Sized>(Arc<T>);

impl<T: ?Sized> Clone for State<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: ?Sized> Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for State<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: ?Sized> State<T> {
    /// The shared allocation, for moving into threads.
    pub fn inner(&self) -> Arc<T> {
        Arc::clone(&self.0)
    }
}

type CloseHandler = Arc<dyn Fn(&App) -> CloseRequest + Send + Sync>;
type SetupHandler = Box<dyn FnOnce(&App) + Send>;
type ExitHandler = Box<dyn FnOnce(&App) + Send>;

/// A named set of commands, the state they share, and lifecycle hooks.
pub struct Module {
    name: String,
    commands: Vec<Box<dyn Command>>,
    states: Vec<(TypeId, Arc<dyn Any + Send + Sync>)>,
    setup: Vec<SetupHandler>,
    on_close: Option<CloseHandler>,
    on_exit: Vec<ExitHandler>,
}

impl fmt::Debug for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Module")
            .field("name", &self.name)
            .field(
                "commands",
                &self.commands.iter().map(|command| command.name()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            commands: Vec::new(),
            states: Vec::new(),
            setup: Vec::new(),
            on_close: None,
            on_exit: Vec::new(),
        }
    }

    /// Add commands, usually `handlers![...]`.
    pub fn commands(mut self, commands: Vec<Box<dyn Command>>) -> Self {
        self.commands.extend(commands);
        self
    }

    /// Share a value with every command of every module, as `State<T>`.
    pub fn manage<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        self.states.push((TypeId::of::<T>(), Arc::new(state)));
        self
    }

    /// Run once at startup, after the host is ready and before any command.
    pub fn setup(mut self, setup: impl FnOnce(&App) + Send + 'static) -> Self {
        self.setup.push(Box::new(setup));
        self
    }

    /// Decide what happens when the user asks to close the window. Return
    /// `CloseRequest::Prevent` to keep it open (and close it later with
    /// `app.window().close()`), for example to ask about unsaved work.
    pub fn on_close_requested(
        mut self,
        handler: impl Fn(&App) -> CloseRequest + Send + Sync + 'static,
    ) -> Self {
        self.on_close = Some(Arc::new(handler));
        self
    }

    /// Run once when the app ends (the window closed or the app exited),
    /// after the page has stopped: stop child processes here.
    pub fn on_exit(mut self, handler: impl FnOnce(&App) + Send + 'static) -> Self {
        self.on_exit.push(Box::new(handler));
        self
    }
}

struct Shared {
    states: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    events: RwLock<Option<Arc<dyn NativeEvents>>>,
    host: OnceLock<Arc<dyn NativeHost>>,
    runtime: OnceLock<tokio::runtime::Handle>,
}

/// The app as commands see it: state, events and the window. Cheap to clone
/// and safe to move to other threads.
#[derive(Clone)]
pub struct App(Arc<Shared>);

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("App")
    }
}

impl App {
    fn new() -> Self {
        Self(Arc::new(Shared {
            states: RwLock::default(),
            events: RwLock::default(),
            host: OnceLock::new(),
            runtime: OnceLock::new(),
        }))
    }

    /// Send `payload`, serialized, to every JavaScript listener of `event`.
    /// Events before the page starts or after it closes are dropped, which
    /// is not an error.
    pub fn emit<P: Serialize>(&self, event: &str, payload: P) -> Result<(), Error> {
        let json = serde_json::to_string(&payload).map_err(|error| Error(error.to_string()))?;
        let events = self
            .0
            .events
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(events) = events {
            events.emit(event, json);
        }
        Ok(())
    }

    /// State registered with `Module::manage` or `App::manage`, if any.
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<State<T>> {
        let states = self
            .0
            .states
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        let state = states.get(&TypeId::of::<T>())?.clone();
        state.downcast::<T>().ok().map(State)
    }

    /// Add state at runtime. Returns false if a `T` is already managed.
    pub fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool {
        let mut states = self
            .0
            .states
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if states.contains_key(&TypeId::of::<T>()) {
            return false;
        }
        states.insert(TypeId::of::<T>(), Arc::new(state));
        true
    }

    /// The runtime async commands run on, for spawning background work.
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.0
            .runtime
            .get_or_init(|| {
                squark_net::shared_runtime()
                    .or_else(|| tokio::runtime::Handle::try_current().ok())
                    .expect("squark-native: no async runtime")
            })
            .clone()
    }

    pub fn window(&self) -> Window {
        Window(self.0.host.get().cloned())
    }

    /// Show a native dialog. Blocks the calling thread, so call it from a
    /// sync command or `spawn_blocking`, never from an async command.
    pub fn dialog(&self, request: DialogRequest) -> Option<Vec<PathBuf>> {
        self.0.host.get()?.dialog(request)
    }

    /// Open an http, https or mailto URL in the system's handler.
    pub fn open_url(&self, url: &str) -> Result<(), String> {
        match self.0.host.get() {
            Some(host) => host.open_external(url),
            None => Err("the window system is not available".into()),
        }
    }
}

/// An event payload that could not be serialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The app window, for module code.
#[derive(Clone)]
pub struct Window(Option<Arc<dyn NativeHost>>);

impl fmt::Debug for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Window")
    }
}

impl Window {
    fn op(&self, op: WindowOp) {
        if let Some(host) = &self.0 {
            host.window(op);
        }
    }
    pub fn set_title(&self, title: impl Into<String>) {
        self.op(WindowOp::SetTitle(title.into()));
    }
    pub fn set_zoom(&self, factor: f64) {
        self.op(WindowOp::SetZoom(factor));
    }
    /// Close the window and end the app, even if close requests are prevented.
    pub fn close(&self) {
        self.op(WindowOp::Close);
    }
    pub fn relaunch(&self) {
        self.op(WindowOp::Relaunch);
    }
    pub fn exit(&self, code: i32) {
        self.op(WindowOp::Exit(code));
    }
}

struct Entry {
    module: String,
    command: Box<dyn Command>,
}

/// The modules of an app runtime, as the host sees them.
pub struct Dispatcher {
    app: App,
    modules: Vec<String>,
    commands: HashMap<(String, String), Entry>,
    enabled: RwLock<Vec<String>>,
    setup: Mutex<Vec<SetupHandler>>,
    on_close: Vec<CloseHandler>,
    on_exit: Mutex<Vec<ExitHandler>>,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher")
            .field("modules", &self.modules)
            .finish_non_exhaustive()
    }
}

/// Collect modules into the dispatcher `squark_host::runtime_main` takes.
pub fn dispatcher(modules: impl IntoIterator<Item = Module>) -> Arc<Dispatcher> {
    let app = App::new();
    let mut names = Vec::new();
    let mut commands = HashMap::new();
    let mut setup = Vec::new();
    let mut on_close = Vec::new();
    let mut on_exit = Vec::new();
    for module in modules {
        for (id, state) in module.states {
            app.0
                .states
                .write()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(id, state);
        }
        for command in module.commands {
            let key = (module.name.clone(), command.name().to_owned());
            commands.insert(
                key,
                Entry {
                    module: module.name.clone(),
                    command,
                },
            );
        }
        setup.extend(module.setup);
        on_close.extend(module.on_close);
        on_exit.extend(module.on_exit);
        names.push(module.name);
    }
    Arc::new(Dispatcher {
        app,
        modules: names,
        commands,
        enabled: RwLock::default(),
        setup: Mutex::new(setup),
        on_close,
        on_exit: Mutex::new(on_exit),
    })
}

impl Dispatcher {
    /// The app handle, for tests and for code that drives modules directly.
    pub fn app(&self) -> &App {
        &self.app
    }

    fn error(message: impl fmt::Display) -> NativeResult {
        Err(serde_json::to_string(&message.to_string()).unwrap_or_else(|_| "\"error\"".into()))
    }
}

impl NativeDispatcher for Dispatcher {
    fn modules(&self) -> Vec<String> {
        self.enabled
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn start(&self, host: Arc<dyn NativeHost>, enabled: &[String]) {
        let _ = self.app.0.host.set(host);
        *self.enabled.write().unwrap_or_else(|poison| poison.into_inner()) = self
            .modules
            .iter()
            .filter(|name| enabled.contains(name))
            .cloned()
            .collect();
        for name in enabled {
            if !self.modules.contains(name) {
                eprintln!(
                    "squark-native: squark.config.ts enables module {name:?}, which this runtime does not contain"
                );
            }
        }
        let setup = std::mem::take(&mut *self.setup.lock().unwrap_or_else(|poison| poison.into_inner()));
        for run in setup {
            run(&self.app);
        }
    }

    fn attach(&self, events: Arc<dyn NativeEvents>) {
        *self
            .app
            .0
            .events
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(events);
    }

    fn invoke(&self, call: NativeCall, reply: NativeReply) {
        let enabled = self
            .enabled
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(&call.module);
        if !enabled {
            reply(Self::error(format_args!(
                "native module {:?} is not enabled in squark.config.ts",
                call.module
            )));
            return;
        }
        let Some(entry) = self.commands.get(&(call.module.clone(), call.command.clone())) else {
            reply(Self::error(format_args!(
                "native module {:?} has no command {:?}",
                call.module, call.command
            )));
            return;
        };
        debug_assert_eq!(entry.module, call.module);
        let runtime = self.app.runtime();
        let name = call.command;
        match entry.command.invoke(self.app.clone(), call.args) {
            Invocation::Blocking(run) => {
                runtime.spawn_blocking(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                        .unwrap_or_else(|_| Self::error(format_args!("command {name:?} panicked")));
                    reply(result);
                });
            }
            Invocation::Async(future) => {
                let task = runtime.spawn(future);
                runtime.spawn(async move {
                    reply(
                        task.await
                            .unwrap_or_else(|_| Self::error(format_args!("command {name:?} panicked"))),
                    );
                });
            }
        }
    }

    fn close_requested(&self) -> CloseRequest {
        let prevent = self
            .on_close
            .iter()
            .any(|handler| handler(&self.app) == CloseRequest::Prevent);
        if prevent {
            CloseRequest::Prevent
        } else {
            CloseRequest::Allow
        }
    }

    fn exit(&self) {
        let handlers =
            std::mem::take(&mut *self.on_exit.lock().unwrap_or_else(|poison| poison.into_inner()));
        for handler in handlers {
            handler(&self.app);
        }
    }
}

/// Used by `#[export]`; not part of the API.
#[doc(hidden)]
pub mod __private {
    pub use serde;
    use serde::{Serialize, de::DeserializeOwned};
    use squark_types::native::NativeResult;

    pub fn parse_args<T: DeserializeOwned>(command: &str, args: &str) -> Result<T, String> {
        serde_json::from_str(if args.is_empty() { "{}" } else { args }).map_err(|error| {
            json_error(format_args!("invalid arguments for command {command:?}: {error}"))
        })
    }

    pub fn missing_state(command: &str, type_name: &str) -> String {
        json_error(format_args!(
            "command {command:?} needs state {type_name}, which no module manages"
        ))
    }

    fn json_error(message: std::fmt::Arguments<'_>) -> String {
        serde_json::to_string(&message.to_string()).unwrap_or_else(|_| "\"error\"".into())
    }

    pub fn ok<T: Serialize + ?Sized>(value: &T) -> NativeResult {
        serde_json::to_string(value)
            .map_err(|error| json_error(format_args!("could not serialize the result: {error}")))
    }

    pub fn result<T: Serialize, E: Serialize>(value: Result<T, E>) -> NativeResult {
        match value {
            Ok(value) => ok(&value),
            Err(error) => Err(serde_json::to_string(&error).unwrap_or_else(|error| {
                json_error(format_args!("could not serialize the error: {error}"))
            })),
        }
    }
}
