// SPDX-License-Identifier: MIT

use squark_types::host::{ImeKind, Input, InputTask, KeyKind, Modifiers, PointerKind, PointerType};

use super::*;

fn input(target: NodeId, time: Instant, request: u32, input: Input) -> InputTask {
    InputTask {
        request,
        target,
        related: NodeId::from_raw(0),
        time: time + Duration::from_millis(u64::from(request)),
        modifiers: Modifiers::default(),
        input,
    }
}

fn key_press(runner: &mut DocumentRunner<'_>, time: Instant, key: &str, shift: bool) -> bool {
    let mut task = input(
        BODY,
        time,
        9,
        Input::Key {
            kind: KeyKind::Down,
            key: key.into(),
            code: key.into(),
            location: 0,
            repeat: false,
            composing: false,
        },
    );
    task.modifiers.shift = shift;
    runner.input(&task).unwrap().default_prevented
}

fn ime(
    runner: &mut DocumentRunner<'_>,
    time: Instant,
    target: NodeId,
    kind: ImeKind,
    text: &str,
    selection_utf16: Option<[u32; 2]>,
) -> bool {
    runner
        .input(&input(
            target,
            time,
            10,
            Input::Ime {
                kind,
                text: text.into(),
                selection_utf16,
            },
        ))
        .unwrap()
        .default_prevented
}

#[test]
fn host_ime_updates_replace_preedit_and_dispatch_input_events_in_order() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let journal = engine.journal.clone();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(engine),
        Box::new(Paint(trace)),
        move || origin,
    )
    .unwrap();
    runner.eval(r#"
        globalThis.events = [];
        globalThis.editor = document.body.appendChild(document.createElement("input"));
        editor.id = "editor";
        editor.value = "aOLDz";
        editor.focus();
        editor.setSelectionRange(1, 4);
        for (const type of ["compositionstart","compositionupdate","beforeinput","input","compositionend"]) {
          editor.addEventListener(type, e => events.push([e.type, e.data, e.inputType ?? null, e.isComposing ?? null,
            e.cancelable, editor.value, editor.selectionStart, editor.selectionEnd]));
        }
    "#).unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let target = journal.borrow().ids["editor"];
    assert!(!ime(&mut runner, origin, target, ImeKind::Start, "", None));
    assert!(!ime(
        &mut runner,
        origin,
        target,
        ImeKind::Update,
        "💡",
        Some([1, 2])
    ));
    runner.frame(tick(origin, 2, 16)).unwrap();
    {
        let log = journal.borrow();
        let edits = log
            .editing
            .iter()
            .filter(|(record, _)| record.a == target.raw())
            .collect::<Vec<_>>();
        assert_eq!(
            edits
                .iter()
                .filter(|(record, _)| record.op == Opcode::SetValue)
                .count(),
            1
        );
        assert_eq!(
            (
                edits[edits.len() - 2].0.op,
                edits[edits.len() - 2].0.b,
                edits[edits.len() - 2].0.c
            ),
            (Opcode::SetSelection, 1, 4)
        );
        let (preedit, text) = edits.last().unwrap();
        assert_eq!(
            (preedit.op, preedit.c, text.as_deref()),
            (Opcode::SetPreedit, 2, Some(&[0xd83d, 0xdca1][..]))
        );
    }
    assert!(!ime(&mut runner, origin, target, ImeKind::Update, "", None));
    assert!(!ime(
        &mut runner,
        origin,
        target,
        ImeKind::Commit,
        "字",
        None
    ));
    assert!(!ime(&mut runner, origin, target, ImeKind::End, "", None));
    runner.frame(tick(origin, 3, 32)).unwrap();
    {
        let log = journal.borrow();
        let edits = &log.editing;
        let commit = &edits[edits.len() - 3..];
        assert_eq!(commit[0].0.op, Opcode::SetPreedit);
        assert_eq!(commit[0].1, Some(vec![]));
        assert_eq!(commit[1].0.op, Opcode::SetValue);
        assert_eq!(commit[1].1, Some("a字z".encode_utf16().collect()));
        assert_eq!(
            (commit[2].0.op, commit[2].0.b, commit[2].0.c),
            (Opcode::SetSelection, 2, 2)
        );
    }
    assert_eq!(
        runner.eval("events").unwrap(),
        r#"[
        ["compositionstart","OLD",null,null,true,"aOLDz",1,4],
        ["compositionupdate","💡",null,null,true,"aOLDz",1,4],
        ["beforeinput","💡","insertCompositionText",true,false,"aOLDz",1,4],
        ["input","💡","insertCompositionText",true,false,"a💡z",2,3],
        ["compositionupdate","",null,null,true,"a💡z",1,3],
        ["beforeinput","","insertCompositionText",true,false,"a💡z",1,3],
        ["input",null,"insertCompositionText",true,false,"az",1,1],
        ["compositionupdate","字",null,null,true,"az",1,1],
        ["beforeinput","字","insertCompositionText",true,false,"az",1,1],
        ["input","字","insertCompositionText",true,false,"a字z",2,2],
        ["compositionend","字",null,null,true,"a字z",2,2]
    ]"#
        .split_whitespace()
        .collect::<String>()
    );

    runner
        .eval(
            r#"
        events.length = 0;
        editor.addEventListener("beforeinput", e => e.preventDefault(), { once: true });
    "#,
        )
        .unwrap();
    assert!(ime(
        &mut runner,
        origin,
        target,
        ImeKind::Commit,
        "cancel",
        None
    ));
    assert_eq!(runner.eval("editor.value").unwrap(), "\"a字z\"");
    ime(
        &mut runner,
        origin,
        target,
        ImeKind::Update,
        "preedit",
        None,
    );
    key_press(&mut runner, origin, "Escape", false);
    assert_eq!(
        runner
            .eval("[editor.value, events.at(-1)[0], events.at(-1)[1]]")
            .unwrap(),
        r#"["a字z","compositionend",""]"#
    );

    ime(&mut runner, origin, target, ImeKind::Update, "keep", None);
    runner
        .eval(
            r#"
        globalThis.other = document.body.appendChild(document.createElement("input"));
        other.focus();
    "#,
        )
        .unwrap();
    ime(&mut runner, origin, target, ImeKind::Commit, "stale", None);
    assert_eq!(
        runner
            .eval("[editor.value, other.value, events.at(-1)[0]]")
            .unwrap(),
        r#"["a字keepz","","compositionend"]"#
    );
}

fn real_runner<'a>(
    pack: &'a Arc<Pack>,
    bundle: Option<Vec<u8>>,
    origin: Instant,
) -> (
    DocumentRunner<'a>,
    mpsc::Receiver<squark_types::render::ResourceMessage>,
) {
    support::initialize_v8();
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, receiver) = resource_channel();
    let painter = Paint(Trace::default());
    let lookup = pack.clone();
    let engine = StaticDocument::from_pack(pack, URL.parse().unwrap(), resources).unwrap();
    let runner = DocumentRunner::new(
        options(origin, true),
        bytes,
        sources(move |path| {
            if let Some(bytes) = bundle.as_ref().filter(|_| path == "/radix-editing.js") {
                return Some(("text/javascript".into(), bytes.clone()));
            }
            let asset = lookup.entry(path)?;
            Some((asset.mime.into(), asset.bytes.to_vec()))
        }),
        Box::new(engine),
        Box::new(painter),
        move || origin,
    )
    .unwrap();
    (runner, receiver)
}

fn fixture_pack() -> Arc<Pack> {
    Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    )
}

fn get_node(runner: &mut DocumentRunner<'_>, pack: &Pack, id: &str) -> NodeId {
    let root = DocumentRunner::document_id(pack.entry(DOCUMENT_PATH).unwrap().bytes).unwrap();
    let value = runner
        .eval(&format!(
            "__squark.query(5, {}, {{selector: '#{id}', mode: 'first'}})",
            root.raw()
        ))
        .unwrap();
    NodeId::from_raw(value.parse().unwrap())
}

#[test]
fn radix_traps_focus_in_a_portal_and_dismisses_on_escape_and_outside_pointer() {
    let bundle = crate::timeout::output(
        esbuild()
            .arg(key("crates/squark-js/tests/fixtures/radix-editing.mjs"))
            .args([
                "--bundle",
                "--platform=browser",
                "--format=esm",
                "--define:process.env.NODE_ENV=\"production\"",
            ]),
        Duration::from_secs(30),
    )
    .expect("run esbuild for the Radix fixture");
    assert!(
        bundle.status.success(),
        "Run npm install first. {}",
        String::from_utf8_lossy(&bundle.stderr)
    );
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, Some(bundle.stdout), origin);
    runner
        .eval(
            r#"
        import("/radix-editing.js").catch(error => { globalThis.importError = String(error); });
        null
    "#,
        )
        .unwrap();
    assert_eq!(runner.eval("globalThis.importError").unwrap(), "null");
    runner
        .eval("document.getElementById('trigger').focus(); openRadix()")
        .unwrap();
    runner.run_ready().unwrap();
    assert_eq!(runner.eval(r#"
        [document.activeElement.id, document.getElementById("scope").parentNode.id, getSelection().toString()]
    "#).unwrap(), r#"["first","portal","modal"]"#);
    assert!(!key_press(&mut runner, origin, "Tab", false));
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"last\""
    );
    assert!(key_press(&mut runner, origin, "Tab", false));
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"first\""
    );
    assert!(key_press(&mut runner, origin, "Tab", true));
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"last\""
    );

    runner
        .eval("document.getElementById('outside').focus()")
        .unwrap();
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"last\""
    );
    runner
        .eval("document.getElementById('first').focus(); document.activeElement.select()")
        .unwrap();
    runner.frame(tick(origin, 1, 1)).unwrap();
    let field = get_node(&mut runner, &pack, "first");
    ime(
        &mut runner,
        origin,
        field,
        ImeKind::Update,
        "に",
        Some([1, 1]),
    );
    ime(&mut runner, origin, field, ImeKind::Commit, "日本", None);
    assert_eq!(
        runner
            .eval(
                r#"[
        document.getElementById("echo").textContent, document.activeElement.value,
        radixEvents.slice(-3)
    ]"#
            )
            .unwrap(),
        r#"["日本","日本",[["compositionupdate","に"],["compositionupdate","日本"],["compositionend","日本"]]]"#
    );
    runner.eval("removeFocusedField()").unwrap();
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"scope\""
    );

    assert!(key_press(&mut runner, origin, "Escape", false));
    runner.run_ready().unwrap();
    assert_eq!(runner.eval("[document.activeElement.id, document.getElementById('layer'), radixEvents.slice(-2)]").unwrap(),
        r#"["trigger",null,[["escape","scope"],["dismiss"]]]"#);
    runner.eval("openRadix()").unwrap();
    runner.run_ready().unwrap();
    runner.frame(tick(origin, 2, 2)).unwrap();
    let outside = get_node(&mut runner, &pack, "outside");
    runner
        .input(&input(
            outside,
            origin,
            11,
            Input::Pointer {
                kind: PointerKind::Down,
                boundary_update: false,
                click_count: None,
                id: 1,
                device: PointerType::Mouse,
                primary: true,
                css: Point { x: 1., y: 1. },
                pressure: 0.5,
                button: 0,
                buttons: 1,
                samples: Box::new([]),
            },
        ))
        .unwrap();
    runner.run_ready().unwrap();
    assert_eq!(
        runner
            .eval(r#"[document.getElementById("layer"), radixEvents.slice(-2)]"#)
            .unwrap(),
        r#"[null,[["outside","outside"],["dismiss"]]]"#
    );
}

#[test]
fn line_breaks_expose_the_interface_used_by_editor_shortcuts() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    assert_eq!(
        runner
            .eval(
                r#"
        (() => {
          const editor = document.createElement("div");
          editor.innerHTML = "<br clear='left'><textarea></textarea>";
          const lineBreak = editor.firstChild;
          const created = document.createElement("br");
          const initial = created.clear;
          created.clear = "all";
          const svg = document.createElementNS("http://www.w3.org/2000/svg", "br");
          return [lineBreak instanceof HTMLBRElement, lineBreak.clear,
            created instanceof HTMLBRElement, initial, created.getAttribute("clear"),
            editor.lastChild instanceof HTMLBRElement, svg instanceof HTMLBRElement];
        })()
        "#
            )
            .unwrap(),
        r#"[true,"left",true,"","all",false,false]"#
    );
}

#[test]
fn ime_edits_a_transformed_textarea_and_marks_shortcuts_as_composing() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    runner.eval(r#"
        globalThis.editor = document.body.appendChild(document.createElement("textarea"));
        editor.id = "editor";
        editor.style.cssText = "position:absolute; left:15px; top:10px; width:96px; height:30px; border:0; padding:0; transform-origin:0 0; transform:translate(9px,6px) scale(1.25)";
        editor.value = "Edit text";
        editor.focus();
        editor.setSelectionRange(5, 9);
        globalThis.actions = [];
        editor.addEventListener("keydown", e => {
          if (e.ctrlKey && e.key === "Enter") {
            e.preventDefault();
            actions.push([e.isComposing, e.keyCode]);
          }
        });
        editor.addEventListener("input", e => actions.push([e.inputType, e.data, editor.value]));
    "#).unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let node = get_node(&mut runner, &pack, "editor");
    assert_eq!(
        runner
            .eval(
                r#"
            (() => {
              const r = editor.getBoundingClientRect();
              return [document.activeElement === editor, r.x, r.y, r.width, r.height];
            })()
        "#
            )
            .unwrap(),
        "[true,24,16,120,37.5]"
    );
    ime(
        &mut runner,
        origin,
        node,
        ImeKind::Update,
        "に",
        Some([1, 1]),
    );
    let mut submit = input(
        node,
        origin,
        12,
        Input::Key {
            kind: KeyKind::Down,
            key: "Enter".into(),
            code: "Enter".into(),
            location: 0,
            repeat: false,
            composing: false,
        },
    );
    submit.modifiers.ctrl = true;
    assert!(runner.input(&submit).unwrap().default_prevented);
    ime(&mut runner, origin, node, ImeKind::Commit, "日本", None);
    assert!(runner.input(&submit).unwrap().default_prevented);
    assert_eq!(
        runner
            .eval("[editor.value, editor.selectionStart, editor.selectionEnd, actions]")
            .unwrap(),
        r#"["Edit 日本",7,7,[["insertCompositionText","に","Edit に"],[true,229],["insertCompositionText","日本","Edit 日本"],[false,13]]]"#
    );
}

#[test]
fn computed_style_and_svg_geometry_read_the_real_engine() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    assert_eq!(runner.eval(r#"
        const box = document.createElement("div");
        document.body.append(box);
        box.style.display = "none";
        const computed = getComputedStyle(box);
        const before = computed.display;
        box.style.display = "block";
        box.style.color = "rgb(0, 128, 0)";
        let readonly = false;
        try { computed.display = "none"; } catch (e) { readonly = e.name === "NoModificationAllowedError"; }

        const ns = "http://www.w3.org/2000/svg";
        const svg = document.body.appendChild(document.createElementNS(ns, "svg"));
        const rect = svg.appendChild(document.createElementNS(ns, "rect"));
        rect.setAttribute("x", "4");
        rect.setAttribute("y", "7");
        rect.setAttribute("width", "10");
        rect.setAttribute("height", "20");
        const text = svg.appendChild(document.createElementNS(ns, "text"));
        text.textContent = "squark";
        const bbox = rect.getBBox();
        [before, computed.display, computed.getPropertyValue("color"), readonly,
         svg instanceof SVGSVGElement, rect instanceof SVGGraphicsElement,
         bbox.x, bbox.y, bbox.width, bbox.height, text.getComputedTextLength() > 0]
    "#).unwrap(), r#"["none","block","rgb(0, 128, 0)",true,true,true,4,7,10,20,true]"#);
}

#[test]
fn tab_uses_positive_order_then_document_order_and_escape_clears_search() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    runner
        .eval(
            r#"
      const container = document.body.appendChild(document.createElement("section"));
      function make(tag, id, index = null) {
        const node = container.appendChild(document.createElement(tag));
        node.id = id;
        if (index !== null) node.tabIndex = index;
        return node;
      }
      make("button","natural");
      make("button","second",2);
      make("button","first",1);
      globalThis.programmatic = make("span","programmatic",-1);
      make("button","hidden").style.visibility = "hidden";
      make("button","disabled").disabled = true;
      globalThis.search = make("input","search");
      search.type = "search";
      search.value = "query";
      globalThis.events = [];
      search.addEventListener("search", e => events.push([e.isTrusted, search.value]));
    "#,
        )
        .unwrap();
    for expected in ["first", "second", "natural", "search"] {
        key_press(&mut runner, origin, "Tab", false);
        assert_eq!(
            runner.eval("document.activeElement.id").unwrap(),
            format!("\"{expected}\"")
        );
    }
    key_press(&mut runner, origin, "Escape", false);
    assert_eq!(runner.eval("events").unwrap(), r#"[[true,""]]"#);
    runner.eval("programmatic.focus()").unwrap();
    key_press(&mut runner, origin, "Tab", false);
    assert_eq!(
        runner.eval("document.activeElement.id").unwrap(),
        "\"search\""
    );
}

fn sample(owner: NodeId, left: f32, top: f32, generation: u64) -> ScrollOffset {
    ScrollOffset {
        owner,
        offset: Point { x: left, y: top },
        generation,
    }
}

#[test]
fn scroll_sync_rejects_old_generations_and_dispatches_once_before_raf() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let reads = engine.scroll_reads.clone();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(engine),
        Box::new(Paint(trace)),
        move || origin,
    )
    .unwrap();
    runner.eval(r#"
        globalThis.events = [];
        globalThis.pane = document.body;
        pane.addEventListener("scroll", e => events.push([e.type, e.target === pane, pane.scrollLeft, pane.scrollTop]));
        addEventListener("scroll", e => events.push([e.type, e.target === document, scrollX, scrollY]));
        requestAnimationFrame(() => events.push(["raf", pane.scrollTop]));
    "#).unwrap();
    let (send, receive) = mpsc::channel();
    let (before_tx, before_rx) = mpsc::channel();
    let (after_tx, after_rx) = mpsc::channel();
    for (tick, offsets) in [
        (3, vec![sample(BODY, 3., 9., 3)]),
        (5, vec![sample(BODY, 7., 12., 5), sample(ROOT, 1., 8., 1)]),
        // A later frame timestamp cannot make an older owner generation new.
        (6, vec![sample(BODY, 20., 99., 4), sample(ROOT, 1., 8., 2)]),
    ] {
        send.send(DocumentTask::ScrollSync {
            frame: FrameId(1),
            tick,
            offsets: offsets.into_boxed_slice(),
        })
        .unwrap();
    }
    send.send(DocumentTask::Eval {
        script: "pane.getBoundingClientRect(); [pane.scrollLeft, pane.scrollTop, document.scrollingElement.scrollTop, events.length]".into(),
        reply: before_tx,
    }).unwrap();
    send.send(tick(origin, 2, 10).into()).unwrap();
    let mut send = Some(send);
    runner
        .run(receive, |_| {
            let send = send.take().unwrap();
            send.send(DocumentTask::Eval {
                script: "events".into(),
                reply: after_tx.clone(),
            })
            .unwrap();
        })
        .unwrap();
    assert_eq!(before_rx.try_recv().unwrap().unwrap(), "[7,12,8,0]");
    assert_eq!(
        after_rx.try_recv().unwrap().unwrap(),
        r#"[["scroll",true,7,12],["scroll",true,1,8],["raf",12]]"#
    );
    assert!(!reads.borrow().is_empty());
    assert!(
        reads
            .borrow()
            .iter()
            .all(|values| values == &[sample(ROOT, 1., 8., 2), sample(BODY, 7., 12., 5)])
    );
}

#[test]
fn document_scroll_reads_reconcile_commands_before_compositor_acknowledgement() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    runner.eval(r#"
        document.body.replaceChildren();
        document.body.style.margin = "0px";
        globalThis.pane = document.body.appendChild(document.createElement("div"));
        pane.id = "pane";
        pane.style.cssText = "position: relative; width: 120px; height: 60px; overflow: auto;";
        const filler = pane.appendChild(document.createElement("div"));
        filler.style.cssText = "width: 400px; height: 320px;";
        globalThis.target = filler.appendChild(document.createElement("div"));
        target.style.cssText = "position: absolute; left: 160px; top: 125px; width: 20px; height: 10px;";
    "#).unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let pane = get_node(&mut runner, &pack, "pane");
    assert_eq!(
        runner
            .eval("[target.getBoundingClientRect().x, target.getBoundingClientRect().y]")
            .unwrap(),
        "[160,125]"
    );
    runner
        .scroll_sync(FrameId(1), 2, &[sample(pane, 15., 20., 2)])
        .unwrap();
    assert_eq!(runner.eval("[pane.scrollLeft, pane.scrollTop, target.getBoundingClientRect().x, target.getBoundingClientRect().y]").unwrap(), "[15,20,145,105]");
    runner
        .eval(
            r#"
        pane.scrollTop = 40;
        pane.scrollLeft = 23;
        pane.scrollTo({top: 50});
        globalThis.scrollEvents = [];
        pane.addEventListener("scroll", () => scrollEvents.push(pane.scrollTop));
    "#,
        )
        .unwrap();
    assert_eq!(
        runner
            .eval("[target.getBoundingClientRect().x, target.getBoundingClientRect().y, pane.scrollLeft, pane.scrollTop]")
            .unwrap(),
        "[137,75,23,50]"
    );
    runner
        .scroll_sync(FrameId(2), 3, &[sample(pane, 90., 100., 3)])
        .unwrap();
    runner
        .eval("pane.scrollTo({top: 90}); target.scrollIntoView({block:'start', inline:'nearest', container:'nearest'})")
        .unwrap();
    assert_eq!(
        runner.eval("target.getBoundingClientRect().y").unwrap(),
        "0"
    );
    assert_eq!(
        runner.eval("pane.scrollTop = 9000; [pane.scrollTop === pane.scrollHeight - pane.clientHeight, pane.scrollLeft]").unwrap(),
        "[true,60]"
    );
    runner.frame(tick(origin, 2, 10)).unwrap();
    assert_eq!(
        runner
            .eval("[scrollEvents.length, scrollEvents[0] === pane.scrollTop]")
            .unwrap(),
        "[1,true]"
    );
}

#[test]
fn pointer_selects_text_using_the_engine_caret_geometry() {
    let pack = fixture_pack();
    let origin = Instant::now();
    let (mut runner, _resources) = real_runner(&pack, None, origin);
    runner.eval(r#"
        document.body.replaceChildren();
        globalThis.editor = document.body.appendChild(document.createElement("input"));
        editor.id = "editor";
        editor.style.cssText = "width: 180px; font: 16px monospace";
        editor.value = "abcdef";
        globalThis.selected = [];
        editor.onselect = () => selected.push([editor.selectionStart, editor.selectionEnd, editor.selectionDirection]);
    "#).unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let node = get_node(&mut runner, &pack, "editor");
    let caret = |runner: &mut DocumentRunner<'_>, offset: u32| {
        let coordinates = runner.eval(&format!(r#"
            (() => {{ const r = __squark.query(11, {}, {}); return [r.x + 0.1, r.y + r.height / 2]; }})()
        "#, node.raw(), offset)).unwrap();
        let [x, y]: [f32; 2] = serde_json::from_str(&coordinates).unwrap();
        Point { x, y }
    };
    let start = caret(&mut runner, 4);
    let end = caret(&mut runner, 1);
    let pointer = |kind, request, position, buttons| {
        input(
            node,
            origin,
            request,
            Input::Pointer {
                kind,
                boundary_update: false,
                click_count: None,
                id: 1,
                device: PointerType::Mouse,
                primary: true,
                css: position,
                pressure: 0.,
                button: if kind == PointerKind::Move { -1 } else { 0 },
                buttons,
                samples: Box::new([]),
            },
        )
    };
    runner
        .input(&pointer(PointerKind::Down, 1, start, 1))
        .unwrap();
    assert_eq!(
        runner
            .eval("[editor.selectionStart, editor.selectionEnd]")
            .unwrap(),
        "[4,4]"
    );
    runner
        .input(&pointer(PointerKind::Move, 2, end, 1))
        .unwrap();
    runner.input(&pointer(PointerKind::Up, 3, end, 0)).unwrap();
    runner.run_ready().unwrap();
    assert_eq!(
        runner
            .eval("[getSelection().toString(), selected]")
            .unwrap(),
        r#"["bcd",[[1,4,"backward"]]]"#
    );
}

#[test]
fn text_rect_and_caret_queries_hit_test_through_the_painter() {
    support::initialize_v8();
    let pack = fixture_pack();
    let origin = Instant::now();
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, _receiver) = resource_channel();
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources.clone()).unwrap();
    let painter = Painter::new(Arc::new(FrameMailbox::default()), resources);
    let lookup = pack.clone();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        bytes,
        sources(move |path| {
            let asset = lookup.entry(path)?;
            Some((asset.mime.into(), asset.bytes.to_vec()))
        }),
        Box::new(engine),
        Box::new(painter),
        move || origin,
    )
    .unwrap();
    runner
        .eval(
            r#"
        document.body.replaceChildren();
        document.body.style.cssText = "margin: 0; font: 16px/20px 'Liberation Mono', monospace";
        const line = document.body.appendChild(document.createElement("div"));
        line.id = "line";
        line.style.cssText = "position: absolute; left: 30px; top: 10px; width: 200px; white-space: pre";
        line.appendChild(document.createElement("span")).textContent = "const x";
        const cover = document.body.appendChild(document.createElement("div"));
        cover.id = "cover";
        cover.style.cssText = "position: absolute; left: 200px; top: 100px; width: 40px; height: 40px";
    "#,
        )
        .unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let line = get_node(&mut runner, &pack, "line");
    let cover = get_node(&mut runner, &pack, "cover");
    // The caret query names the Text node, which the rect query then measures.
    let caret: (u32, u32) = serde_json::from_str(
        &runner
            .eval("__squark.query(15, 1, [30 + 9.6 * 2.3, 20])")
            .unwrap(),
    )
    .unwrap();
    let text = caret.0;
    assert_eq!(caret.1, 2);
    assert!(text != line.raw() && text != cover.raw());
    let rects: Vec<[f32; 4]> = serde_json::from_str(
        &runner
            .eval(&format!(
                "[[0, 1], [0, 2], [2, 2]].map(range => __squark.query(14, {text}, range))
                    .map(([r]) => [r.x, r.y, r.width, r.height])"
            ))
            .unwrap(),
    )
    .unwrap();
    let [one, two, caret] = rects[..] else {
        unreachable!()
    };
    assert_eq!(one[0], 30.0);
    assert!((two[2] - 2.0 * one[2]).abs() < 0.01 && one[2] > 9.0);
    assert!(one[1] > 5.0 && one[1] < 15.0 && one[3] > 15.0);
    assert_eq!(caret, [30.0 + two[2], one[1], 0.0, one[3]]);
    assert_eq!(
        runner
            .eval(&format!(
                "[
                    __squark.query(15, 1, [220, 20]), __squark.query(15, 1, [220, 120]),
                    __squark.query(15, 1, [-5, 20]), __squark.query(14, {cover}, [0, 1]),
                ]",
                cover = cover.raw()
            ))
            .unwrap(),
        format!("[[{text},7],[{},0],null,[]]", cover.raw())
    );
    assert!(runner.eval("__squark.query(15, 1, [NaN, 20])").is_err());
}
