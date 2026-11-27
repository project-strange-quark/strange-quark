// SPDX-License-Identifier: MIT
//! A Tauri backend written against `tauri`, compiled against squark-tauri.

extern crate squark_tauri as tauri;
extern crate squark_tauri as tauri_plugin_dialog;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use squark_native::{Module, dispatcher};
use squark_types::native::{
    DialogRequest, NativeCall, NativeDispatcher, NativeEvents, NativeHost, WindowOp,
};
use tauri::{Emitter, Manager};

type Shared = Arc<tokio::sync::Mutex<Vec<String>>>;

#[tauri::command]
fn greet(full_name: String) -> String {
    format!("Hello, {full_name}!")
}

#[tauri::command]
async fn remember(state: tauri::State<'_, Shared>, item: String) -> Result<usize, String> {
    let mut lock = state.lock().await;
    lock.push(item);
    let shared = state.inner().clone();
    Ok(lock.len().max(Arc::strong_count(&shared).min(1)))
}

#[tauri::command]
async fn announce(app: tauri::AppHandle, session_id: String) -> Result<(), String> {
    app.emit("announced", serde_json::json!({ "session_id": session_id }))
        .map_err(|error| error.to_string())?;
    let count = app.state::<Shared>().lock().await.len();
    let blocking = tauri::async_runtime::spawn_blocking(move || count * 2)
        .await
        .map_err(|error| format!("join: {error}"))?;
    assert_eq!(blocking, count * 2);
    Ok(())
}

#[tauri::command]
async fn open_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .add_filter("Julia Files", &["jl"])
        .add_filter("All Files", &["*"])
        .blocking_pick_file();
    Ok(path.map(|p| p.to_string()))
}

#[tauri::command(rename_all = "snake_case")]
fn echo(raw_value: u32) -> u32 {
    raw_value
}

#[tauri::command]
pub fn quit(window: tauri::Window) {
    let _ = window.destroy();
}

mod nested {
    #[tauri::command]
    pub fn status() -> tauri::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<(String, String)>>);

impl NativeEvents for Events {
    fn emit(&self, event: &str, payload: String) {
        self.0.lock().unwrap().push((event.into(), payload));
    }
}

#[derive(Default)]
struct Host {
    ops: Mutex<Vec<WindowOp>>,
    dialogs: Mutex<Vec<DialogRequest>>,
}

impl NativeHost for Host {
    fn window(&self, op: WindowOp) {
        self.ops.lock().unwrap().push(op);
    }
    fn dialog(&self, request: DialogRequest) -> Option<Vec<PathBuf>> {
        self.dialogs.lock().unwrap().push(request);
        Some(vec![PathBuf::from("/work/a.jl")])
    }
    fn open_external(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
}

fn call(dispatcher: &dyn NativeDispatcher, command: &str, args: &str) -> Result<String, String> {
    let (send, receive) = mpsc::channel();
    dispatcher.invoke(
        NativeCall {
            module: "app".into(),
            command: command.into(),
            args: args.into(),
        },
        Box::new(move |result| send.send(result).unwrap()),
    );
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("a reply")
}

#[test]
fn tauri_commands_run_as_a_native_module() {
    let dispatcher = dispatcher([Module::new("app").manage(Shared::default()).commands(
        squark_tauri::generate_handler![
            greet,
            remember,
            announce,
            open_folder,
            echo,
            quit,
            nested::status
        ],
    )]);
    let host = Arc::new(Host::default());
    dispatcher.start(host.clone(), &["app".into()]);
    let events = Arc::new(Events::default());
    dispatcher.attach(events.clone());

    assert_eq!(
        call(&*dispatcher, "greet", r#"{"fullName":"Ada"}"#),
        Ok(r#""Hello, Ada!""#.into())
    );
    assert_eq!(
        call(&*dispatcher, "remember", r#"{"item":"x"}"#),
        Ok("1".into())
    );
    assert_eq!(
        call(&*dispatcher, "announce", r#"{"sessionId":"s1"}"#),
        Ok("null".into())
    );
    assert_eq!(
        events.0.lock().unwrap().as_slice(),
        [(
            "announced".to_string(),
            r#"{"session_id":"s1"}"#.to_string()
        )]
    );
    assert_eq!(
        call(&*dispatcher, "open_folder", "{}"),
        Ok(r#""/work/a.jl""#.into())
    );
    // "All Files" (`*`) means no filter on Strange Quark.
    assert!(matches!(
        host.dialogs.lock().unwrap().as_slice(),
        [DialogRequest::PickFiles { filters, multiple: false, .. }] if filters.is_empty()
    ));
    assert_eq!(
        call(&*dispatcher, "echo", r#"{"raw_value":7}"#),
        Ok("7".into())
    );
    assert_eq!(call(&*dispatcher, "status", "{}"), Ok("null".into()));
    assert_eq!(call(&*dispatcher, "quit", "{}"), Ok("null".into()));
    assert_eq!(host.ops.lock().unwrap().as_slice(), [WindowOp::Close]);
}
