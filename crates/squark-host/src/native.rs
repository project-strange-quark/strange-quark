// SPDX-License-Identifier: MIT

//! The host side of native modules (architecture §13): window operations,
//! dialogs and external links for module code, and the window sink that page
//! window operations share.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use squark_types::native::{DialogRequest, NativeDispatcher, NativeHost, WindowOp};

use crate::services::{FileDialogKind, FileDialogQueue, open_external_url};

/// Applies a window operation. Returns false when the window has gone.
pub(crate) type WindowSink = Arc<dyn Fn(WindowOp) -> bool + Send + Sync>;

/// Window operations of a run without a real window (headless or test
/// control), kept so tests can read what the app asked for.
#[derive(Clone, Default)]
pub(crate) struct RecordedWindow(Arc<Mutex<Vec<WindowOp>>>);

impl RecordedWindow {
    pub(crate) fn sink(&self) -> WindowSink {
        let ops = Arc::clone(&self.0);
        Arc::new(move |op| {
            ops.lock().unwrap_or_else(|poison| poison.into_inner()).push(op);
            true
        })
    }

    #[cfg(test)]
    pub(crate) fn ops(&self) -> Vec<WindowOp> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

/// Where module dialogs come from: the system, or paths a test queued.
#[derive(Clone)]
pub(crate) enum ModuleDialogs {
    System,
    Queued(FileDialogQueue),
}

/// The modules a pack enables: `native.modules` in its manifest.
pub(crate) fn enabled_modules(manifest: Option<&serde_json::Value>) -> Vec<String> {
    manifest
        .and_then(|manifest| manifest.pointer("/native/modules"))
        .and_then(serde_json::Value::as_array)
        .map(|modules| {
            modules
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// An app runtime's native modules and the host they are started with.
#[derive(Clone)]
pub(crate) struct NativeRuntime {
    pub(crate) dispatcher: Arc<dyn NativeDispatcher>,
}

impl NativeRuntime {
    pub(crate) fn start(
        &self,
        window: WindowSink,
        dialogs: ModuleDialogs,
        manifest: Option<&serde_json::Value>,
    ) {
        let enabled = enabled_modules(manifest);
        let host: Arc<dyn NativeHost> = Arc::new(HostBridge { window, dialogs });
        self.dispatcher.start(host, &enabled);
    }
}

struct HostBridge {
    window: WindowSink,
    dialogs: ModuleDialogs,
}

impl NativeHost for HostBridge {
    fn window(&self, op: WindowOp) {
        let _ = (self.window)(op);
    }

    fn dialog(&self, request: DialogRequest) -> Option<Vec<PathBuf>> {
        match &self.dialogs {
            ModuleDialogs::Queued(queue) => {
                let kind = match request {
                    DialogRequest::SaveFile { .. } => FileDialogKind::Save,
                    _ => FileDialogKind::Open,
                };
                queue.pop(kind).map(|path| vec![path])
            }
            ModuleDialogs::System => system_dialog(request),
        }
    }

    fn open_external(&self, url: &str) -> Result<(), String> {
        open_external_url(url).map_err(|error| error.message)
    }
}

fn dialog(
    title: Option<String>,
    directory: Option<PathBuf>,
    filters: &[(String, Vec<String>)],
) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new();
    if let Some(title) = title {
        dialog = dialog.set_title(title);
    }
    if let Some(directory) = directory {
        dialog = dialog.set_directory(directory);
    }
    for (name, extensions) in filters {
        dialog = dialog.add_filter(name, extensions);
    }
    dialog
}

fn system_dialog(request: DialogRequest) -> Option<Vec<PathBuf>> {
    match request {
        DialogRequest::PickFolder { title, directory } => dialog(title, directory, &[])
            .pick_folder()
            .map(|path| vec![path]),
        DialogRequest::PickFiles {
            title,
            directory,
            filters,
            multiple,
        } => {
            let dialog = dialog(title, directory, &filters);
            if multiple {
                dialog.pick_files()
            } else {
                dialog.pick_file().map(|path| vec![path])
            }
        }
        DialogRequest::SaveFile {
            title,
            directory,
            file_name,
            filters,
        } => {
            let mut dialog = dialog(title, directory, &filters);
            if let Some(name) = file_name {
                dialog = dialog.set_file_name(name);
            }
            dialog.save_file().map(|path| vec![path])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lists_the_enabled_modules() {
        let manifest = serde_json::json!({ "native": { "modules": ["julide", 3, "extra"] } });
        assert_eq!(enabled_modules(Some(&manifest)), ["julide", "extra"]);
        assert!(enabled_modules(Some(&serde_json::json!({}))).is_empty());
        assert!(enabled_modules(None).is_empty());
    }

    #[test]
    fn queued_dialogs_answer_module_picks_in_order() {
        let queue = FileDialogQueue::default();
        queue.push(FileDialogKind::Open, PathBuf::from("/work/project"));
        queue.push(FileDialogKind::Save, PathBuf::from("/work/out.txt"));
        let bridge = HostBridge {
            window: RecordedWindow::default().sink(),
            dialogs: ModuleDialogs::Queued(queue),
        };
        let folder = bridge.dialog(DialogRequest::PickFolder {
            title: None,
            directory: None,
        });
        assert_eq!(folder, Some(vec![PathBuf::from("/work/project")]));
        let save = bridge.dialog(DialogRequest::SaveFile {
            title: None,
            directory: None,
            file_name: None,
            filters: Vec::new(),
        });
        assert_eq!(save, Some(vec![PathBuf::from("/work/out.txt")]));
        assert_eq!(
            bridge.dialog(DialogRequest::PickFolder {
                title: None,
                directory: None
            }),
            None
        );
    }

    #[test]
    fn recorded_windows_keep_operations_in_order() {
        let recorded = RecordedWindow::default();
        let sink = recorded.sink();
        assert!(sink(WindowOp::SetTitle("a".into())));
        assert!(sink(WindowOp::SetZoom(1.5)));
        assert_eq!(
            recorded.ops(),
            [WindowOp::SetTitle("a".into()), WindowOp::SetZoom(1.5)]
        );
    }
}
