// SPDX-License-Identifier: MIT
//! Native modules: an app's own Rust code, linked into an app runtime, that
//! JavaScript calls by name (architecture §13).
//!
//! This module is the seam between the document (squark-js, which carries
//! calls, replies and events across V8), the host (squark-host, which owns the
//! window and the dialogs) and the app-facing `squark-native` crate, which
//! implements [`NativeDispatcher`]. Values cross as JSON text, so the
//! document never needs the app's types and apps without native modules link
//! none of this beyond the trait objects.

use std::{fmt, path::PathBuf, sync::Arc};

/// One command call from JavaScript. `args` is a JSON object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCall {
    pub module: String,
    pub command: String,
    pub args: String,
}

/// A command's outcome: `Ok` holds the result as JSON, `Err` the error value
/// as JSON (a JSON string for plain messages), which JavaScript receives as
/// the promise's rejection value, as Tauri apps expect.
pub type NativeResult = Result<String, String>;

/// Delivers a command's outcome. It may run on any thread.
pub type NativeReply = Box<dyn FnOnce(NativeResult) + Send>;

/// The document side: events a module emits for JavaScript listeners.
pub trait NativeEvents: Send + Sync {
    /// `payload` is JSON. Events emitted before a document attaches, or after
    /// it closes, are dropped, as Tauri drops events with no webview.
    fn emit(&self, event: &str, payload: String);
}

/// Window operations, requested by JavaScript or by a module.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowOp {
    SetTitle(String),
    /// Page zoom: CSS pixels per device-independent pixel, as browser zoom.
    SetZoom(f64),
    /// Close the window and end the app, bypassing close interception.
    Close,
    /// Start the app again with the same arguments, then exit.
    Relaunch,
    Exit(i32),
    /// Replace the window's menu; `None` removes it.
    SetMenu(Option<Menu>),
}

/// A window's menu bar: its entries, normally one submenu each.
///
/// Labels may mark a mnemonic with `&` (`"&File"`; `"&&"` is a literal
/// ampersand). Accelerators use Electron's and Tauri's syntax, such as
/// `"CmdOrCtrl+Shift+F"`: modifiers joined by `+`, then one key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// Activating it sends `id` to the modules' menu handlers and to the
    /// page's native API.
    Command {
        id: String,
        label: String,
        accelerator: Option<String>,
        enabled: bool,
        /// `Some` for a check item.
        checked: Option<bool>,
    },
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<MenuItem>,
    },
    Separator,
    /// A standard item the runtime implements. `label` replaces the
    /// default label and `accelerator` the default accelerator.
    Role {
        role: MenuRole,
        label: Option<String>,
        accelerator: Option<String>,
    },
}

impl MenuItem {
    pub fn command(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Command {
            id: id.into(),
            label: label.into(),
            accelerator: None,
            enabled: true,
            checked: None,
        }
    }

    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self::Submenu {
            label: label.into(),
            enabled: true,
            items,
        }
    }

    pub fn role(role: MenuRole) -> Self {
        Self::Role {
            role,
            label: None,
            accelerator: None,
        }
    }

    /// Sets the accelerator of a command or role item; others are unchanged.
    pub fn with_accelerator(mut self, value: impl Into<String>) -> Self {
        if let Self::Command { accelerator, .. } | Self::Role { accelerator, .. } = &mut self {
            *accelerator = Some(value.into());
        }
        self
    }
}

/// Standard menu items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuRole {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// Asks to close the window, as the title bar's close button does.
    CloseWindow,
    /// Asks to close the window and end the app.
    Quit,
    About(AboutInfo),
}

/// What an About item shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AboutInfo {
    pub name: Option<String>,
    pub version: Option<String>,
    pub comments: Option<String>,
    pub copyright: Option<String>,
    pub website: Option<String>,
}

/// A native dialog a module asks for. Paths come back absolute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogRequest {
    PickFolder {
        title: Option<String>,
        directory: Option<PathBuf>,
    },
    PickFiles {
        title: Option<String>,
        directory: Option<PathBuf>,
        /// `(name, extensions without dots)` pairs.
        filters: Vec<(String, Vec<String>)>,
        multiple: bool,
    },
    SaveFile {
        title: Option<String>,
        directory: Option<PathBuf>,
        file_name: Option<String>,
        filters: Vec<(String, Vec<String>)>,
    },
}

/// The host side: what a module may ask of the window system.
pub trait NativeHost: Send + Sync {
    fn window(&self, op: WindowOp);
    /// Blocks the calling (module worker) thread until the user answers.
    /// `None` when cancelled.
    fn dialog(&self, request: DialogRequest) -> Option<Vec<PathBuf>>;
    /// Opens an http, https or mailto URL in the system's handler.
    fn open_external(&self, url: &str) -> Result<(), String>;
}

/// What a decision on a close request means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseRequest {
    /// Close now.
    Allow,
    /// Keep the window open; a module will close it with [`WindowOp::Close`]
    /// once the app agrees (for example, after asking about unsaved files).
    Prevent,
}

/// The app-facing registry of native modules, linked into an app runtime.
pub trait NativeDispatcher: Send + Sync {
    /// Module names this runtime provides.
    fn modules(&self) -> Vec<String>;
    /// Called once at startup with the host and the modules the pack's
    /// manifest enables (`native.modules` in squark.config.ts). Calls to
    /// other modules must fail.
    fn start(&self, host: Arc<dyn NativeHost>, enabled: &[String]);
    /// Called when a document starts; its events go to `events`.
    fn attach(&self, events: Arc<dyn NativeEvents>);
    /// Run a command. `reply` must be called exactly once, on any thread.
    fn invoke(&self, call: NativeCall, reply: NativeReply);
    /// The user asked to close the window (title bar, Alt+F4, ⌘Q).
    fn close_requested(&self) -> CloseRequest;
    /// The user activated the menu command `id`, by its item or its
    /// accelerator. Handlers must not block the caller (the window's event
    /// loop).
    fn menu(&self, _id: &str) {}
    /// The app is ending: the window closed or the app exited. Called once,
    /// after the page has stopped, so modules can stop child processes.
    fn exit(&self) {}
}

impl fmt::Debug for dyn NativeDispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeDispatcher")
            .field("modules", &self.modules())
            .finish()
    }
}

impl fmt::Debug for dyn NativeHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeHost")
    }
}

impl fmt::Debug for dyn NativeEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeEvents")
    }
}
