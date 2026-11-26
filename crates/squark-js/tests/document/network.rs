// SPDX-License-Identifier: MIT

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
};

use super::*;

fn runner(
    source: ScriptSources,
    url: &str,
    scripts: &[(ScriptKind, &str, &str)],
) -> DocumentRunner<'static> {
    support::initialize_v8();
    let origin = Instant::now();
    let mut options = options(origin, true);
    options.url = url.to_owned();
    let trace = Trace::default();
    DocumentRunner::new(
        options,
        &fixture(scripts),
        source,
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap()
}

fn await_result(runner: &mut DocumentRunner<'_>, expression: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        runner.run_ready().unwrap();
        let result = runner.eval(expression).unwrap();
        if result != "null" && result != "undefined" {
            return result;
        }
        assert!(Instant::now() < deadline, "timed out awaiting {expression}");
        thread::sleep(Duration::from_millis(2));
    }
}

struct Vite {
    child: Child,
    port: u16,
    deadline: Option<crate::timeout::Deadline>,
}

impl Vite {
    fn start() -> Self {
        let mut child = Command::new("node")
            .arg(key("crates/squark-js/tests/fixtures/network-server.mjs"))
            .current_dir(key(""))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start Vite test fixture");
        let deadline = Some(crate::timeout::Deadline::start(
            &child,
            Duration::from_secs(40),
        ));
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .trim()
            .parse()
            .expect("Vite fixture must print its port");
        Self {
            child,
            port,
            deadline,
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for Vite {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.wait();
        // Cancel the deadline only once the fixture has been reaped.
        self.deadline.take();
    }
}

#[test]
fn packed_assets_and_network_failures_reach_javascript() {
    let pack =
        Arc::new(Pack::open(&File::open(key("tests/fixtures/pack/basic.sqpak")).unwrap()).unwrap());
    let font = pack.entry("/fonts/sample.woff2").unwrap();
    let expected = font.bytes.len();
    let mime = font.mime.to_owned();
    let mut runner = runner(
        sources(move |path| {
            let entry = pack.entry(path)?;
            Some((entry.mime.to_owned(), entry.bytes.to_vec()))
        })
        .with_storage_root(key("target/step3-unused-storage")),
        URL,
        &[],
    );
    runner.eval(r#"
        globalThis.result = null;
        (async () => {
          const url = new URL("/fonts/sample.woff2", location.origin);
          const response = await fetch(url);
          const clone = response.clone();
          const bytes = await response.arrayBuffer();
          const file = await clone.blob();
          const size = file.size;
          if (!(file instanceof Blob) || file.type !== response.headers.get("Content-Type")) throw new Error("Invalid font Blob");
          const head = await fetch(url, { method: "HEAD" });
          const module = await fetch("/assets/main.js");
          const missing = await fetch("/assets/missing.js");
          const blocked = await fetch("https://cdn.example.invalid/font.woff2").catch(error => [error.name, error instanceof TypeError]);

          const loading = document.createElement("script");
          const script = Promise.withResolvers();
          loading.onload = () => script.resolve("loaded");
          loading.onerror = event => script.resolve([event.type, event.target === loading, event.isTrusted]);
          loading.src = "https://scripts.simpleanalyticscdn.com/latest.js";
          document.head.appendChild(loading);

          const socket = new WebSocket("wss://events.example.invalid");
          const events = [];
          socket.onerror = e => events.push([e.type, socket.readyState]);
          const closed = new Promise(resolve => {
            socket.onclose = e => resolve([e.code, e.wasClean, socket.readyState]);
          });
          const source = new EventSource("https://events.example.invalid/stream");
          const failed = new Promise(resolve => {
            source.onerror = e => resolve([e.type, source.readyState]);
          });

          const [scriptResult, closeResult, sourceResult] = await Promise.all([script.promise, closed, failed]);
          globalThis.result = [bytes.byteLength, size, response.headers.get("Content-Type"), response.bodyUsed,
            head.body, head.headers.get("content-length"), module.ok, missing.status,
            blocked, scriptResult, events, closeResult, sourceResult];
        })().catch(error => { globalThis.result = ["error", String(error)]; });
    "#).unwrap();
    assert_eq!(
        await_result(&mut runner, "result"),
        format!(
            r#"[{expected},{expected},"{mime}",true,null,"{expected}",true,404,["TypeError",true],["error",true,true],[["error",3]],[1006,false,3],["error",2]]"#
        )
    );
}

#[test]
fn allowed_http_aborts_and_event_stream_resume() {
    let vite = Vite::start();
    let origin = vite.origin();
    let manifest =
        format!(r#"{{"app_name":"network-test","network":{{"allow":["{origin}"]}}}}"#).into_bytes();
    let mut runner = runner(
        sources(move |path| {
            (path == squark_types::pack::MANIFEST_PATH)
                .then(|| ("application/json".to_owned(), manifest.clone()))
        })
        .with_storage_root(key("target/step3-unused-storage")),
        URL,
        &[
            (
                ScriptKind::InlineClassic,
                r#"
                globalThis.startupEvents = [];
                const scripts = document.getElementsByTagName("script");
                scripts[1].onerror = e => startupEvents.push(["blocked", e.isTrusted]);
                scripts[2].onload = () => startupEvents.push("loaded");
            "#,
                "/index.html",
            ),
            (
                ScriptKind::ExternalClassic,
                "https://scripts.simpleanalyticscdn.com/latest.js",
                "/index.html",
            ),
            (
                ScriptKind::ExternalClassic,
                &format!("{origin}/classic"),
                "/index.html",
            ),
        ],
    );
    let script = r#"
        globalThis.result = null;
        (async () => {
          const headers = new Headers([["X-Mixed", "  one  "], ["x-mixed", "two"]]);
          const request = new Request(ORIGIN + "/echo", { method: "post", headers, body: "café" });
          const initial = await request.clone().text();
          const res = await fetch(request);
          const echo = await res.json();
          const duplicate = res.headers.get("X-Duplicate");
          const redirected = await fetch(ORIGIN + "/redirect-local", {method:"POST", body:"drop"});
          const redirectInfo = [redirected.redirected, (await redirected.json()).method];
          const manual = await fetch(ORIGIN + "/redirect-local", {redirect:"manual"});
          const redirect = await fetch(ORIGIN + "/redirect-blocked").catch(error => error.name);
          const tries = await (await fetch(ORIGIN + "/attempts")).text();

          const aborted = new AbortController();
          const liveResponse = await fetch(ORIGIN + "/abort-stream", {signal:aborted.signal});
          const reader = liveResponse.body.getReader();
          const first = (await reader.read()).value.length;
          const next = reader.read();
          aborted.abort();
          const interrupted = await next.catch(error => error.name);
          const before = await fetch(ORIGIN + "/forbidden", {signal:AbortSignal.abort("stop")}).catch(error => error);
          const source = new EventSource(ORIGIN + "/events");
          const resumed = Promise.withResolvers();
          const events = [];
          source.onopen = () => events.push(["open", source.readyState]);
          source.onerror = () => events.push(["error", source.readyState]);
          source.addEventListener("ping", e => events.push([e.data, e.lastEventId]));
          source.onmessage = e => {
            events.push([e.data, e.lastEventId]);
            source.close();
            resumed.resolve();
          };
          await resumed.promise;
          globalThis.result = [[initial, headers.get("X-MIXED"), request.bodyUsed], echo, duplicate,
            redirectInfo, [manual.type, manual.status, manual.body], redirect, tries, first, interrupted, before, events];
        })().catch(error => { globalThis.result = ["error", String(error)]; });
    "#.replace("ORIGIN", &format!("{origin:?}"));
    runner.eval(&script).unwrap();
    assert_eq!(
        await_result(&mut runner, "result"),
        r#"[["café","one, two",true],{"method":"POST","body":"café"},"first, second",[true,"GET"],["opaqueredirect",0,null],"TypeError","0",4,"AbortError","stop",[["open",1],["café\nsecond","7"],["error",0],["open",1],["7","7"]]]"#
    );
    assert_eq!(
        runner.eval("[startupEvents, document.readyState]").unwrap(),
        r#"[[["blocked",true],"ran","loaded"],"complete"]"#
    );
}

#[test]
fn dev_module_graph_loads_from_vite_and_applies_hmr() {
    let vite = Vite::start();
    let origin = vite.origin();
    let base = format!("{origin}/index.html");
    let mut runner = runner(
        sources(|path| {
            (path == "/dev-packed.js").then(|| {
                (
                    "text/javascript".into(),
                    b"export const packed = 43;".to_vec(),
                )
            })
        })
        .with_dev_server()
        .with_storage_root(key("target/step3-unused-storage")),
        &base,
        &[
            (
                ScriptKind::InlineClassic,
                "addEventListener('error', e => { globalThis.hmrError = [e.message, e.error?.stack]; });",
                "/index.html",
            ),
            (ScriptKind::ExternalModule, "/dev-entry.js", "/dev-entry.js"),
            (
                ScriptKind::InlineModule,
                r#"
                    import { createHotContext } from "/@vite/client";
                    const hot = createHotContext("/test.js");
                    globalThis.sendUpdate = () => hot.send("squark:update");
                "#,
                "/index.html",
            ),
        ],
    );
    assert_eq!(
        await_result(&mut runner, "globalThis.hmrError ?? globalThis.hmrResult"),
        "42"
    );
    assert_eq!(
        runner
            .eval("[devEntry, devSharedEvaluations, document.readyState]")
            .unwrap(),
        r#"[[41,42,43],1,"complete"]"#
    );
    runner.eval("sendUpdate()").unwrap();
    assert_eq!(
        await_result(&mut runner, "globalThis.update"),
        format!(r#"[2,"{origin}/dev-entry.js?t=2"]"#)
    );
    let script = r#"
        (async () => {
            const first = await import("/dev-entry.js?t=1");
            const second = await import("/dev-entry.js?t=2");
            const sameSecond = await import("/dev-entry.js?t=2");
            const badMime = await import("/invalid-module.js").catch(error => error.name);
            const blocked = await import(BLOCKED).catch(error => error.name);
            const attempts = await (await fetch("/attempts")).text();
            globalThis.imports = [first.version, second.version, second === sameSecond,
                first === second, badMime, blocked, attempts];
        })().catch(error => { globalThis.imports = [String(error)]; });
    "#
    .replace(
        "BLOCKED",
        &format!(r#""http://localhost:{}/forbidden""#, vite.port),
    );
    runner.eval(&script).unwrap();
    assert_eq!(
        await_result(&mut runner, "globalThis.imports"),
        r#"[1,2,true,false,"TypeError","TypeError","0"]"#
    );
}

#[test]
fn storage_is_atomic_persistent_and_namespaced_by_app_name() {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let path = key(&format!(
        "target/dom-storage-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    let make_runner = |app: &str| {
        let manifest = serde_json::json!({"app_name":app}).to_string().into_bytes();
        runner(
            sources(move |name| {
                (name == squark_types::pack::MANIFEST_PATH)
                    .then(|| ("application/json".to_owned(), manifest.clone()))
            })
            .with_storage_root(path.clone()),
            URL,
            &[],
        )
    };
    {
        let mut first = make_runner("todo/../lists");
        assert_eq!(
            first
                .eval(
                    r#"
            localStorage.clear();
            localStorage.setItem("☃\ud800", "\udfff");
            localStorage.app = "todos";
            [localStorage.length, localStorage.key(1), Object.keys(localStorage).length]
        "#
                )
                .unwrap(),
            r#"[2,"app",2]"#
        );
    }
    {
        let mut second = make_runner("todo/../lists");
        assert_eq!(
            second
                .eval(
                    r#"
            localStorage.fresh = 12;
            [localStorage.getItem("☃\ud800").charCodeAt(0), localStorage.app]
        "#
                )
                .unwrap(),
            r#"[57343,"todos"]"#
        );
    }
    {
        let mut first = make_runner("todo/../lists");
        assert_eq!(
            first
                .eval(
                    r#"
            localStorage.removeItem("☃\ud800");
            [localStorage.length, localStorage.fresh, 'app' in localStorage]
        "#
                )
                .unwrap(),
            r#"[2,"12",true]"#
        );
    }
    assert_eq!(
        make_runner("lists").eval("localStorage.length").unwrap(),
        "0"
    );
    let mut reopened = make_runner("todo/../lists");
    assert_eq!(
        reopened
            .eval(
                r#"
        delete localStorage.app;
        [localStorage.length, localStorage.key(0), localStorage.getItem("app")]
    "#
            )
            .unwrap(),
        r#"[1,"fresh",null]"#
    );
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn concurrent_bodies_finish_through_an_idle_document_queue() {
    let vite = Vite::start();
    let origin = vite.origin();
    let mut runner = runner(
        sources(|_| None).with_dev_server(),
        &format!("{origin}/index.html"),
        &[],
    );
    runner
        .eval(
            r#"
        globalThis.allBodies = null;
        void (async () => {
          let total = 0;
          for (let wave = 0; wave < 8; wave++) {
            const texts = await Promise.all(Array.from({length: 25}, (_, i) =>
              fetch("/parallel?request=" + (25 * wave + i)).then(response => response.text())));
            if (texts.some((text, i) => text !== String(25 * wave + i) + ":complete")) {
              throw new Error("Mismatching response bodies");
            }
            total += texts.length;
          }
          globalThis.allBodies = total;
        })().catch(error => { globalThis.allBodies = String(error); });
    "#,
        )
        .unwrap();
    let (queue, messages) = std::sync::mpsc::channel();
    runner.set_network_wakeup(queue.clone());
    let driver = thread::spawn(move || {
        thread::sleep(Duration::from_millis(800));
        let (reply, result) = std::sync::mpsc::channel();
        queue
            .send(DocumentTask::Eval {
                script: "allBodies".into(),
                reply,
            })
            .unwrap();
        let value = result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        queue.send(DocumentTask::Shutdown).unwrap();
        value
    });
    runner
        .run(messages, |_| panic!("no frames are needed for network IO"))
        .unwrap();
    assert_eq!(driver.join().unwrap(), "200");
}
