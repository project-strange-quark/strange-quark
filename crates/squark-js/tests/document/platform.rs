// SPDX-License-Identifier: MIT

use std::sync::mpsc;

use squark_js::CanvasBackend;
use squark_types::host::{
    ClipboardItem, FileHandle, HostError, HostRequest, HostValue, Input, InputTask, KeyKind,
    Modifiers,
};

use super::*;

struct Harness {
    runner: DocumentRunner<'static>,
    journal: Rc<RefCell<JournalLog>>,
    host: mpsc::Receiver<HostRequest>,
    now: Instant,
}

impl Harness {
    fn new() -> Self {
        Self::with_sources(sources(|_| None))
    }

    fn with_sources(sources: ScriptSources) -> Self {
        support::initialize_v8();
        let (sender, host) = mpsc::channel();
        let now = Instant::now();
        let trace = Trace::default();
        let engine = Engine::new(trace.clone());
        let journal = engine.journal.clone();
        let runner = DocumentRunner::new(
            options(now, true),
            &fixture(&[]),
            sources.with_host_requests(sender),
            Box::new(engine),
            Box::new(Paint(trace)),
            Instant::now,
        )
        .unwrap();
        Self {
            runner,
            journal,
            host,
            now,
        }
    }

    fn eval(&mut self, script: &str) -> String {
        self.runner.eval(script).unwrap()
    }

    fn key(&mut self, key: &str, modifiers: Modifiers) -> bool {
        self.now += Duration::from_millis(1);
        self.runner
            .input(&InputTask {
                request: 1,
                target: BODY,
                related: NodeId::NONE,
                time: self.now,
                modifiers,
                input: Input::Key {
                    kind: KeyKind::Down,
                    key: key.into(),
                    code: String::new(),
                    location: 0,
                    repeat: false,
                    composing: false,
                },
            })
            .unwrap()
            .default_prevented
    }

    fn request(&self) -> HostRequest {
        self.host.try_recv().expect("host request")
    }

    fn request_when_ready(&mut self) -> HostRequest {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.runner.run_ready().unwrap();
            match self.host.try_recv() {
                Ok(request) => return request,
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "host request did not settle");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("host request channel disconnected")
                }
            }
        }
    }

    fn reply(&mut self, id: u32, value: HostValue) {
        self.runner.host_reply(id, Ok(value)).unwrap();
    }
}

#[test]
fn host_platform_fields_match_the_rust_target() {
    let mut h = Harness::new();
    assert_eq!(
        h.eval("[__squark.os, __squark.arch]"),
        format!(
            "[\"{}\",\"{}\"]",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    );
}

fn clipboard_item(mime: &str, bytes: &[u8]) -> ClipboardItem {
    ClipboardItem {
        mime: mime.into(),
        bytes: Arc::from(bytes),
    }
}

#[test]
fn clipboard_without_host_rejects_through_the_api() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap();
    assert_eq!(
        runner.eval(r#"
            globalThis.denied = [];
            for (const work of [
              navigator.clipboard.readText(), navigator.clipboard.writeText("a"),
              navigator.clipboard.read(),
              navigator.clipboard.write([new ClipboardItem({"text/plain": new Blob(["a"], {type:"text/plain"})})]),
            ]) work.catch(e => denied.push(e.name));
            [navigator.clipboard instanceof Clipboard, denied.length]
        "#).unwrap(),
        "[true,0]"
    );
    assert_eq!(
        runner.eval("denied").unwrap(),
        r#"["NotAllowedError","NotAllowedError","NotAllowedError","NotAllowedError"]"#
    );
}

#[test]
fn clipboard_promises_and_native_keyboard_defaults_use_the_host() {
    let mut h = Harness::new();
    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    assert_eq!(
        h.eval(
            r#"
        globalThis.editor = document.createElement("textarea");
        document.body.appendChild(editor);
        editor.value = "héllo";
        editor.focus();
        editor.setSelectionRange(0, 2);
        globalThis.events = [];
        for (const name of ["keydown", "copy", "cut", "paste", "beforeinput", "input"]) {
          editor.addEventListener(name, e => events.push(e.inputType || name));
        }
        navigator.userAgent.includes("StrangeQuark") && navigator.clipboard instanceof Clipboard
    "#
        ),
        "true"
    );
    h.key("c", ctrl);
    let HostRequest::ClipboardWrite { id, items } = h.request() else {
        panic!("copy request");
    };
    assert_eq!(&*items, &[clipboard_item("text/plain", "hé".as_bytes())]);
    h.reply(id, HostValue::Done);
    assert_eq!(h.eval("events.splice(0)"), r#"["keydown","copy"]"#);

    h.eval(
        r#"editor.addEventListener("cut", e => {
      e.preventDefault();
      e.clipboardData.setData("text", "overridden");
    }, {once:true});"#,
    );
    h.key("x", ctrl);
    let HostRequest::ClipboardWrite { id, items } = h.request() else {
        panic!("cut override");
    };
    assert_eq!(&*items, &[clipboard_item("text/plain", b"overridden")]);
    h.reply(id, HostValue::Done);
    assert_eq!(h.eval("editor.value"), r#""héllo""#);
    h.key("x", ctrl);
    let HostRequest::ClipboardWrite { id, items } = h.request() else {
        panic!("cut request");
    };
    assert_eq!(&*items, &[clipboard_item("text/plain", "hé".as_bytes())]);
    h.reply(id, HostValue::Done);
    assert_eq!(
        h.eval(r#"[editor.value, events.splice(0)]"#),
        r#"["llo",["keydown","cut","keydown","cut","deleteByCut","deleteByCut"]]"#
    );

    h.eval(r#"
        editor.addEventListener("keydown", e => e.preventDefault(), {once:true});
        editor.addEventListener("paste", e => {
          const transfer = e.clipboardData;
          transfer.setData("text/plain", "ignored");
          globalThis.pasteData = [e instanceof ClipboardEvent, e.isTrusted, transfer.getData("text"),
            transfer.types, transfer.items[1].kind, transfer.files.item(0).name, transfer.files[0].size];
        });
    "#);
    assert!(h.key("v", ctrl));
    assert!(h.host.try_recv().is_err());
    h.key("v", ctrl);
    let HostRequest::ClipboardRead { id } = h.request() else {
        panic!("paste request");
    };
    assert_eq!(h.eval("editor.value"), r#""llo""#);
    h.reply(
        id,
        HostValue::Clipboard(Box::new([
            clipboard_item("text/plain", b"ok"),
            clipboard_item("image/png", &[1, 2]),
        ])),
    );
    assert_eq!(
        h.eval("[editor.value, pasteData, events.slice(-3)]"),
        r#"["okllo",[true,true,"ok",["text/plain","Files"],"file","image.png",2],["paste","insertFromPaste","insertFromPaste"]]"#
    );

    h.eval(r#"
      globalThis.done = false;
      void navigator.clipboard.write([new ClipboardItem({"image/png": Promise.resolve(new Blob([Uint8Array.of(3,4)], {type:"image/png"}))})])
        .then(() => { globalThis.done = true; });
    "#);
    let HostRequest::ClipboardWrite { id, items } = h.request() else {
        panic!("ClipboardItem request");
    };
    assert_eq!(&*items, &[clipboard_item("image/png", &[3, 4])]);
    assert_eq!(h.eval("done"), "false");
    h.reply(id, HostValue::Done);
    assert_eq!(h.eval("done"), "true");
    h.eval("void navigator.clipboard.read().then(async ([item]) => globalThis.readBack = await (await item.getType('text/plain')).text())");
    let HostRequest::ClipboardRead { id } = h.request() else {
        panic!("read request");
    };
    h.reply(
        id,
        HostValue::Clipboard(Box::new([clipboard_item("text/plain", b"read back")])),
    );
    assert_eq!(h.eval("readBack"), r#""read back""#);
}

#[test]
fn png_canvas_blob_crosses_the_download_and_clipboard_bridges() {
    struct PngEncoder(Vec<u8>);

    impl CanvasBackend for PngEncoder {
        fn encode(&mut self, _: NodeId, mime: &str, _: Option<f64>) -> Result<Vec<u8>, String> {
            assert_eq!(mime, "image/png");
            Ok(self.0.clone())
        }
    }

    let pack =
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap();
    let png = pack.entry("/images/dot.png").unwrap().bytes;
    let mut h =
        Harness::with_sources(sources(|_| None).with_canvas_backend(PngEncoder(png.to_vec())));
    assert_eq!(h.eval(r#"
      globalThis.exported = false;
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = 1;
      canvas.getContext('2d').fillRect(0, 0, 1, 1);
      canvas.toBlob(blob => {
        const link = document.createElement('a');
        link.download = 'scene.png';
        link.href = URL.createObjectURL(blob);
        link.click();
        URL.revokeObjectURL(link.href);
        globalThis.written = navigator.clipboard.write([new ClipboardItem({'image/png': Promise.resolve(blob)})]);
        void written.then(() => { exported = true; });
      }, 'image/png');
      exported
    "#), "false");
    let HostRequest::FileSave {
        id,
        suggested_name,
        mime,
        bytes,
    } = h.request_when_ready()
    else {
        panic!("PNG download request");
    };
    assert_eq!(
        (suggested_name.as_str(), mime.as_str(), bytes.as_ref()),
        ("scene.png", "image/png", png)
    );
    h.reply(id, HostValue::Done);
    let HostRequest::ClipboardWrite { id, items } = h.request_when_ready() else {
        panic!("PNG clipboard request");
    };
    assert_eq!(&*items, &[clipboard_item("image/png", png)]);
    h.reply(id, HostValue::Done);
    assert_eq!(h.eval("exported"), "true");

    h.eval(r#"
      globalThis.pasted = null;
      document.addEventListener('paste', e => {
        const transfer = e.clipboardData;
        const file = transfer.files.item(0);
        pasted = [e.isTrusted, transfer.types, file instanceof File, file.name, file.type, file.size,
          transfer.items[0].getAsFile() === file];
      });
    "#);
    h.key(
        "v",
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    let HostRequest::ClipboardRead { id } = h.request() else {
        panic!("paste request");
    };
    h.reply(id, HostValue::Clipboard(items));
    assert_eq!(
        h.eval("pasted"),
        serde_json::json!([
            true,
            ["Files"],
            true,
            "image.png",
            "image/png",
            png.len(),
            true
        ])
        .to_string()
    );
}

#[test]
fn file_activation_opens_owned_data_and_links_follow_their_scheme() {
    let mut h = Harness::new();
    h.eval(r#"
      globalThis.picker = document.createElement("input");
      picker.type = "file"; picker.accept = ".png, image/jpeg, "; picker.multiple = true;
      globalThis.pickerErrors = [];
      window.addEventListener('error', e => { pickerErrors.push(e.error.name); e.preventDefault(); });
      globalThis.pickerEvents = [];
      for (const type of ["change", "input", "cancel"]) {
        picker.addEventListener(type, event => pickerEvents.push([type,event.isTrusted,picker.files.length]));
      }
      picker.disabled = true; picker.click(); picker.disabled = false;
      picker.addEventListener("click", e => e.preventDefault(), {once:true}); picker.click();
    "#);
    assert!(h.host.try_recv().is_err());
    h.eval("picker.click(); picker.click()");
    let HostRequest::FileOpen {
        id,
        accept,
        multiple,
    } = h.request()
    else {
        panic!("open request");
    };
    assert_eq!(&*accept, &[".png", "image/jpeg"]);
    assert!(multiple);
    assert!(h.host.try_recv().is_err());
    h.reply(
        id,
        HostValue::Files(Box::new([FileHandle {
            name: "pic.png".into(),
            mime: "image/png".into(),
            last_modified_ms: 1000,
            bytes: Arc::from([4, 5, 6]),
        }])),
    );
    assert_eq!(h.eval(r#"[picker.isConnected, picker.files instanceof FileList, picker.files[0] instanceof File, picker.files.item(0).lastModified, picker.value, pickerEvents]"#),
        r#"[false,true,true,1000,"C:\\fakepath\\pic.png",[["input",true,1],["change",true,1]]]"#);
    h.eval("picker.click()");
    let HostRequest::FileOpen { id, .. } = h.request() else {
        panic!("second open request");
    };
    h.reply(id, HostValue::Cancelled);
    assert_eq!(h.eval("pickerEvents.at(-1)"), r#"["cancel",true,1]"#);
    assert_eq!(h.eval(r#"picker.value = ""; picker.files.length"#), "0");
    h.eval("picker.click()");
    let HostRequest::FileOpen { id, .. } = h.request() else {
        panic!("denied picker request");
    };
    h.runner
        .host_reply(
            id,
            Err(HostError {
                name: "NotAllowedError".into(),
                message: "Dialog unavailable".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        h.eval("[picker.files.length, pickerEvents.length, pickerErrors]"),
        r#"[0,3,["NotAllowedError"]]"#
    );
    h.eval("picker.click()");
    let HostRequest::FileOpen { id, .. } = h.request() else {
        panic!("retry after denial");
    };
    h.reply(id, HostValue::Cancelled);

    h.eval(
        r#"
      const blob = new Blob(["foo"], {type:"text/plain"});
      globalThis.link = document.createElement("a");
      link.download = "preview.txt";
      link.href = URL.createObjectURL(blob);
      link.click();
      URL.revokeObjectURL(link.href);
    "#,
    );
    let HostRequest::FileSave {
        id,
        suggested_name,
        mime,
        bytes,
    } = h.request()
    else {
        panic!("save request");
    };
    assert_eq!(
        (suggested_name.as_str(), mime.as_str(), bytes.as_ref()),
        ("preview.txt", "text/plain", b"foo".as_slice())
    );
    h.reply(id, HostValue::Done);
    assert_eq!(h.eval(r#"window.open("https://example.org/doc")"#), "null");
    let HostRequest::OpenExternal { id, url } = h.request() else {
        panic!("external request");
    };
    assert_eq!(url, "https://example.org/doc");
    h.reply(id, HostValue::Done);
    h.eval(r#"link.removeAttribute("download"); link.href = "https://example.org/link"; link.click();"#);
    assert!(
        matches!(h.request(), HostRequest::OpenExternal { url, .. } if url == "https://example.org/link")
    );
    h.eval(r#"link.href = "/separate"; link.target = "_blank"; link.click();"#);
    assert!(
        matches!(h.request(), HostRequest::OpenExternal { url, .. } if url.ends_with("/separate"))
    );
    assert_eq!(
        h.eval(r##"link.target = "_self"; link.href = "#chapter"; link.click(); location.hash"##),
        r##""#chapter""##
    );
    assert!(h.host.try_recv().is_err());
}

#[test]
fn images_accept_only_the_current_src_generation_even_when_detached() {
    let mut h = Harness::new();
    assert_eq!(h.eval(r#"
      globalThis.img = new Image(17, 19);
      globalThis.images = [];
      img.onload = e => images.push([e.type, img.complete, img.naturalWidth, img.naturalHeight, e.isTrusted]);
      img.onerror = e => images.push([e.type, img.complete, img.naturalWidth, e.isTrusted]);
      img.src = "data:image/png;base64,AA==";
      void img.decode().then(() => images.push("decoded data"));
      [img instanceof HTMLImageElement, img instanceof Image, img.width, img.height, img.complete, img.isConnected]
    "#), "[true,true,17,19,false,false]");
    h.runner.frame(tick(h.now, 1, 1)).unwrap();
    let id = h
        .journal
        .borrow()
        .records
        .iter()
        .find(|record| record.op == Opcode::CreateElement)
        .unwrap()
        .a;
    let node = NodeId::from_raw(id);
    h.runner.image_result(node, 1, Some([8, 9])).unwrap();
    assert_eq!(
        h.eval("images"),
        r#"[["load",true,8,9,true],"decoded data"]"#
    );
    h.eval(
        r#"
      img.src = URL.createObjectURL(new Blob([Uint8Array.of(1)],{type:"image/png"}));
      void img.decode().then(() => images.push("decoded blob"));
    "#,
    );
    h.runner.frame(tick(h.now, 2, 2)).unwrap();
    h.runner.image_result(node, 2, Some([4, 5])).unwrap();
    assert_eq!(
        h.eval("[img.complete, img.naturalWidth, images.at(-1)]"),
        r#"[true,4,"decoded blob"]"#
    );
    h.eval(
        r#"
      img.src = "/superseded.png";
      void img.decode().catch(e => images.push(e.name));
      img.src = "/packed.png";
      void img.decode().then(() => images.push("decoded pack"));
    "#,
    );
    h.runner.frame(tick(h.now, 3, 3)).unwrap();
    h.runner.image_result(node, 3, Some([1, 1])).unwrap();
    assert_eq!(
        h.eval("[img.complete, img.naturalWidth, images.at(-1)]"),
        r#"[false,0,"EncodingError"]"#
    );
    h.runner.image_result(node, 4, Some([100, 110])).unwrap();
    assert_eq!(
        h.eval("[img.currentSrc.endsWith('/packed.png'), img.naturalWidth, images.at(-1)]"),
        r#"[true,100,"decoded pack"]"#
    );
    h.eval(r#"img.src = "/missing.png"; void img.decode().catch(e => images.push(e.name));"#);
    h.runner.image_result(node, 5, None).unwrap();
    assert_eq!(
        h.eval("images.slice(-2)"),
        r#"[["error",true,0,true],"EncodingError"]"#
    );
}

#[test]
fn pending_decode_keeps_a_detached_image_alive_until_completion() {
    let mut h = Harness::new();
    h.eval(
        r#"
      globalThis.decodeStatus = "pending";
      void (async () => {
        const image = new Image();
        image.src = "/packed.png";
        await image.decode();
        decodeStatus = image.naturalWidth;
      })();
    "#,
    );
    h.runner.frame(tick(h.now, 1, 1)).unwrap();
    let id = h
        .journal
        .borrow()
        .records
        .iter()
        .find(|record| record.op == Opcode::CreateElement)
        .unwrap()
        .a;
    h.eval("gc()");
    h.runner
        .image_result(NodeId::from_raw(id), 1, Some([8, 9]))
        .unwrap();
    assert_eq!(h.eval("decodeStatus"), "8");
    h.eval("gc()");
    // The finalizer runs as a V8 platform task. A frame whose commit deadline
    // has already passed runs only one task, so give it a ready turn first.
    h.runner.run_ready().unwrap();
    h.runner.frame(tick(h.now, 2, 2)).unwrap();
    assert!(
        h.journal
            .borrow()
            .records
            .iter()
            .any(|record| record.op == Opcode::Release && record.a == id)
    );
}

#[test]
fn caret_movement_and_undo_redo_are_cancellable_editor_defaults() {
    let mut h = Harness::new();
    let plain = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..plain
    };
    let shift = Modifiers {
        shift: true,
        ..plain
    };
    h.eval(
        r#"
      globalThis.text = document.createElement("textarea");
      document.body.appendChild(text); text.value = "ab\ncd\nef"; text.focus();
      text.setSelectionRange(4, 4);
      globalThis.edits = [];
      text.onbeforeinput = e => edits.push(["beforeinput",e.inputType,e.cancelable]);
      text.oninput = e => edits.push(["input", e.inputType,e.cancelable]);
    "#,
    );
    for (key, modifiers, expected) in [
        ("Home", plain, "[3,3]"),
        ("ArrowUp", plain, "[0,0]"),
        ("ArrowDown", plain, "[3,3]"),
        ("ArrowRight", shift, "[3,4]"),
        ("ArrowLeft", plain, "[3,3]"),
        ("End", plain, "[5,5]"),
        ("End", ctrl, "[8,8]"),
        ("Home", ctrl, "[0,0]"),
    ] {
        h.key(key, modifiers);
        assert_eq!(
            h.eval("[text.selectionStart,text.selectionEnd]"),
            expected,
            "{key}"
        );
    }
    h.eval("text.value = ''; edits.length = 0");
    h.key("x", plain);
    h.key("y", plain);
    h.eval(r#"text.addEventListener("beforeinput", e => e.preventDefault(), {once:true});"#);
    assert!(h.key("z", ctrl));
    assert_eq!(h.eval("text.value"), r#""xy""#);
    h.key("z", ctrl);
    assert_eq!(h.eval("text.value"), r#""""#);
    h.key(
        "Z",
        Modifiers {
            shift: true,
            ..ctrl
        },
    );
    assert_eq!(h.eval("text.value"), r#""xy""#);
    h.key("z", ctrl);
    h.key("y", ctrl);
    assert_eq!(
        h.eval(r#"[text.value, edits.slice(-8)]"#),
        r#"["xy",[["beforeinput","historyUndo",true],["input","historyUndo",false],["beforeinput","historyRedo",true],["input","historyRedo",false],["beforeinput","historyUndo",true],["input","historyUndo",false],["beforeinput","historyRedo",true],["input","historyRedo",false]]]"#
    );
}

#[test]
fn form_data_and_named_controls_cover_live_entries_and_submitters() {
    let mut h = Harness::new();
    assert_eq!(h.eval(r#"
      globalThis.form = document.createElement("form"); form.id = "form";
      const tag = document.createElement("input"); tag.name="tag"; tag.value="first";
      const a = document.createElement("input"), b = document.createElement("input");
      for (const input of [a,b]) { input.type = "radio"; input.name = "choice"; }
      a.value = "a"; b.value = "b"; a.checked = true;
      const button = document.createElement("button"); button.name = "action"; button.value = "publish";
      const ignored = document.createElement("input"); ignored.name="disabled"; ignored.disabled = true;
      form.append(tag, a, b, button, ignored); document.body.append(form);
      const external = document.createElement("input");
      external.setAttribute("form", form.id); external.id = "outside"; external.name = "tag"; external.value = "last";
      document.body.appendChild(external);
      const controls = form.elements; const two = controls.tag;
      const initial = [controls === form.elements, controls.length, controls.item(0)===tag,
        two instanceof RadioNodeList, form.tag[1] === external, controls.namedItem("outside")===external,
        controls.namedItem("missing")];
      form.choice.value = "b";
      form.addEventListener("formdata", e => e.formData.append("hook","ran"));
      const onSubmit = new FormData(form,button);
      const custom = new FormData(form);
      const expected = [[...onSubmit],[...custom]];
      external.remove();
      [initial, expected, two.length, form.tag===tag, form.elements.tag===tag]
    "#), r#"[[true,6,true,true,true,true,null],[[["tag","first"],["choice","b"],["action","publish"],["tag","last"],["hook","ran"]],[["tag","first"],["choice","b"],["tag","last"],["hook","ran"]]],1,true,true]"#);
}

#[test]
fn title_session_storage_base64_and_css_namespace_are_available_in_v8() {
    let mut h = Harness::new();
    assert_eq!(h.eval(r##"
      const emptyTitle = document.title;
      document.title = "  a title\nwith whitespace\t ";
      localStorage.setItem("key", "local"); sessionStorage.key = "session";
      const pair = [localStorage.getItem("key"), sessionStorage.getItem("key"), "key" in sessionStorage];
      sessionStorage.clear();
      [emptyTitle, document.title, document.getElementsByTagName("title")[0].textContent,
        pair, sessionStorage.length, localStorage.length, atob("  AOk=\n").charCodeAt(1),
        btoa("\x00é"), CSS.supports("display","grid"), CSS.supports("display","impossible"),
        CSS.supports("not (display: impossible)"), CSS.supports("selector(:has(*))"),
        CSS.escape("1 #é\0"), CSS.escape("-"), CSS.escape("-1")]
    "##), r##"["","a title with whitespace","  a title\nwith whitespace\t ",["local","session",true],0,1,233,"AOk=",true,false,true,true,"\\31 \\ \\#é�","\\-","-\\31 "]"##);
    let mut next = Harness::new();
    assert_eq!(next.eval("sessionStorage.length"), "0");
}

#[test]
fn asynchronous_wasm_instantiation_is_serviced_without_frames() {
    let mut h = Harness::new();
    h.eval(
        r#"
        const bytes = Uint8Array.of(0, 97, 115, 109, 1, 0, 0, 0);
        void WebAssembly.compile(bytes).then(WebAssembly.instantiate)
          .then(result => { globalThis.wasmResult = result instanceof WebAssembly.Instance; });
    "#,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while h.eval("globalThis.wasmResult ?? null") == "null" {
        assert!(
            Instant::now() < deadline,
            "V8 did not complete WebAssembly compile"
        );
        h.runner.run_ready().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.eval("wasmResult"), "true");
}

#[test]
fn detached_subtree_releases_parent_before_children_and_reuses_slots_after_flush() {
    let mut h = Harness::new();
    h.eval(
        r#"
        (() => {
          const parent = document.createElement("section"); parent.id = "gone";
          globalThis.retained = document.createElement("span"); retained.id = "child";
          parent.appendChild(retained); document.body.appendChild(parent); parent.remove();
        })();
    "#,
    );
    h.runner.frame(tick(h.now, 1, 1)).unwrap();
    let (parent, child) = {
        let journal = h.journal.borrow();
        (journal.ids["gone"], journal.ids["child"])
    };
    h.eval("gc()");
    h.runner.run_ready().unwrap();
    h.runner.frame(tick(h.now, 2, 2)).unwrap();
    assert!(
        !h.journal
            .borrow()
            .records
            .iter()
            .any(|r| r.op == Opcode::Release)
    );
    h.eval("globalThis.retained = null; gc()");
    h.runner.run_ready().unwrap();
    h.runner.frame(tick(h.now, 3, 3)).unwrap();
    let released: Vec<_> = h
        .journal
        .borrow()
        .records
        .iter()
        .filter(|r| r.op == Opcode::Release)
        .map(|r| r.a)
        .collect();
    assert_eq!(released, [parent.raw(), child.raw()]);
    h.eval(r#"const next = document.createElement("span"); next.id = "fresh"; document.body.appendChild(next);"#);
    h.runner.frame(tick(h.now, 4, 4)).unwrap();
    let fresh = h.journal.borrow().ids["fresh"];
    assert_eq!(fresh.slot(), child.slot());
    assert_eq!(fresh.generation(), child.generation() + 1);
}

impl Harness {
    fn fullscreen_request(&self) -> (u32, bool) {
        match self.request() {
            HostRequest::Fullscreen { id, enter } => (id, enter),
            other => panic!("expected a fullscreen request, got {other:?}"),
        }
    }

    /// Runs task turns until no posted task is left, for checks that
    /// nothing more happens.
    fn settle(&mut self) {
        for _ in 0..4 {
            self.runner.run_ready().unwrap();
        }
    }

    /// Runs task turns until `log` holds `count` entries, then takes them.
    fn take_log(&mut self, count: usize) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.eval("log.length") != count.to_string() {
            assert!(
                Instant::now() < deadline,
                "log did not reach {count}: {}",
                self.eval("log")
            );
            self.runner.run_ready().unwrap();
        }
        self.eval("log.splice(0)")
    }
}

// Order and rejections follow the pinned Chromium 159 probe: the promise
// settles first, then `fullscreenchange` fires at <html> and bubbles to the
// document and window; a request without user activation rejects with
// TypeError, then fires `fullscreenerror`.
#[test]
fn fullscreen_requests_the_host_and_fires_change_events() {
    let mut h = Harness::new();
    assert_eq!(
        h.eval(
            r#"
        globalThis.log = [];
        const name = e => e.target.nodeName ?? "document";
        for (const type of ["fullscreenchange", "fullscreenerror"]) {
          document.addEventListener(type, e => log.push(`${type}@${name(e)}:${e.bubbles}:${e.composed}:${e.cancelable}:${e.isTrusted}:${document.fullscreenElement?.nodeName ?? null}`));
          window.addEventListener(type, () => log.push(`window:${type}`));
        }
        globalThis.track = (label, promise) => promise.then(
          () => log.push(`${label}:resolved:${document.fullscreenElement?.nodeName ?? null}`),
          e => log.push(`${label}:${e.constructor.name}:${e.message}`));
        document.addEventListener("keydown", () => track("enter", document.documentElement.requestFullscreen()));
        [document.fullscreenEnabled, document.fullscreenElement, document.fullscreen,
         typeof Element.prototype.requestFullscreen, "onfullscreenchange" in document]
    "#
        ),
        r#"[true,null,false,"function",true]"#
    );

    // Without user activation, the request never reaches the host.
    h.eval(r#"track("no-gesture", document.documentElement.requestFullscreen())"#);
    assert_eq!(
        h.take_log(3),
        r#"["no-gesture:TypeError:Permissions check failed","fullscreenerror@HTML:true:true:false:true:null","window:fullscreenerror"]"#
    );
    assert!(h.host.try_recv().is_err());

    // Exiting while not fullscreen rejects as Chromium does.
    h.eval(r#"track("exit", document.exitFullscreen())"#);
    assert_eq!(
        h.take_log(1),
        r#"["exit:TypeError:Failed to execute 'exitFullscreen' on 'Document': Document not active"]"#
    );
    assert!(h.host.try_recv().is_err());

    // Only the document element may be fullscreen in v1, even with a gesture.
    h.eval(
        r#"
        document.addEventListener("keydown", () => {
          if (globalThis.divRequest) track("div", document.body.requestFullscreen());
        });
        globalThis.divRequest = true;
    "#,
    );
    h.key("d", Modifiers::default());
    let (id, enter) = h.fullscreen_request();
    assert!(enter);
    h.eval("divRequest = false");
    assert_eq!(
        h.take_log(3),
        r#"["div:TypeError:Only the document element can be fullscreen in Strange Quark","fullscreenerror@BODY:true:true:false:true:null","window:fullscreenerror"]"#
    );
    assert!(h.host.try_recv().is_err());

    // A trusted keydown grants activation; the state changes on the host's reply.
    assert_eq!(h.eval("document.fullscreenElement"), "null");
    h.reply(id, HostValue::Done);
    assert_eq!(
        h.eval("[document.fullscreenElement.nodeName, document.fullscreen, log.slice()]"),
        r#"["HTML",true,["enter:resolved:HTML"]]"#
    );
    assert_eq!(
        h.take_log(3),
        r#"["enter:resolved:HTML","fullscreenchange@HTML:true:true:false:true:HTML","window:fullscreenchange"]"#
    );

    // Activation is consumed: a second request needs a new gesture. With one,
    // a request while already fullscreen resolves with no host round trip.
    h.eval(r#"track("again", document.documentElement.requestFullscreen())"#);
    assert_eq!(
        h.take_log(3),
        r#"["again:TypeError:Permissions check failed","fullscreenerror@HTML:true:true:false:true:HTML","window:fullscreenerror"]"#
    );
    h.key("a", Modifiers::default());
    assert_eq!(h.take_log(1), r#"["enter:resolved:HTML"]"#);
    h.settle();
    assert_eq!(h.eval("log.splice(0)"), "[]");
    assert!(h.host.try_recv().is_err());

    // Two exits in one task both resolve, with one change event.
    h.eval(
        r#"track("exit1", document.exitFullscreen()); track("exit2", document.exitFullscreen());"#,
    );
    let (first, enter) = h.fullscreen_request();
    assert!(!enter);
    let (second, _) = h.fullscreen_request();
    h.reply(first, HostValue::Done);
    h.reply(second, HostValue::Done);
    assert_eq!(
        h.take_log(4),
        r#"["exit1:resolved:null","exit2:resolved:null","fullscreenchange@HTML:true:true:false:true:null","window:fullscreenchange"]"#
    );
    h.settle();
    assert_eq!(h.eval("[document.fullscreenElement, log]"), "[null,[]]");

    // A host failure rejects the promise and fires fullscreenerror.
    h.key("e", Modifiers::default());
    let (id, _) = h.fullscreen_request();
    h.runner
        .host_reply(
            id,
            Err(HostError {
                name: "WindowError".into(),
                message: "window closed".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        h.take_log(3),
        r#"["enter:TypeError:window closed","fullscreenerror@HTML:true:true:false:true:null","window:fullscreenerror"]"#
    );
    assert_eq!(h.eval("document.fullscreenElement"), "null");

    // The window system can leave fullscreen on its own (a host task).
    h.key("f", Modifiers::default());
    let (id, _) = h.fullscreen_request();
    h.reply(id, HostValue::Done);
    h.take_log(3);
    h.runner.fullscreen_changed(false).unwrap();
    assert_eq!(h.eval("document.fullscreenElement"), "null");
    assert_eq!(
        h.take_log(2),
        r#"["fullscreenchange@HTML:true:true:false:true:null","window:fullscreenchange"]"#
    );
    // A repeated host state is not a change.
    h.runner.fullscreen_changed(false).unwrap();
    h.settle();
    assert_eq!(h.eval("log.splice(0)"), "[]");
}

#[test]
fn fullscreen_is_absent_without_a_host() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap();
    assert_eq!(
        runner
            .eval(r#"["requestFullscreen" in Element.prototype, "fullscreenEnabled" in document, "exitFullscreen" in document]"#)
            .unwrap(),
        "[false,false,false]"
    );
    runner.fullscreen_changed(true).unwrap();
}
