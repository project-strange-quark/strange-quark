// SPDX-License-Identifier: MIT

use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

#[test]
fn indexed_db_cursor_reuses_its_request_until_the_store_is_exhausted() {
    let root = key(&format!("target/dom-idb-cursor-{}", std::process::id()));
    let mut runner = runner(sources(|_| None).with_storage_root(root.clone()));
    runner
        .eval(
            r#"
        globalThis.rows = [];
        const opening = indexedDB.open("cursor-test");
        opening.onupgradeneeded = () => {
          const items = opening.result.createObjectStore("items");
          items.put("third", 3); items.put("first", 1); items.put("second", 2);
        };
        opening.onsuccess = () => {
          const items = opening.result.transaction("items").objectStore("items");
          const scan = items.openCursor();
          let cursor;
          scan.onsuccess = function() {
            const current = this.result;
            if (current === null) { rows.push("done"); return; }
            rows.push([current instanceof IDBCursorWithValue, current === (cursor ||= current),
              current.request === this, current.key, current.primaryKey, current.value]);
            if (current.key === 1) current.continue(2);
            else current.continue();
            rows.push(this.readyState);
          };
          const backwards = items.openCursor(null, "prev");
          backwards.onsuccess = () => {
            const current = backwards.result;
            if (current === null) { rows.push("reverse done"); return; }
            rows.push(current.key);
            current.advance(2);
          };
        };
    "#,
        )
        .unwrap();
    for _ in 0..8 {
        runner.run_ready().unwrap();
    }
    assert_eq!(
        runner.eval("rows").unwrap(),
        r#"[[true,true,true,1,1,"first"],"pending",3,[true,true,true,2,2,"second"],"pending",1,[true,true,true,3,3,"third"],"pending","reverse done","done"]"#
    );
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn runner(sources: ScriptSources) -> DocumentRunner<'static> {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources,
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap()
}

fn finish(runner: &mut DocumentRunner<'_>) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runner.run_ready().unwrap();
        let value = runner.eval("globalThis.result").unwrap();
        if value != "null" && value != "undefined" {
            return value;
        }
        assert!(Instant::now() < deadline, "data test timed out");
    }
}

#[test]
fn fetch_data_urls_returns_decoded_bytes_and_the_serialized_mime_type() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let pack =
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap();
    let bytes = pack.entry("/images/dot.png").unwrap().bytes;
    let url = format!("data:image/png;base64,{}", STANDARD.encode(bytes));
    let mut runner = runner(sources(|_| None));
    runner
        .eval(&format!("globalThis.pngURL = {url:?}"))
        .unwrap();
    runner.eval(r##"
        globalThis.result = null;
        (async () => {
          const plain = await fetch("data:text/plain,squark-probe");
          const raw = await fetch("data:,A+%23%00%C3%A9?end#ignored");
          const normalized = await fetch('data:TEXT/PLAIN;Charset="UTF-8",a%20b#ignored');
          const forgiving = await fetch("data:;base64,aGVs%0AbG8");
          const image = await fetch(pngURL);
          const blob = await image.blob();
          const long = await fetch("data:," + "a".repeat(65536) + "end");
          const text = await long.text();
          const invalid = await Promise.all(['data:text/plain;base64,a', 'data:text/plain']
            .map(url => fetch(url).then(() => 'unexpected success', e => e.name)));
          const head = await fetch('data:,head', { method: 'HEAD' });
          const post = await fetch('data:,post', { method: 'POST', body: 'unused' });
          result = [[plain.status, plain.statusText, plain.type, [...plain.headers], await plain.text()],
            [raw.url, raw.headers.get('content-type'), [...new Uint8Array(await raw.arrayBuffer())]],
            [normalized.headers.get('content-type'), await normalized.text(), await forgiving.text()],
            [image.status, blob instanceof Blob, blob.type, [...new Uint8Array(await blob.arrayBuffer())]],
            [text.length, text.slice(-3)], invalid, [head.status, await head.text(), await post.text()]];
        })().catch(e => { result = [String(e), e.stack]; });
    "##).unwrap();
    let result: serde_json::Value = serde_json::from_str(&finish(&mut runner)).unwrap();
    assert_eq!(
        result,
        serde_json::json!([
            [
                200,
                "OK",
                "basic",
                [["content-type", "text/plain"]],
                "squark-probe"
            ],
            [
                "data:,A+%23%00%C3%A9?end",
                "text/plain;charset=US-ASCII",
                [65, 43, 35, 0, 195, 169, 63, 101, 110, 100]
            ],
            ["text/plain;charset=UTF-8", "a b", "hello"],
            [200, true, "image/png", bytes],
            [65539, "end"],
            ["TypeError", "TypeError"],
            [200, "", "post"],
        ])
    );
}

#[test]
fn files_clone_and_fetch_the_same_blob_url() {
    let registry = squark_js::BlobRegistry::default();
    let mut runner = runner(sources(|_| None).with_blob_registry(registry.clone()));
    runner.eval(r##"
        globalThis.result = null;
        globalThis.file = new File([new Uint8Array([33]), 'é'], 'example.txt',
            {type:'TEXT/PLAIN', lastModified:21});
        globalThis.url = URL.createObjectURL(file);
        (async () => {
          const body = await fetch(url + "#preview");
          const copy = structuredClone({file, alias:file});
          const events = [];
          const reader = new FileReader();
          const read = new Promise(resolve => { reader.onloadend = resolve; });
          reader.onloadstart = e => events.push([e.type, e.lengthComputable, e.total]);
          reader.onload = e => events.push([e.type, reader.readyState, reader.result]);
          reader.readAsDataURL(file);
          const pending = reader.readyState;
          await read;
          URL.revokeObjectURL(url);
          const revoked = await fetch(url).then(() => "unexpected", e => e.name);
          const input = Uint8Array.of(1, 2, 3);
          const cyclic = {input};
          cyclic.self = cyclic;
          const moved = structuredClone(cyclic, {transfer:[input.buffer]});
          const preserved = new ArrayBuffer(3);
          let duplicate, uncloneable;
          try { structuredClone(null, {transfer:[preserved, preserved]}); } catch (e) { duplicate = e.name; }
          try { structuredClone({nested:[document]}); } catch (e) { uncloneable = e.name; }
          result = [await body.text(), body.headers.get("content-type"), copy.file instanceof File, copy.file === copy.alias,
            copy.file.name, copy.file.lastModified, await copy.file.text(), pending, events, revoked,
            input.byteLength, moved.self === moved, [...moved.input], duplicate, preserved.byteLength, uncloneable];
        })().catch(e => { result = ["error", String(e), e.stack]; });
    "##).unwrap();
    let url: String = serde_json::from_str(&runner.eval("url").unwrap()).unwrap();
    assert!(url.starts_with("blob:http://squark.invalid/"));
    assert_eq!(
        registry.resolve(&url).unwrap().bytes.as_ref(),
        "!é".as_bytes()
    );
    assert_eq!(
        finish(&mut runner),
        r#"["!é","text/plain",true,true,"example.txt",21,"!é",1,[["loadstart",true,3],["load",2,"data:text/plain;base64,IcOp"]],"TypeError",0,true,[1,2,3],"DataCloneError",3,"DataCloneError"]"#
    );
    assert!(registry.resolve(&url).is_none());
}

#[test]
fn web_crypto_matches_sha_and_aes_gcm_vectors_and_checks_key_access() {
    let mut runner = runner(sources(|_| None));
    runner.eval(r#"
        globalThis.result = null;
        (async () => {
          const hex = data => Array.from(new Uint8Array(data), b => b.toString(16).padStart(2, '0')).join("");
          const subtle = crypto.subtle;
          const input = Uint8Array.of(97, 98, 99);
          const sha = await Promise.all(["SHA-1", "sha-256"].map(name => subtle.digest(name, input).then(hex)));
          const key = await subtle.importKey("jwk", {kty:"oct", alg:"A128GCM", k:"AAAAAAAAAAAAAAAAAAAAAA",
            ext:true, key_ops:["encrypt", "decrypt"]}, {name:"AES-GCM"}, true, ["encrypt", "decrypt"]);
          const params = {name:"AES-GCM", iv:new Uint8Array(12)};
          const encrypted = new Uint8Array(await subtle.encrypt(params, key, new Uint8Array(16)));
          const decrypted = hex(await subtle.decrypt(params, key, encrypted));
          const jwk = await subtle.exportKey("jwk", key);
          const vector = hex(encrypted);
          encrypted[0] ^= 1;
          const tampered = await subtle.decrypt(params, key, encrypted).catch(e => e.name);
          const protectedKey = await subtle.generateKey({name:"AES-GCM",length:128}, false, ["encrypt"]);
          const nonExportable = await subtle.exportKey("jwk", protectedKey).catch(e => e.name);
          const nonDecryptable = await subtle.decrypt(params, protectedKey, encrypted).catch(e => e.name);
          const random = new BigUint64Array(2);
          result = [sha, vector, decrypted, jwk.k, jwk.key_ops, tampered, nonExportable, nonDecryptable,
            crypto.getRandomValues(random) === random, /^[0-9a-f-]{36}$/.test(crypto.randomUUID())];
        })().catch(e => { result = ["error", String(e), e.stack]; });
    "#).unwrap();
    assert_eq!(
        finish(&mut runner),
        r#"[["a9993e364706816aba3e25717850c26c9cd0d89d","ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"],"0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf","00000000000000000000000000000000","AAAAAAAAAAAAAAAAAAAAAA",["encrypt","decrypt"],"OperationError","InvalidAccessError","InvalidAccessError",true,true]"#
    );
}

#[test]
fn indexed_db_commits_atomic_updates_and_reopens_blob_values() {
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    let root = key(&format!(
        "target/dom-idb-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let make = || {
        runner(
            sources(|path| {
                (path == squark_types::pack::MANIFEST_PATH).then(|| {
                    (
                        "application/json".to_owned(),
                        br#"{"app_name":"indexed-db-test"}"#.to_vec(),
                    )
                })
            })
            .with_storage_root(root.clone()),
        )
    };
    let helpers = r#"
      globalThis.result = null;
      globalThis.wait = request => new Promise((resolve, reject) => {
        request.onsuccess = request.oncomplete = () => resolve(request.result);
        request.onerror = request.onabort = () => reject(request.error);
      });
      globalThis.open = () => {
        const request = indexedDB.open("keyval");
        request.onupgradeneeded = () => request.result.createObjectStore("values");
        return wait(request);
      };
    "#;
    {
        let mut first = make();
        first.eval(helpers).unwrap();
        first.eval(r#"
            (async () => {
              const db = await open();
              const tx = db.transaction("values", "readwrite");
              tx.objectStore("values").put(new File(["file"], "stored.txt"), [0, "file"]);
              tx.objectStore("values").put({n:1}, "count");
              await wait(tx);
              const update = () => {
                const tx = db.transaction("values", "readwrite");
                const store = tx.objectStore("values");
                store.get("count").onsuccess = function () { store.put({n:this.result.n + 1}, "count"); };
                return wait(tx);
              };
              await Promise.all([update(), update()]);
              const aborted = db.transaction("values", "readwrite");
              aborted.objectStore("values").put("lose", "count");
              const completion = wait(aborted).catch(e => e.name);
              aborted.abort();
              const store = db.transaction("values").objectStore("values");
              const keys = wait(store.getAllKeys());
              const value = wait(store.get("count"));
              result = [await completion, await keys, await value];
            })().catch(e => { result = ["error", String(e), e.stack]; });
        "#).unwrap();
        assert_eq!(
            finish(&mut first),
            r#"["AbortError",["count",[0,"file"]],{"n":3}]"#
        );
    }
    {
        let mut second = make();
        second.eval(helpers).unwrap();
        second.eval(r#"
            (async () => {
              const db = await open();
              const file = await wait(db.transaction("values").objectStore("values").get([0, "file"]));
              result = [file instanceof File, file.name, await file.text()];
            })().catch(e => { result = ["error", String(e)]; });
        "#).unwrap();
        assert_eq!(finish(&mut second), r#"[true,"stored.txt","file"]"#);
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn durable_sources(root: &Path) -> ScriptSources {
    sources(|path| {
        (path == squark_types::pack::MANIFEST_PATH).then(|| {
            (
                "application/json".to_owned(),
                br#"{"app_name":"shutdown-test"}"#.to_vec(),
            )
        })
    })
    .with_storage_root(root.to_owned())
}

const OPEN_DURABLE_DB: &str = r#"
    globalThis.result = null;
    const request = indexedDB.open("durable");
    request.onupgradeneeded = () => request.result.createObjectStore("items");
    request.onsuccess = () => { globalThis.db = request.result; result = "open"; };
"#;

fn quit(runner: DocumentRunner<'_>) {
    let (send, receive) = mpsc::channel();
    send.send(DocumentTask::Shutdown).unwrap();
    runner
        .run(receive, |_| panic!("frame during quit"))
        .unwrap();
}

#[test]
fn indexed_db_quit_drains_only_committing_transactions_in_order() {
    let root = key(&format!("target/dom-idb-quit-{}", std::process::id()));
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let clock = Rc::new(Cell::new(origin));
    let paced = Rc::new(Cell::new(false));
    let mut first = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        durable_sources(&root),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        {
            let clock = clock.clone();
            let paced = paced.clone();
            move || {
                let now = clock.get();
                if paced.get() {
                    clock.set(now + Duration::from_millis(3));
                }
                now
            }
        },
    )
    .unwrap();
    first.eval(OPEN_DURABLE_DB).unwrap();
    assert_eq!(finish(&mut first), r#""open""#);
    first
        .eval(
            r#"
        globalThis.handled = false;
        globalThis.completed = false;
        const auto = db.transaction("items", "readwrite");
        auto.oncomplete = () => { completed = true; localStorage.setItem("callback", "complete"); };
        auto.objectStore("items").put("first", "a").onsuccess = async () => {
          await Promise.resolve();
          auto.objectStore("items").put("microtask write", "b").onsuccess = () => { handled = true; };
        };
    "#,
        )
        .unwrap();
    paced.set(true);
    for _ in 0..24 {
        if first.eval("handled").unwrap() == "true" {
            break;
        }
        first.run_ready().unwrap();
    }
    assert_eq!(first.eval("[handled, completed]").unwrap(), "[true,false]");
    assert_eq!(
        first
            .eval(
                r#"
        const open = db.transaction("items", "readwrite");
        open.objectStore("items").put("discard", "c");
        const explicit = db.transaction("items", "readwrite");
        const store = explicit.objectStore("items");
        store.put(new Blob(["file contents"]), "d").onsuccess =
          () => localStorage.setItem("callback", "request");
        explicit.commit();
        const queued = db.transaction("items", "readwrite");
        queued.objectStore("items").put("last", "a");
        queued.commit();
        setTimeout(() => localStorage.setItem("callback", "timer"), 0);
        try { store.put("after commit", "e"); } catch (error) { error.name; }
    "#,
            )
            .unwrap(),
        r#""TransactionInactiveError""#
    );
    quit(first);

    let mut second = runner(durable_sources(&root));
    second.eval(OPEN_DURABLE_DB).unwrap();
    assert_eq!(finish(&mut second), r#""open""#);
    second
        .eval(
            r#"
        result = null;
        const read = db.transaction("items").objectStore("items").getAll();
        read.onsuccess = async () => {
          const [first, chained, blob] = read.result;
          result = [first, chained, blob instanceof Blob, await blob.text(), localStorage.getItem("callback")];
        };
    "#,
        )
        .unwrap();
    assert_eq!(
        finish(&mut second),
        r#"["last","microtask write",true,"file contents",null]"#
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn indexed_db_quit_stops_waiting_for_a_locked_storage_backend() {
    let root = key(&format!("target/dom-idb-deadline-{}", std::process::id()));
    let mut first = runner(durable_sources(&root));
    first.eval(OPEN_DURABLE_DB).unwrap();
    assert_eq!(finish(&mut first), r#""open""#);
    let app = std::fs::read_dir(&root).unwrap().next().unwrap().unwrap();
    let lock = File::open(app.path().join("local-storage.lock")).unwrap();
    // The same std lock the storage backend takes: flock on Unix,
    // LockFileEx on Windows.
    lock.lock().unwrap();
    first
        .eval(
            r#"
        const tx = db.transaction("items", "readwrite");
        tx.objectStore("items").put("must not publish late", "value");
        tx.commit();
    "#,
        )
        .unwrap();
    let started = Instant::now();
    quit(first);
    assert!(started.elapsed() < Duration::from_secs(3));
    drop(lock);

    let mut second = runner(durable_sources(&root));
    second.eval(OPEN_DURABLE_DB).unwrap();
    assert_eq!(finish(&mut second), r#""open""#);
    second
        .eval(
            r#"
        result = null;
        const read = db.transaction("items").objectStore("items").getAll();
        read.onsuccess = () => { result = read.result; };
    "#,
        )
        .unwrap();
    assert_eq!(finish(&mut second), "[]");
    std::fs::remove_dir_all(root).unwrap();
}
