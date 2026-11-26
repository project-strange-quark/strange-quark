// SPDX-License-Identifier: MIT

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use squark_js::document::DocumentError;
use squark_types::{Rect, host::ClipboardItem, render::FrameId};
use winit::event::Ime;

use crate::{
    HostError, failure,
    services::{FileDialogKind, FileDialogQueue, RecordedFullscreen},
};

// Named pipes need Win32 calls that std does not wrap; the unsafe stays in
// this one module, as squark-mirror keeps it in its Stylo trait module.
#[cfg(windows)]
#[allow(unsafe_code)]
#[path = "control/pipe.rs"]
mod transport;
#[cfg(unix)]
#[path = "control/unix.rs"]
mod transport;

const MAX_LINE: u64 = 4 * 1024 * 1024;
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const IO_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "params")]
pub(crate) enum Request {
    #[serde(rename = "input.pointer")]
    Pointer(Pointer),
    #[serde(rename = "input.wheel")]
    Wheel(Wheel),
    #[serde(rename = "input.key")]
    Key(Key),
    #[serde(rename = "input.ime")]
    Ime(ImeInput),
    #[serde(rename = "frame.waitIdle")]
    WaitIdle {
        #[serde(default = "default_idle_frames")]
        frames: u32,
    },
    #[serde(rename = "frame.nextPresented")]
    NextPresented(NextPresented),
    #[serde(rename = "frame.screenshot")]
    Screenshot { path: Option<PathBuf> },
    #[serde(rename = "dom.eval")]
    Eval { script: String },
    #[serde(rename = "dom.queryRect")]
    QueryRect { selector: String },
    #[serde(rename = "clipboard.write")]
    ClipboardWrite { items: ClipboardInput },
    #[serde(rename = "clipboard.read")]
    ClipboardRead {},
    #[serde(rename = "fileDialog.queue")]
    FileDialogQueue { kind: FileDialogKind, path: PathBuf },
    /// The recorded fullscreen state of a test-control run.
    #[serde(rename = "window.fullscreen")]
    Fullscreen {},
    /// Leaves fullscreen as the window system would (a compositor key).
    #[serde(rename = "window.leaveFullscreen")]
    LeaveFullscreen {},
    #[serde(rename = "app.quit")]
    Quit {},
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ClipboardInput {
    Single(ClipboardInputItem),
    Multiple(Vec<ClipboardInputItem>),
}

impl ClipboardInput {
    pub(crate) fn into_items(self) -> Vec<ClipboardItem> {
        let inputs = match self {
            Self::Single(item) => vec![item],
            Self::Multiple(items) => items,
        };
        inputs
            .into_iter()
            .map(|item| ClipboardItem {
                mime: item.mime,
                bytes: item.bytes,
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipboardInputItem {
    mime: String,
    #[serde(rename = "base64", deserialize_with = "deserialize_base64")]
    bytes: Arc<[u8]>,
}

fn deserialize_base64<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let encoded = String::deserialize(deserializer)?;
    STANDARD
        .decode(encoded)
        .map(Arc::from)
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NextPresented {}

fn default_idle_frames() -> u32 {
    2
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PointerKind {
    Down,
    Move,
    Up,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pointer {
    pub kind: PointerKind,
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub buttons: u16,
    pub modifiers: u8,
    pub pointer_id: i32,
    pub click_count: Option<u32>,
}

impl Pointer {
    fn validate(&self) -> Result<(), &'static str> {
        if let Some(click_count) = self.click_count {
            if click_count == 0 {
                return Err("clickCount must be at least 1");
            }
            if matches!(&self.kind, PointerKind::Move) {
                return Err("clickCount is only valid on down and up");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Wheel {
    pub x: f32,
    pub y: f32,
    pub dx: f64,
    pub dy: f64,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum KeyKind {
    Down,
    Up,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Key {
    pub kind: KeyKind,
    pub key: String,
    pub code: String,
    #[serde(rename = "text")]
    pub _text: Option<String>,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImeKind {
    Preedit,
    Commit,
    End,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImeInput {
    pub kind: ImeKind,
    pub text: String,
    pub cursor: Option<[usize; 2]>,
}

impl ImeInput {
    fn validate(&self) -> Result<(), &'static str> {
        if self.cursor.is_some_and(|[start, end]| {
            self.text.get(..start).is_none() || self.text.get(..end).is_none()
        }) {
            return Err("cursor offsets must be UTF-8 character boundaries within text");
        }
        Ok(())
    }

    pub(crate) fn into_event(self) -> Ime {
        match self.kind {
            ImeKind::Preedit => {
                Ime::Preedit(self.text, self.cursor.map(|[start, end]| (start, end)))
            }
            ImeKind::Commit => Ime::Commit(self.text),
            ImeKind::End => Ime::Disabled,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Call {
    pub request: Request,
    pub received_at: Instant,
    pub reply: mpsc::Sender<Reply>,
}

#[derive(Debug)]
pub(crate) enum Reply {
    Json(Result<Value, HostError>),
    Eval(mpsc::Receiver<Result<String, DocumentError>>),
    Rect(mpsc::Receiver<Result<Option<Rect>, DocumentError>>),
    Screenshot {
        size: [u32; 2],
        rgba: Vec<u8>,
        path: Option<PathBuf>,
    },
}

impl Reply {
    fn result(self) -> Result<Value, HostError> {
        match self {
            Self::Json(result) => result,
            Self::Eval(receiver) => {
                let value = receive(receiver)?.map_err(|error| failure("DocumentError", error))?;
                let value: Value = serde_json::from_str(&value)
                    .map_err(|error| failure("DocumentError", error))?;
                Ok(json!({ "value": value }))
            }
            Self::Rect(receiver) => {
                let rect = receive(receiver)?.map_err(|error| failure("DocumentError", error))?;
                let rect = rect.map(|rect| {
                    json!({
                        "x": rect.origin.x, "y": rect.origin.y,
                        "width": rect.size.width, "height": rect.size.height,
                    })
                });
                Ok(json!({ "rect": rect }))
            }
            Self::Screenshot {
                size: [width, height],
                rgba,
                path,
            } => {
                let png = crate::encode_png([width, height], &rgba)?;
                if let Some(path) = path {
                    std::fs::write(&path, png)
                        .map_err(|error| failure("ScreenshotError", error))?;
                    Ok(json!({ "width": width, "height": height, "path": path }))
                } else {
                    Ok(
                        json!({ "width": width, "height": height, "pngBase64": STANDARD.encode(png) }),
                    )
                }
            }
        }
    }
}

pub(crate) fn receive<T>(receiver: mpsc::Receiver<T>) -> Result<T, HostError> {
    receiver
        .recv_timeout(REQUEST_TIMEOUT)
        .map_err(|error| failure("ControlError", error))
}

pub(crate) fn send(reply: mpsc::Sender<Reply>, result: Result<Value, HostError>) {
    let _ = reply.send(Reply::Json(result));
}

pub(crate) fn clipboard_write(
    items: ClipboardInput,
    clipboard: &crate::services::MemoryClipboard,
) -> Value {
    clipboard.replace(items.into_items());
    json!({ "ok": true })
}

pub(crate) fn clipboard_read(clipboard: &crate::services::MemoryClipboard) -> Value {
    let items = clipboard
        .items()
        .into_iter()
        .map(|item| json!({ "mime": item.mime, "base64": STANDARD.encode(item.bytes.as_ref()) }))
        .collect::<Vec<_>>();
    json!({ "items": items })
}

pub(crate) fn file_dialog_queue(
    kind: FileDialogKind,
    path: PathBuf,
    queue: &FileDialogQueue,
) -> Value {
    queue.push(kind, path);
    json!({ "ok": true })
}

pub(crate) fn fullscreen(state: &RecordedFullscreen) -> Value {
    json!({ "fullscreen": state.get() })
}

/// Test control stands in for the window system: leaving a recorded
/// fullscreen tells the document, as a real window's exit does.
pub(crate) fn leave_fullscreen(
    state: &RecordedFullscreen,
    tasks: &mpsc::Sender<squark_js::document::DocumentTask>,
) -> Result<Value, HostError> {
    let changed = state.set(false);
    if changed {
        tasks
            .send(squark_js::document::DocumentTask::FullscreenChanged { fullscreen: false })
            .map_err(|_| failure("DocumentError", "document thread closed"))?;
    }
    Ok(json!({ "changed": changed }))
}

pub(crate) fn presented(frame: FrameId, at: Instant) -> Value {
    json!({
        "frameId": frame.0,
        "presentedAtMs": at.saturating_duration_since(crate::process_start()).as_secs_f64() * 1000.0,
    })
}

pub(crate) fn dom_task(
    request: Request,
    tasks: &mpsc::Sender<squark_js::document::DocumentTask>,
    response: mpsc::Sender<Reply>,
) {
    use squark_js::document::DocumentTask;

    let (task, reply) = match request {
        Request::Eval { script } => {
            let (reply, receiver) = mpsc::channel();
            (DocumentTask::Eval { script, reply }, Reply::Eval(receiver))
        }
        Request::QueryRect { selector } => {
            let (reply, receiver) = mpsc::channel();
            (
                DocumentTask::QueryRect { selector, reply },
                Reply::Rect(receiver),
            )
        }
        _ => unreachable!("expected a DOM request"),
    };
    if tasks.send(task).is_ok() {
        let _ = response.send(reply);
    } else {
        send(
            response,
            Err(failure("DocumentError", "document thread closed")),
        );
    }
}

pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(path: &Path, submit: impl Fn(Call) -> bool + Send + 'static) -> io::Result<Self> {
        let listener = transport::Listener::bind(path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("squark-test-control".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok(Some(stream)) => {
                            let _ = serve(stream, &stopping, &submit);
                        }
                        Ok(None) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::park_timeout(IO_POLL)
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// Serves one client. Reads on `stream` give up after about [`IO_POLL`]
/// with `WouldBlock` or `TimedOut`, so a stop request is noticed.
fn serve(
    stream: impl Read + Write,
    stop: &AtomicBool,
    submit: &impl Fn(Call) -> bool,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let available = MAX_LINE.saturating_sub(line.len() as u64) + 1;
        match (&mut reader).take(available).read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) if line.len() as u64 > MAX_LINE => break,
            Ok(_) if line.last() != Some(&b'\n') => break,
            Ok(_) => {
                let response = handle_line(&line, submit);
                serde_json::to_writer(reader.get_mut(), &response).map_err(io::Error::other)?;
                reader.get_mut().write_all(b"\n")?;
                line.clear();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn handle_line(bytes: &[u8], submit: &impl Fn(Call) -> bool) -> Value {
    let received_at = Instant::now();
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return rpc_error(Value::Null, -32700, "Parse error");
    };
    let id = value
        .get("id")
        .filter(|id| id.is_number() || id.is_string() || id.is_null());
    let method = value.get("method").and_then(Value::as_str);
    let (Some(id), Some(method), Some("2.0")) =
        (id, method, value.get("jsonrpc").and_then(Value::as_str))
    else {
        return rpc_error(Value::Null, -32600, "Invalid request");
    };
    if !matches!(
        method,
        "input.pointer"
            | "input.wheel"
            | "input.key"
            | "input.ime"
            | "frame.waitIdle"
            | "frame.nextPresented"
            | "frame.screenshot"
            | "dom.eval"
            | "dom.queryRect"
            | "clipboard.write"
            | "clipboard.read"
            | "fileDialog.queue"
            | "window.fullscreen"
            | "window.leaveFullscreen"
            | "app.quit"
    ) {
        return rpc_error(id.clone(), -32601, "Method not found");
    }
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    let request = match serde_json::from_value(json!({ "method": method, "params": params })) {
        Ok(request) => request,
        Err(error) => return rpc_error(id.clone(), -32602, error.to_string()),
    };
    if let Request::Pointer(pointer) = &request
        && let Err(error) = pointer.validate()
    {
        return rpc_error(id.clone(), -32602, error);
    }
    if let Request::Ime(input) = &request
        && let Err(error) = input.validate()
    {
        return rpc_error(id.clone(), -32602, error);
    }
    let (reply, result) = mpsc::channel();
    if !submit(Call {
        request,
        received_at,
        reply,
    }) {
        return rpc_error(id.clone(), -32000, "Runtime closed");
    }
    match receive(result).and_then(Reply::result) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => rpc_error(id.clone(), -32000, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_parses_optional_click_count_for_down_and_up() {
        let parse = |kind, click_count| {
            let mut params = json!({
                "kind": kind,
                "x": 1,
                "y": 2,
                "button": 0,
                "buttons": 1,
                "modifiers": 0,
                "pointerId": 1,
            });
            if let Some(click_count) = click_count {
                params["clickCount"] = json!(click_count);
            }
            serde_json::from_value::<Request>(json!({
                "method": "input.pointer",
                "params": params,
            }))
            .unwrap()
        };

        for kind in ["down", "up"] {
            let Request::Pointer(pointer) = parse(kind, Some(2)) else {
                panic!("expected a pointer request");
            };
            assert_eq!(pointer.click_count, Some(2));
            assert!(pointer.validate().is_ok());
        }

        let Request::Pointer(pointer) = parse("down", None) else {
            panic!("expected a pointer request");
        };
        assert_eq!(pointer.click_count, None);
        assert!(pointer.validate().is_ok());
    }

    #[test]
    fn pointer_rejects_zero_click_count_and_click_count_on_move() {
        for input in [
            br#"{"jsonrpc":"2.0","id":1,"method":"input.pointer","params":{"kind":"down","x":0,"y":0,"button":0,"buttons":1,"modifiers":0,"pointerId":1,"clickCount":0}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":2,"method":"input.pointer","params":{"kind":"move","x":0,"y":0,"button":0,"buttons":0,"modifiers":0,"pointerId":1,"clickCount":1}}"#.as_slice(),
        ] {
            let response = handle_line(input, &|_| panic!("invalid pointer params must not dispatch"));
            assert_eq!(response["error"]["code"], -32602);
        }
    }

    #[test]
    fn ime_rejects_bad_kinds_and_cursor_offsets_as_invalid_params() {
        for params in [
            json!({ "kind": "cancel", "text": "" }),
            json!({ "kind": "preedit", "text": "に", "cursor": [1, 1] }),
            json!({ "kind": "preedit", "text": "に", "cursor": [0, 4] }),
        ] {
            let request = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "input.ime",
                "params": params,
            }))
            .unwrap();
            let response = handle_line(&request, &|_| {
                panic!("invalid IME params must not dispatch")
            });
            assert_eq!(response["error"]["code"], -32602);
        }
    }

    #[test]
    fn ime_accepts_a_cursor_at_a_utf8_character_boundary() {
        let Request::Ime(input) = serde_json::from_value(json!({
            "method": "input.ime",
            "params": { "kind": "preedit", "text": "日本", "cursor": [6, 6] },
        }))
        .unwrap() else {
            panic!("expected an IME request");
        };
        assert!(input.validate().is_ok());
        assert!(matches!(input.into_event(), Ime::Preedit(_, Some((6, 6)))));
    }

    #[test]
    fn rpc_validates_envelopes_and_params_and_keeps_request_ids() {
        let handle = |input: &[u8]| {
            handle_line(input, &|call| {
                assert!(matches!(call.request, Request::WaitIdle { frames: 2 }));
                send(call.reply, Ok(json!({ "frameId": 17 })));
                true
            })
        };
        assert_eq!(handle(b"{")["error"]["code"], -32700);
        assert_eq!(
            handle(br#"{"method":"frame.waitIdle"}"#)["error"]["code"],
            -32600
        );
        assert_eq!(
            handle(br#"{"jsonrpc":"2.0","id":4,"method":"missing"}"#)["error"]["code"],
            -32601
        );
        assert_eq!(
            handle(br#"{"jsonrpc":"2.0","id":5,"method":"frame.waitIdle","params":{"frames":-1}}"#)
                ["error"]["code"],
            -32602
        );
        let response = handle(br#"{"jsonrpc":"2.0","id":"idle","method":"frame.waitIdle"}"#);
        assert_eq!(
            response,
            json!({ "jsonrpc": "2.0", "id": "idle", "result": { "frameId": 17 } })
        );
    }

    #[test]
    fn next_presented_requires_empty_params_and_captures_receipt_time() {
        let start = Instant::now();
        let response = handle_line(
            br#"{"jsonrpc":"2.0","id":2,"method":"frame.nextPresented","params":{"frames":2}}"#,
            &|_| panic!("invalid params must never reach render"),
        );
        assert_eq!(response["error"]["code"], -32602);
        let response = handle_line(
            br#"{"jsonrpc":"2.0","id":3,"method":"frame.nextPresented","params":{}}"#,
            &|call| {
                assert!(matches!(call.request, Request::NextPresented(_)));
                assert!(call.received_at >= start);
                send(call.reply, Ok(presented(FrameId(12), Instant::now())));
                true
            },
        );
        assert_eq!(response["result"]["frameId"], 12);
        assert!(response["result"]["presentedAtMs"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn query_rect_uses_the_document_task_reply() {
        let (tasks, reader) = mpsc::channel();
        let (reply, response) = mpsc::channel();
        dom_task(
            Request::QueryRect {
                selector: "#marker".into(),
            },
            &tasks,
            reply,
        );
        let squark_js::document::DocumentTask::QueryRect { selector, reply } =
            reader.recv().unwrap()
        else {
            panic!("expected query rectangle task");
        };
        assert_eq!(selector, "#marker");
        reply.send(Ok(Some(Rect::new(2.0, 3.0, 4.0, 5.0)))).unwrap();
        assert_eq!(
            receive(response).unwrap().result().unwrap(),
            json!({
                "rect": { "x": 2.0, "y": 3.0, "width": 4.0, "height": 5.0 }
            })
        );
    }
}
