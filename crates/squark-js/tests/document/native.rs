// SPDX-License-Identifier: MIT

use std::sync::{Arc, Mutex, mpsc};

use squark_types::{
    host::{HostRequest, HostValue},
    native::{
        CloseRequest, NativeCall, NativeDispatcher, NativeEvents, NativeHost,
        NativeReply, WindowOp,
    },
};

use super::*;

/// Records calls; the test replies and emits by hand, as module threads would.
#[derive(Default)]
struct FakeDispatcher {
    calls: Mutex<Vec<(NativeCall, NativeReply)>>,
    events: Mutex<Option<Arc<dyn NativeEvents>>>,
}

impl NativeDispatcher for FakeDispatcher {
    fn modules(&self) -> Vec<String> {
        vec!["demo".into()]
    }
    fn start(&self, _host: Arc<dyn NativeHost>, _enabled: &[String]) {}
    fn attach(&self, events: Arc<dyn NativeEvents>) {
        *self.events.lock().unwrap() = Some(events);
    }
    fn invoke(&self, call: NativeCall, reply: NativeReply) {
        self.calls.lock().unwrap().push((call, reply));
    }
    fn close_requested(&self) -> CloseRequest {
        CloseRequest::Allow
    }
}

struct Harness {
    runner: DocumentRunner<'static>,
    host: mpsc::Receiver<HostRequest>,
    wake: mpsc::Receiver<DocumentTask>,
    native: Arc<FakeDispatcher>,
}

impl Harness {
    fn new(native: Option<Arc<FakeDispatcher>>) -> Self {
        support::initialize_v8();
        let (sender, host) = mpsc::channel();
        let now = Instant::now();
        let trace = Trace::default();
        let engine = Engine::new(trace.clone());
        let mut sources = sources(|_| None).with_host_requests(sender);
        let dispatcher = native.clone().unwrap_or_default();
        if native.is_some() {
            sources = sources.with_native(dispatcher.clone());
        }
        let mut runner = DocumentRunner::new(
            options(now, true),
            &fixture(&[]),
            sources,
            Box::new(engine),
            Box::new(Paint(trace)),
            Instant::now,
        )
        .unwrap();
        let (wake_sender, wake) = mpsc::channel();
        runner.set_network_wakeup(wake_sender);
        Self {
            runner,
            host,
            wake,
            native: dispatcher,
        }
    }

    fn eval(&mut self, script: &str) -> String {
        self.runner.eval(script).unwrap()
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.runner.run_ready().unwrap();
        }
    }
}

const API: &str = "globalThis[Symbol.for('strange-quark.native')]";

#[test]
fn invoke_round_trips_json_and_events_reach_listeners() {
    let mut h = Harness::new(Some(Arc::default()));
    assert_eq!(h.eval(&format!("JSON.stringify({API}.modules)")), r#""[\"demo\"]""#);
    // Not a web API: no global name, only the registered symbol.
    assert_eq!(
        h.eval("Object.getOwnPropertyNames(globalThis).some(name => /native/i.test(name))"),
        "false"
    );
    h.eval(&format!(
        r#"
        globalThis.log = [];
        {API}.invoke("demo", "fs_read_file", {{ path: "/a.jl", sessionId: 1 }})
          .then(value => log.push(["ok", value]), error => log.push(["err", error]));
        {API}.invoke("demo", "fail", {{}})
          .then(value => log.push(["ok", value]), error => log.push(["err", error]));
        {API}.invoke("other", "x", {{}}).catch(error => log.push(["missing", error]));
        globalThis.off = {API}.listen("pty-output", payload => log.push(["event", payload]));
        "#
    ));
    let calls: Vec<_> = h.native.calls.lock().unwrap().drain(..).collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0.module, "demo");
    assert_eq!(calls[0].0.command, "fs_read_file");
    assert_eq!(calls[0].0.args, r#"{"path":"/a.jl","sessionId":1}"#);
    let mut replies = calls.into_iter().map(|(_, reply)| reply);
    // Replies come from module threads; they wake the document thread.
    let first = replies.next().unwrap();
    std::thread::spawn(move || first(Ok(r#""module Demo""#.into()))).join().unwrap();
    replies.next().unwrap()(Err(r#""No such file""#.into()));
    let events = h.native.events.lock().unwrap().clone().unwrap();
    events.emit("pty-output", r#"{"session_id":"s","data":"julia> "}"#.into());
    events.emit("unheard", "1".into());
    assert!(matches!(h.wake.try_recv(), Ok(DocumentTask::NativeReady)));
    h.settle();
    assert_eq!(
        h.eval("JSON.stringify(log)"),
        r#""[[\"missing\",\"native module \\\"other\\\" is not available\"],[\"ok\",\"module Demo\"],[\"err\",\"No such file\"],[\"event\",{\"session_id\":\"s\",\"data\":\"julia> \"}]]""#
    );
    h.eval("off()");
    events.emit("pty-output", "2".into());
    h.settle();
    assert_eq!(h.eval("log.length"), "4");
}

#[test]
fn window_operations_go_to_the_host_and_the_api_exists_without_modules() {
    let mut h = Harness::new(None);
    assert_eq!(h.eval(&format!("JSON.stringify({API}.modules)")), r#""[]""#);
    h.eval(&format!(
        r#"
        globalThis.rejected = null;
        {API}.invoke("demo", "x", {{}}).catch(error => rejected = error);
        {API}.window.setZoom(1.25);
        {API}.window.setTitle("julIDE — demo");
        "#
    ));
    assert_eq!(
        h.host.try_recv().unwrap(),
        HostRequest::Window {
            id: 1,
            op: WindowOp::SetZoom(1.25)
        }
    );
    assert_eq!(
        h.host.try_recv().unwrap(),
        HostRequest::Window {
            id: 2,
            op: WindowOp::SetTitle("julIDE — demo".into())
        }
    );
    h.runner.host_reply(1, Ok(HostValue::Done)).unwrap();
    h.settle();
    assert_eq!(h.eval("rejected"), r#""native module \"demo\" is not available""#);
}
