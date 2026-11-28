// SPDX-License-Identifier: MIT
//! The fixture app's native module: echo, errors, state, a burst of events
//! from a worker thread, a folder dialog and close interception.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use serde::Serialize;
use squark_native::{App, CloseRequest, DialogRequest, Module, State, export, handlers};

#[derive(Default)]
struct Counter(AtomicU32);

#[derive(Default)]
struct CloseGuard(AtomicBool);

#[derive(Serialize)]
struct Tick {
    session_id: String,
    index: u32,
}

#[export]
fn echo(value: serde_json::Value, label: Option<String>) -> serde_json::Value {
    serde_json::json!({ "label": label, "value": value })
}

#[export]
fn fail(message: String) -> Result<(), String> {
    Err(message)
}

#[export]
fn count(counter: State<Counter>) -> u32 {
    counter.0.fetch_add(1, Ordering::SeqCst) + 1
}

#[export]
fn burst(app: App, session_id: String, count: u32) {
    std::thread::spawn(move || {
        for index in 0..count {
            let _ = app.emit("tick", Tick { session_id: session_id.clone(), index });
        }
        let _ = app.emit("burst-done", count);
    });
}

#[export]
async fn delayed(millis: u64) -> u64 {
    // Timers work: async commands run on the shared runtime.
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
    millis
}

#[export]
fn pick_folder(app: App) -> Option<PathBuf> {
    app.dialog(DialogRequest::PickFolder { title: None, directory: None })?
        .into_iter()
        .next()
}

#[export]
fn guard_close(guard: State<CloseGuard>, enabled: bool) {
    guard.0.store(enabled, Ordering::SeqCst);
}

#[squark_tauri::command]
fn greet(greeting_target: String) -> String {
    format!("Tauri says {greeting_target}")
}

pub fn modules() -> Vec<Module> {
    vec![
        Module::new("fixture")
            .manage(Counter::default())
            .manage(CloseGuard::default())
            .commands(handlers![echo, fail, count, burst, delayed, pick_folder, guard_close])
            .on_close_requested(|app| {
                if app.state::<CloseGuard>().is_some_and(|guard| guard.0.load(Ordering::SeqCst)) {
                    let _ = app.emit("close-requested", ());
                    CloseRequest::Prevent
                } else {
                    CloseRequest::Allow
                }
            }),
        Module::new("unlisted").commands(handlers![echo]),
        Module::new("tauri-fixture").commands(squark_tauri::generate_handler![greet]),
    ]
}
