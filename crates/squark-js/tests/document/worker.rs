// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

use super::*;

/// Waits until `ticks` stops changing, and returns its final value.
fn quiescent(ticks: &AtomicUsize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = ticks.load(Ordering::Relaxed);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = ticks.load(Ordering::Relaxed);
        if now == last {
            return now;
        }
        assert!(Instant::now() < deadline, "the worker kept running");
        last = now;
    }
}

/// A document whose pack holds `files` as `(path, mime, source)`.
struct Harness {
    runner: DocumentRunner<'static>,
    wake: mpsc::Receiver<DocumentTask>,
}

impl Harness {
    fn new(files: &[(&str, &str, &str)]) -> Self {
        Self::with_ticks(files).0
    }

    /// Also counts the reads of `/tick.js`, which a worker's JavaScript makes
    /// with importScripts, so a test sees when that JavaScript stops.
    fn with_ticks(files: &[(&str, &str, &str)]) -> (Self, Arc<AtomicUsize>) {
        support::initialize_v8();
        let mut files: HashMap<String, (String, Vec<u8>)> = files
            .iter()
            .map(|(path, mime, source)| {
                ((*path).into(), ((*mime).into(), source.as_bytes().to_vec()))
            })
            .collect();
        files.insert("/tick.js".into(), (JS.into(), Vec::new()));
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let pack = move |path: &str| {
            if path == "/tick.js" {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            files.get(path).cloned()
        };
        let trace = Trace::default();
        let mut runner = DocumentRunner::new(
            options(Instant::now(), true),
            &fixture(&[]),
            sources(pack),
            Box::new(Engine::new(trace.clone())),
            Box::new(Paint(trace)),
            Instant::now,
        )
        .unwrap();
        let (sender, wake) = mpsc::channel();
        runner.set_network_wakeup(sender);
        (Self { runner, wake }, ticks)
    }

    fn eval(&mut self, script: &str) -> String {
        self.runner.eval(script).unwrap()
    }

    /// Runs document turns, as the host does on each wakeup, until the
    /// JavaScript `condition` holds.
    fn wait_for(&mut self, condition: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.runner.run_ready().unwrap();
            if self.eval(condition) == "true" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {condition}: log is {}",
                self.eval("JSON.stringify(globalThis.log ?? null)")
            );
            let _ = self.wake.recv_timeout(Duration::from_millis(5));
        }
    }

    /// Lets the workers and the document run for `millis` more.
    fn settle(&mut self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            let _ = self.wake.recv_timeout(Duration::from_millis(5));
            self.runner.run_ready().unwrap();
        }
    }
}

const JS: &str = "text/javascript";

#[test]
fn classic_worker_round_trips_structured_data_and_transfers_buffers() {
    let mut h = Harness::new(&[(
        "/echo.js",
        JS,
        r#"
        onmessage = async event => {
          const { buffer, map, date, blob } = event.data;
          const bytes = new Uint8Array(buffer);
          bytes[0] += 1;
          const reply = {
            map, date, sum: bytes.reduce((a, b) => a + b, 0), back: buffer, text: await blob.text(),
            name: self.name, location: String(location), ports: event.ports.length,
            globals: [typeof document, typeof window, typeof Worker, typeof SharedWorker,
              self === globalThis, Object.prototype.toString.call(self), typeof importScripts,
              typeof requestAnimationFrame, typeof fetch, typeof crypto.getRandomValues,
              navigator.onLine, navigator.hardwareConcurrency > 0, navigator.userAgent.includes("StrangeQuark")],
          };
          postMessage(reply, [buffer]);
          globalThis.after = buffer.byteLength;
        };
        "#,
    )]);
    h.eval(
        r#"
        globalThis.log = [];
        const worker = new Worker("/echo.js", { name: "echo" });
        worker.onmessage = event => log.push(event.data);
        const buffer = new Uint8Array([1, 2, 3]).buffer;
        worker.postMessage({ buffer, map: new Map([["a", 1]]), date: new Date(5), blob: new Blob(["hi"]) }, [buffer]);
        globalThis.detached = [buffer.byteLength, buffer.detached];
        "#,
    );
    assert_eq!(h.eval("detached"), "[0,true]");
    h.wait_for("log.length === 1");
    assert_eq!(
        h.eval(
            r#"(() => {
              const { map, date, sum, back, text, name, location, ports, globals } = log[0];
              return [map instanceof Map && map.get("a"), date instanceof Date && date.getTime(), sum,
                back instanceof ArrayBuffer && [...new Uint8Array(back)], text, name, location, ports, globals];
            })()"#
        ),
        r#"[1,5,7,[2,2,3],"hi","echo","http://squark.invalid/echo.js",0,["undefined","undefined","undefined","undefined",true,"[object DedicatedWorkerGlobalScope]","function","function","function","function",true,true,true]]"#
    );
}

#[test]
fn module_workers_import_statically_and_dynamically() {
    let mut h = Harness::new(&[
        (
            "/module.js",
            JS,
            r#"
            import { value } from "./dep.js";
            const { value: again } = await import("./dep.js");
            postMessage([value, again, import.meta.url, typeof importScripts]);
            try { importScripts("/dep.js"); } catch (error) { postMessage([error.name, error.message]); }
            onmessage = event => postMessage(event.data * 2);
            "#,
        ),
        ("/dep.js", JS, r#"export const value = "dep";"#),
    ]);
    h.eval(
        r#"
        globalThis.log = [];
        const worker = new Worker("/module.js", { type: "module" });
        worker.onmessage = event => log.push(event.data);
        worker.postMessage(21);
        "#,
    );
    h.wait_for("log.length === 3");
    assert_eq!(
        h.eval("log"),
        r#"[["dep","dep","http://squark.invalid/module.js","function"],["TypeError","Failed to execute 'importScripts' on 'WorkerGlobalScope': Module scripts don't support importScripts()."],42]"#
    );
}

#[test]
fn import_scripts_runs_classic_scripts_in_order_and_throws_network_errors() {
    let mut h = Harness::new(&[
        (
            "/importer.js",
            JS,
            r#"
            importScripts("/lib.js", "lib2.js");
            postMessage([libValue, self.lib2]);
            for (const url of ["/missing.js", "/style.css", "http://[bad"]) {
              try { importScripts(url); } catch (error) { postMessage([error.name, error.message, error.code]); }
            }
            "#,
        ),
        ("/lib.js", JS, "var libValue = 42;"),
        ("/lib2.js", JS, "self.lib2 = libValue + 1;"),
        ("/style.css", "text/css", "body {}"),
    ]);
    h.eval(
        r#"
        globalThis.log = [];
        new Worker("/importer.js").onmessage = event => log.push(event.data);
        "#,
    );
    h.wait_for("log.length === 4");
    assert_eq!(
        h.eval("log"),
        r#"[[42,43],["NetworkError","Failed to execute 'importScripts' on 'WorkerGlobalScope': The script at 'http://squark.invalid/missing.js' failed to load.",19],["NetworkError","Failed to execute 'importScripts' on 'WorkerGlobalScope': The script at 'http://squark.invalid/style.css' failed to load.",19],["SyntaxError","Failed to execute 'importScripts' on 'WorkerGlobalScope': The URL 'http://[bad' is invalid.",12]]"#
    );
}

#[test]
fn blob_url_workers_run_classic_and_module_scripts() {
    let mut h = Harness::new(&[("/dep.js", JS, r#"export const value = "dep";"#)]);
    h.eval(
        r#"
        globalThis.log = [];
        const classic = URL.createObjectURL(new Blob([`
          postMessage([location.href.startsWith("blob:http://squark.invalid/"), typeof importScripts]);
          try { importScripts("./lib.js"); } catch (error) { postMessage(error.name); }
        `], { type: "text/javascript" }));
        new Worker(classic).onmessage = event => log.push(event.data);
        const module = URL.createObjectURL(new Blob([`
          import { value } from "http://squark.invalid/dep.js";
          postMessage(value);
        `], { type: "text/javascript" }));
        new Worker(module, { type: "module" }).onmessage = event => log.push(event.data);
        "#,
    );
    h.wait_for("log.length === 3");
    assert_eq!(
        h.eval("[...log].sort((a, b) => JSON.stringify(a) < JSON.stringify(b) ? -1 : 1)"),
        r#"["SyntaxError","dep",[true,"function"]]"#
    );
}

#[test]
fn uncaught_worker_errors_reach_the_worker_then_the_window() {
    let mut h = Harness::new(&[
        (
            "/throw.js",
            JS,
            "\n  function f() { throw new Error('boom'); }\nf();",
        ),
        ("/syntax.js", JS, "let x = ;"),
        (
            "/handled.js",
            JS,
            r#"
            self.onerror = (message, filename, lineno, colno, error) => {
              postMessage([message, lineno, String(error)]);
              return true;
            };
            setTimeout(() => { throw new Error("handled"); }, 0);
            "#,
        ),
        (
            "/listener.js",
            JS,
            "onmessage = () => { throw new TypeError('in listener'); };",
        ),
        ("/module-syntax.js", JS, "export let x = ;"),
        ("/module-throw.js", JS, "throw new Error('module boom');"),
        ("/canceled.js", JS, "throw 'string';"),
    ]);
    h.eval(
        r#"
        globalThis.log = [];
        globalThis.onerror = (message, filename, lineno, colno, error) => {
          log.push(["window", message, filename, lineno, colno, error]);
        };
        const describe = event => event instanceof ErrorEvent
          ? [event.constructor.name, event.message, event.filename, event.lineno, event.colno, event.error, event.cancelable]
          : [event.constructor.name, event.cancelable];
        for (const [url, type] of [["/throw.js"], ["/syntax.js"], ["/handled.js"], ["/listener.js"],
            ["/missing.js"], ["/module-syntax.js", "module"], ["/module-throw.js", "module"]]) {
          const worker = new Worker(url, { type });
          worker.onerror = event => log.push([url, ...describe(event)]);
          worker.onmessage = event => log.push([url, "message", event.data]);
          if (url === "/listener.js") worker.postMessage(1);
        }
        const canceled = new Worker("/canceled.js");
        canceled.addEventListener("error", event => { event.preventDefault(); log.push(["/canceled.js", event.message]); });
        "#,
    );
    h.wait_for("log.length === 12");
    let expected = [
        r#"["/canceled.js","Uncaught string"]"#,
        r#"["/handled.js","message",["Uncaught Error: handled",6,"Error: handled"]]"#,
        r#"["/listener.js","ErrorEvent","Uncaught TypeError: in listener","http://squark.invalid/listener.js",1,27,null,true]"#,
        r#"["/missing.js","Event",true]"#,
        r#"["/module-syntax.js","Event",true]"#,
        r#"["/module-throw.js","ErrorEvent","Uncaught Error: module boom","http://squark.invalid/module-throw.js",1,7,null,true]"#,
        r#"["/syntax.js","ErrorEvent","Uncaught SyntaxError: Unexpected token ';'","http://squark.invalid/syntax.js",1,9,null,true]"#,
        r#"["/throw.js","ErrorEvent","Uncaught Error: boom","http://squark.invalid/throw.js",2,24,null,true]"#,
        r#"["window","Uncaught Error: boom","http://squark.invalid/throw.js",2,24,null]"#,
        r#"["window","Uncaught Error: module boom","http://squark.invalid/module-throw.js",1,7,null]"#,
        r#"["window","Uncaught SyntaxError: Unexpected token ';'","http://squark.invalid/syntax.js",1,9,null]"#,
        r#"["window","Uncaught TypeError: in listener","http://squark.invalid/listener.js",1,27,null]"#,
    ];
    assert_eq!(
        h.eval("log.map(entry => JSON.stringify(entry)).sort()"),
        serde_json::to_string(&expected).unwrap()
    );
    // The canceled error and the handled one never reach the window.
    h.settle(50);
    assert_eq!(h.eval("log.length"), "12");
}

#[test]
fn terminate_stops_a_busy_worker_without_blocking_the_document() {
    let (mut h, ticks) = Harness::with_ticks(&[(
        "/busy.js",
        JS,
        r#"onmessage = () => { postMessage("spinning"); for (;;) importScripts("/tick.js"); };"#,
    )]);
    h.eval(
        r#"
        globalThis.log = [];
        globalThis.busy = new Worker("/busy.js");
        busy.onmessage = event => log.push(event.data);
        busy.postMessage(0);
        "#,
    );
    h.wait_for("log.length === 1");
    let spinning = ticks.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(20));
    assert!(ticks.load(Ordering::Relaxed) > spinning);
    let start = Instant::now();
    h.eval("busy.terminate(); busy.postMessage(1); busy.terminate();");
    assert!(start.elapsed() < Duration::from_millis(200));
    let stopped = quiescent(&ticks);
    h.settle(50);
    assert_eq!(ticks.load(Ordering::Relaxed), stopped);
    assert_eq!(h.eval("log"), r#"["spinning"]"#);
    // Its thread has ended, so closing the document does not wait for it.
    let start = Instant::now();
    drop(h);
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
fn close_inside_a_worker_keeps_what_it_posted_and_drops_later_tasks() {
    let (mut h, ticks) = Harness::with_ticks(&[(
        "/close.js",
        JS,
        r#"
        setInterval(() => importScripts("/tick.js"), 1);
        onmessage = () => {
          postMessage("before");
          close();
          postMessage("after");
          setTimeout(() => postMessage("timer"), 0);
          queueMicrotask(() => postMessage("microtask"));
        };
        "#,
    )]);
    h.eval(
        r#"
        globalThis.log = [];
        globalThis.worker = new Worker("/close.js");
        worker.onmessage = event => log.push(event.data);
        "#,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while ticks.load(Ordering::Relaxed) < 3 {
        assert!(Instant::now() < deadline, "the worker did not start");
        h.settle(5);
    }
    h.eval("worker.postMessage(0)");
    h.wait_for("log.length === 3");
    let stopped = quiescent(&ticks);
    h.eval("worker.postMessage(1)");
    h.settle(100);
    assert_eq!(h.eval("log"), r#"["before","after","microtask"]"#);
    assert_eq!(ticks.load(Ordering::Relaxed), stopped);
}

#[test]
fn document_shutdown_stops_its_workers() {
    let (mut h, ticks) = Harness::with_ticks(&[
        ("/busy.js", JS, r#"for (;;) importScripts("/tick.js");"#),
        ("/idle.js", JS, "onmessage = () => {};"),
    ]);
    h.eval(r#"new Worker("/busy.js"); new Worker("/busy.js"); new Worker("/idle.js");"#);
    let deadline = Instant::now() + Duration::from_secs(10);
    while ticks.load(Ordering::Relaxed) < 10 {
        assert!(Instant::now() < deadline, "the workers did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (sender, receiver) = mpsc::channel();
    sender.send(DocumentTask::Shutdown).unwrap();
    let start = Instant::now();
    h.runner.run(receiver, |_| {}).unwrap();
    // Stopping waits for the threads, which end as soon as V8 terminates.
    assert!(start.elapsed() < Duration::from_millis(500));
    let stopped = ticks.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(ticks.load(Ordering::Relaxed), stopped);
}

#[test]
fn workers_run_timers_microtasks_frames_and_fetch() {
    let mut h = Harness::new(&[
        (
            "/tasks.js",
            JS,
            r#"
            const log = [];
            const done = name => { log.push(String(name)); if (log.length === 7) postMessage(log.sort()); };
            setTimeout(() => done("timeout"), 5);
            let count = 0;
            const interval = setInterval(() => { if (++count === 3) { clearInterval(interval); done("interval"); } }, 1);
            queueMicrotask(() => done("microtask"));
            requestAnimationFrame(time => done(typeof time === "number" && time > 0 ? "frame" : "bad frame"));
            fetch("data:text/plain,hello%20worker").then(response => response.text()).then(done);
            fetch("/data.json").then(response => response.json()).then(value => done(value.answer));
            crypto.subtle.digest("SHA-256", new TextEncoder().encode("abc")).then(digest => done(new Uint8Array(digest)[0]));
            const controller = new AbortController();
            controller.abort();
            postMessage([
              structuredClone(new Map([[1, 2]])).get(1), performance.now() > 0, atob(btoa("ok")),
              new URL("../x?y=1", location.href).href, new URLSearchParams("a=1&a=2").getAll("a"),
              new TextDecoder().decode(new Uint8Array([104, 105])), controller.signal.aborted,
              /^[0-9a-f-]{36}$/.test(crypto.randomUUID()), typeof URL.createObjectURL(new Blob(["x"])),
              typeof localStorage, typeof indexedDB, typeof document,
            ]);
            "#,
        ),
        ("/data.json", "application/json", r#"{"answer":"packed"}"#),
    ]);
    h.eval(
        r#"
        globalThis.log = [];
        new Worker("/tasks.js").onmessage = event => log.push(event.data);
        "#,
    );
    h.wait_for("log.length === 2");
    assert_eq!(
        h.eval("log"),
        r#"[[2,true,"ok","http://squark.invalid/x?y=1",["1","2"],"hi",true,true,"string","undefined","undefined","undefined"],["186","frame","hello worker","interval","microtask","packed","timeout"]]"#
    );
}

#[test]
fn worker_construction_and_messages_validate_like_chromium() {
    let mut h = Harness::new(&[(
        "/clone.js",
        JS,
        r#"
        onmessage = () => {
          try { postMessage(() => 1); }
          catch (error) { postMessage([error.name, error.message, error instanceof DOMException]); }
        };
        "#,
    )]);
    let result = h.eval(
        r#"(() => {
          const errors = [];
          const attempt = run => {
            try { run(); errors.push("ok"); }
            catch (error) { errors.push([error.name, error.message]); }
          };
          attempt(() => new Worker("https://example.com/x.js"));
          attempt(() => new Worker("http://[bad"));
          attempt(() => new Worker("/clone.js", { type: "nope" }));
          attempt(() => new Worker());
          attempt(() => Worker("/clone.js"));
          const worker = new Worker("/clone.js");
          attempt(() => worker.postMessage());
          attempt(() => worker.postMessage(() => 1));
          attempt(() => worker.postMessage(1, [{}]));
          const buffer = new ArrayBuffer(1);
          attempt(() => worker.postMessage(buffer, [buffer, buffer]));
          attempt(() => worker.postMessage(1, [new MessageChannel().port1]));
          attempt(() => worker.postMessage(1, { transfer: [new ArrayBuffer(2)] }));
          return [errors, typeof SharedWorker, Object.getOwnPropertyNames(Worker.prototype).sort(),
            Object.getPrototypeOf(Worker.prototype) === EventTarget.prototype];
        })()"#,
    );
    let expected = serde_json::json!([
        [
            [
                "SecurityError",
                "Failed to construct 'Worker': Script at 'https://example.com/x.js' cannot be accessed from origin 'http://squark.invalid'."
            ],
            [
                "SyntaxError",
                "Failed to construct 'Worker': 'http://[bad' is not a valid URL."
            ],
            [
                "TypeError",
                "Failed to construct 'Worker': Failed to read the 'type' property from 'WorkerOptions': The provided value 'nope' is not a valid enum value of type WorkerType."
            ],
            [
                "TypeError",
                "Failed to construct 'Worker': 1 argument required, but only 0 present."
            ],
            [
                "TypeError",
                "Class constructor Worker cannot be invoked without 'new'"
            ],
            [
                "TypeError",
                "Failed to execute 'postMessage' on 'Worker': 1 argument required, but only 0 present."
            ],
            [
                "DataCloneError",
                "Failed to execute 'postMessage' on 'Worker': () => 1 could not be cloned."
            ],
            [
                "DataCloneError",
                "Failed to execute 'postMessage' on 'Worker': Value at index 0 does not have a transferable type."
            ],
            [
                "DataCloneError",
                "Failed to execute 'postMessage' on 'Worker': ArrayBuffer at index 1 is a duplicate of an earlier ArrayBuffer."
            ],
            [
                "DataCloneError",
                "Failed to execute 'postMessage' on 'Worker': MessagePort at index 0 cannot be transferred to another thread."
            ],
            "ok",
        ],
        "undefined",
        [
            "constructor",
            "onerror",
            "onmessage",
            "postMessage",
            "terminate"
        ],
        true,
    ]);
    assert_eq!(result, expected.to_string());
    h.eval(
        r#"
        globalThis.log = [];
        const worker = new Worker("/clone.js");
        worker.onmessage = event => log.push(event.data);
        worker.postMessage(0);
        "#,
    );
    h.wait_for("log.length === 1");
    assert_eq!(
        h.eval("log[0]"),
        r#"["DataCloneError","Failed to execute 'postMessage' on 'DedicatedWorkerGlobalScope': () => 1 could not be cloned.",true]"#
    );
}
