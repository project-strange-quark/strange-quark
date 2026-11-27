// SPDX-License-Identifier: MIT
//! Optional Tauri 2 compatibility for Strange Quark native modules.
//!
//! This crate gives a Tauri backend's commands the API they were written
//! against, on top of `squark-native`, so they compile as a Strange Quark
//! native module without edits. Strange Quark itself does not use it; an
//! app opts in. Build the backend with this crate named `tauri` (and, for
//! the dialog plugin's API, `tauri_plugin_dialog`):
//!
//! ```ignore
//! #[cfg(feature = "squark")]
//! extern crate squark_tauri as tauri;
//! #[cfg(feature = "squark")]
//! extern crate squark_tauri as tauri_plugin_dialog;
//!
//! #[tauri::command]
//! async fn list(app: tauri::AppHandle, state: tauri::State<'_, Db>, dir: String) -> Result<Vec<String>, String> {
//!     use tauri::Emitter;
//!     app.emit("listing", &dir).map_err(|e| e.to_string())?;
//!     state.list(&dir)
//! }
//!
//! pub fn modules() -> Vec<squark_native::Module> {
//!     vec![squark_native::Module::new("app")
//!         .manage(Db::default())
//!         .commands(squark_tauri::generate_handler![list])]
//! }
//! ```
//!
//! What maps: commands (sync and async, camelCase arguments, `rename_all`),
//! `AppHandle`, `Manager::{state, try_state, manage}`, `Emitter::emit`,
//! `State<'_, T>` (with `inner()`), `Window`/`WebviewWindow` (`emit`,
//! `close`, `destroy`, `set_title`), `async_runtime::{spawn,
//! spawn_blocking}`, `http` (the `http` crate's types) and the dialog
//! plugin's `DialogExt` file dialogs. Builder setup, plugins, menus, URI
//! schemes and window events have no equivalent here: register state,
//! setup and close handling on `squark_native::Module` instead. The
//! frontend side is the `@strange-quark/tauri` package.

use std::{fmt, marker::PhantomData, ops::Deref, path::PathBuf};

use serde::Serialize;
pub use squark_tauri_macros::{command, generate_handler};

/// Tauri's error type, for `tauri::Result`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// As Tauri's: a command returning `tauri::Result` rejects with the message.
impl Serialize for Error {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The app handle Tauri commands take.
#[derive(Clone, Debug)]
pub struct AppHandle(squark_native::App);

impl AppHandle {
    pub fn from_native(app: squark_native::App) -> Self {
        Self(app)
    }

    /// The Strange Quark app handle underneath.
    pub fn native(&self) -> &squark_native::App {
        &self.0
    }

    /// Strange Quark has one window; Tauri's default label is "main".
    pub fn main_window(&self) -> Window {
        Window {
            app: self.clone(),
            label: "main".to_owned(),
        }
    }

    pub fn get_webview_window(&self, label: &str) -> Option<WebviewWindow> {
        (label == "main").then(|| self.main_window())
    }

    pub fn exit(&self, code: i32) {
        self.0.window().exit(code);
    }

    pub fn restart(&self) {
        self.0.window().relaunch();
    }
}

/// Tauri's `Manager`: managed state.
pub trait Manager {
    /// # Panics
    /// If no module manages a `T`, as Tauri's does.
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T>;
    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>>;
    /// Returns false if a `T` is already managed.
    fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool;
}

impl Manager for AppHandle {
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
        self.try_state().unwrap_or_else(|| {
            panic!(
                "state {} is not managed; add it with Module::manage",
                std::any::type_name::<T>()
            )
        })
    }

    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>> {
        self.0.state::<T>().map(|state| State(state, PhantomData))
    }

    fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool {
        self.0.manage(state)
    }
}

impl Manager for Window {
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
        self.app.state()
    }
    fn try_state<T: Send + Sync + 'static>(&self) -> Option<State<'_, T>> {
        self.app.try_state()
    }
    fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool {
        self.app.manage(state)
    }
}

/// Tauri's `Emitter`: events to the frontend's `listen()`.
pub trait Emitter {
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) -> Result<()>;
}

impl Emitter for AppHandle {
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) -> Result<()> {
        self.0
            .emit(event, payload)
            .map_err(|error| Error(error.to_string()))
    }
}

impl Emitter for Window {
    fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) -> Result<()> {
        self.app.emit(event, payload)
    }
}

/// Managed state in a command's parameters: `State<'_, T>`.
pub struct State<'r, T: Send + Sync + 'static>(squark_native::State<T>, PhantomData<&'r ()>);

impl<T: Send + Sync + 'static> State<'_, T> {
    /// The managed value, as Tauri's `State::inner`.
    pub fn inner(&self) -> &T {
        &self.0
    }

    #[doc(hidden)]
    pub fn __from_app(app: &squark_native::App, command: &str) -> Self {
        let state = app.state::<T>().unwrap_or_else(|| {
            panic!(
                "command {command:?} needs state {}, which no module manages",
                std::any::type_name::<T>()
            )
        });
        State(state, PhantomData)
    }
}

impl<T: Send + Sync + 'static> Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Send + Sync + 'static> Clone for State<'_, T> {
    fn clone(&self) -> Self {
        State(self.0.clone(), PhantomData)
    }
}

impl<T: Send + Sync + fmt::Debug + 'static> fmt::Debug for State<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

/// The app's window, as Tauri's `Window` and `WebviewWindow`.
#[derive(Clone, Debug)]
pub struct Window {
    app: AppHandle,
    label: String,
}

pub type WebviewWindow = Window;

impl Window {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn app_handle(&self) -> &AppHandle {
        &self.app
    }

    /// Close the window and end the app (Strange Quark has one window).
    pub fn close(&self) -> Result<()> {
        self.app.0.window().close();
        Ok(())
    }

    /// As `close`: the window goes without another close request.
    pub fn destroy(&self) -> Result<()> {
        self.close()
    }

    pub fn set_title(&self, title: &str) -> Result<()> {
        self.app.0.window().set_title(title);
        Ok(())
    }
}

/// Tauri's `async_runtime`, on the runtime native modules run on.
pub mod async_runtime {
    use std::future::Future;

    pub use tokio::task::JoinHandle;

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        squark_native::runtime().spawn(future)
    }

    pub fn spawn_blocking<F, R>(work: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        squark_native::runtime().spawn_blocking(work)
    }

    pub fn block_on<F: Future>(future: F) -> F::Output {
        squark_native::runtime().block_on(future)
    }
}

/// Tauri re-exports the `http` crate as `tauri::http`.
pub mod http {
    pub use http::*;
}

/// A path a dialog returned, as the dialog plugin's `FilePath`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePath {
    Path(PathBuf),
}

impl FilePath {
    pub fn into_path(self) -> Result<PathBuf> {
        let Self::Path(path) = self;
        Ok(path)
    }

    pub fn as_path(&self) -> Option<&std::path::Path> {
        let Self::Path(path) = self;
        Some(path)
    }
}

impl fmt::Display for FilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Path(path) = self;
        fmt::Display::fmt(&path.display(), f)
    }
}

/// The dialog plugin's `DialogExt`: `app.dialog().file()…blocking_pick_file()`.
pub trait DialogExt {
    fn dialog(&self) -> Dialog;
}

impl DialogExt for AppHandle {
    fn dialog(&self) -> Dialog {
        Dialog(self.clone())
    }
}

#[derive(Clone, Debug)]
pub struct Dialog(AppHandle);

impl Dialog {
    pub fn file(&self) -> FileDialogBuilder {
        FileDialogBuilder {
            app: self.0.clone(),
            title: None,
            directory: None,
            file_name: None,
            filters: Vec::new(),
        }
    }
}

/// The dialog plugin's file dialog builder.
#[derive(Clone, Debug)]
pub struct FileDialogBuilder {
    app: AppHandle,
    title: Option<String>,
    directory: Option<PathBuf>,
    file_name: Option<String>,
    filters: Vec<(String, Vec<String>)>,
}

impl FileDialogBuilder {
    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[&str]) -> Self {
        self.filters.push((
            name.into(),
            extensions.iter().map(|ext| (*ext).to_owned()).collect(),
        ));
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn set_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = Some(directory.into());
        self
    }

    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    /// Native dialogs cannot express Tauri's `"*"` extension; with one of
    /// those filters present, offer every file rather than hide some.
    fn filters(&self) -> Vec<(String, Vec<String>)> {
        if self
            .filters
            .iter()
            .any(|(_, extensions)| extensions.iter().any(|ext| ext == "*"))
        {
            Vec::new()
        } else {
            self.filters.clone()
        }
    }

    fn show(&self, request: squark_native::DialogRequest) -> Option<Vec<FilePath>> {
        let app = self.app.0.clone();
        let ask = move || app.dialog(request);
        // A blocking call from inside an async command must not stall a
        // runtime worker: hand the worker's other tasks to another thread.
        let paths = match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(ask)
            }
            _ => ask(),
        }?;
        Some(paths.into_iter().map(FilePath::Path).collect())
    }

    pub fn blocking_pick_file(self) -> Option<FilePath> {
        self.show(squark_native::DialogRequest::PickFiles {
            title: self.title.clone(),
            directory: self.directory.clone(),
            filters: self.filters(),
            multiple: false,
        })?
        .into_iter()
        .next()
    }

    pub fn blocking_pick_files(self) -> Option<Vec<FilePath>> {
        self.show(squark_native::DialogRequest::PickFiles {
            title: self.title.clone(),
            directory: self.directory.clone(),
            filters: self.filters(),
            multiple: true,
        })
    }

    pub fn blocking_pick_folder(self) -> Option<FilePath> {
        self.show(squark_native::DialogRequest::PickFolder {
            title: self.title.clone(),
            directory: self.directory.clone(),
        })?
        .into_iter()
        .next()
    }

    pub fn blocking_save_file(self) -> Option<FilePath> {
        self.show(squark_native::DialogRequest::SaveFile {
            title: self.title.clone(),
            directory: self.directory.clone(),
            file_name: self.file_name.clone(),
            filters: self.filters(),
        })?
        .into_iter()
        .next()
    }
}
