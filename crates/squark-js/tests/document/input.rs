// SPDX-License-Identifier: MIT

use std::sync::mpsc;

use squark_js::document::InputResult;
use squark_types::{
    host::{DeltaMode, Input, InputTask, KeyKind, Modifiers, PointerKind, PointerType},
    journal::ElementState,
};

use super::*;

const NONE: NodeId = NodeId::from_raw(0);

struct Fixture {
    runner: DocumentRunner<'static>,
    journal: Rc<RefCell<JournalLog>>,
    origin: Instant,
}

impl Fixture {
    fn new(scripts: &[(ScriptKind, &str, &str)], lookup: ScriptSources) -> Self {
        support::initialize_v8();
        let trace = Trace::default();
        let engine = Engine::new(trace.clone());
        let journal = engine.journal.clone();
        let origin = Instant::now();
        let runner = DocumentRunner::new(
            options(origin, true),
            &fixture(scripts),
            lookup,
            Box::new(engine),
            Box::new(Paint(trace)),
            move || origin,
        )
        .unwrap();
        Self {
            runner,
            journal,
            origin,
        }
    }

    fn script(script: &str) -> Self {
        let mut fixture = Self::new(&[], sources(|_| None));
        fixture.runner.eval(script).unwrap();
        fixture.runner.frame(tick(fixture.origin, 1, 0)).unwrap();
        fixture
    }

    fn node(&self, id: &str) -> NodeId {
        self.journal.borrow().ids[id]
    }

    fn eval(&mut self, script: &str) -> String {
        self.runner.eval(script).unwrap()
    }

    fn input(&mut self, task: InputTask) -> InputResult {
        self.runner.input(&task).unwrap()
    }

    fn click(&mut self, target: &str, time: u32) -> InputResult {
        let target = self.node(target);
        self.input(pointer(self.origin, target, PointerKind::Down, time));
        self.input(pointer(self.origin, target, PointerKind::Up, time + 1))
    }

    fn key(&mut self, key: &str, code: &str, time: u32) -> InputResult {
        self.input(key_task(self.origin, key, code, time))
    }
}

fn pointer(origin: Instant, target: NodeId, kind: PointerKind, request: u32) -> InputTask {
    let button = if matches!(kind, PointerKind::Down | PointerKind::Up) {
        0
    } else {
        -1
    };
    let buttons = u16::from(kind == PointerKind::Down);
    InputTask {
        request,
        target,
        related: NONE,
        time: origin + Duration::from_millis(request.into()),
        modifiers: Modifiers::default(),
        input: Input::Pointer {
            kind,
            boundary_update: false,
            click_count: None,
            id: 1,
            device: PointerType::Mouse,
            primary: true,
            css: Point {
                x: request as f32,
                y: 12.,
            },
            pressure: f32::from(buttons) * 0.5,
            button,
            buttons,
            samples: Box::new([]),
        },
    }
}

fn wheel(origin: Instant, target: NodeId, request: u32) -> InputTask {
    InputTask {
        request,
        target,
        related: NONE,
        time: origin + Duration::from_millis(request.into()),
        modifiers: Modifiers {
            ctrl: true,
            ..Default::default()
        },
        input: Input::Wheel {
            css: Point { x: 5., y: 8. },
            delta: [2., -5., 0.],
            mode: DeltaMode::Line,
        },
    }
}

fn key_task(origin: Instant, key: &str, code: &str, request: u32) -> InputTask {
    InputTask {
        request,
        target: BODY,
        related: NONE,
        time: origin + Duration::from_millis(request.into()),
        modifiers: Modifiers::default(),
        input: Input::Key {
            kind: KeyKind::Down,
            key: key.into(),
            code: code.into(),
            location: 0,
            repeat: false,
            composing: false,
        },
    }
}

#[test]
fn react_counter_and_controlled_text_input_receive_native_tasks() {
    let bundle = crate::timeout::output(
        esbuild()
            .arg(key("crates/squark-js/tests/fixtures/react-input.mjs"))
            .args([
                "--bundle",
                "--platform=browser",
                "--format=esm",
                "--define:process.env.NODE_ENV=\"production\"",
            ]),
        Duration::from_secs(30),
    )
    .expect("run esbuild for the React input fixture");
    assert!(
        bundle.status.success(),
        "Build the React fixture with npm install first. {}",
        String::from_utf8_lossy(&bundle.stderr)
    );
    let mut fixture = Fixture::new(
        &[(
            ScriptKind::ExternalModule,
            "/react-input.js",
            "/react-input.js",
        )],
        sources(move |path| {
            (path == "/react-input.js").then(|| ("text/javascript".into(), bundle.stdout.clone()))
        }),
    );
    fixture.runner.frame(tick(fixture.origin, 1, 0)).unwrap();
    assert_eq!(
        fixture.eval("document.getElementById('count').textContent"),
        "\"0\""
    );

    fixture.eval(r#"
        globalThis.latency = [];
        const count = document.getElementById("count");
        count.addEventListener("click", event => latency.push(["click", event.timeStamp]));
        new MutationObserver(() => requestAnimationFrame(time => latency.push(["raf", time, count.textContent])))
            .observe(count, { childList: true, characterData: true, subtree: true });
    "#);
    fixture.click("count", 1);
    assert_eq!(fixture.eval("latency"), r#"[["click",2]]"#);
    fixture.runner.frame(tick(fixture.origin, 2, 3)).unwrap();
    assert_eq!(fixture.eval("latency"), r#"[["click",2],["raf",3,"1"]]"#);
    fixture.click("count", 3);
    assert_eq!(
        fixture.eval("document.getElementById('count').textContent"),
        "\"2\""
    );
    assert_eq!(fixture.eval("document.activeElement.id"), "\"counter\"");

    fixture.click("editor", 5);
    assert!(!fixture.key("a", "KeyA", 7).default_prevented);
    assert!(!fixture.key("💡", "", 8).default_prevented);
    assert_eq!(fixture.eval("[document.activeElement.id, document.activeElement.value, document.getElementById('echo').textContent]"),
        r#"["editor","a💡","a💡"]"#);
    fixture.key("Backspace", "Backspace", 9);
    fixture.key("b", "KeyB", 10);
    assert_eq!(fixture.eval("[document.activeElement.value, document.getElementById('echo').textContent, document.activeElement.selectionStart]"),
        r#"["ab","ab",2]"#);
    fixture.runner.frame(tick(fixture.origin, 3, 16)).unwrap();
    assert!(fixture.journal.borrow().records.iter().any(|record| record.op == Opcode::SetValue && record.a == fixture.node("editor").raw()));
    // The submitter-specific React 19 action builds FormData using a temporary
    // hidden input. It needs a successful constructor to clean up after itself.
    fixture.click("publish", 17);
    fixture.click("publish", 19);
    assert_eq!(
        fixture.eval("[formActionData, document.getElementById('actions').getElementsByTagName('input').length]"),
        r#"[[["title","note"]],1]"#
    );
}

#[test]
fn zoom_keys_preserve_the_logical_key_physical_code_and_modifiers() {
    let mut fixture = Fixture::script(
        r#"
        globalThis.keys = [];
        for (const type of ["keydown", "keypress", "keyup"]) {
          document.addEventListener(type, e => keys.push([
            e.type, e.key, e.code, e.location, e.ctrlKey, e.shiftKey, e.keyCode, e.charCode
          ]));
        }
        "#,
    );
    for (code, location, unshifted, legacy) in [("Equal", 0, "=", 187), ("NumpadAdd", 3, "+", 107)]
    {
        for (ctrl, shift) in [(false, false), (true, false), (false, true), (true, true)] {
            let key = if shift { "+" } else { unshifted };
            let mut expected = Vec::new();
            for (kind, event) in [(KeyKind::Down, "keydown"), (KeyKind::Up, "keyup")] {
                fixture.input(InputTask {
                    request: 1,
                    target: NONE,
                    related: NONE,
                    time: fixture.origin,
                    modifiers: Modifiers {
                        ctrl,
                        shift,
                        ..Default::default()
                    },
                    input: Input::Key {
                        kind,
                        key: key.into(),
                        code: code.into(),
                        location,
                        repeat: false,
                        composing: false,
                    },
                });
                expected.push(serde_json::json!([
                    event, key, code, location, ctrl, shift, legacy, 0
                ]));
                if kind == KeyKind::Down && !ctrl {
                    let ch = key.as_bytes()[0];
                    expected.push(serde_json::json!([
                        "keypress", key, code, location, ctrl, shift, ch, ch
                    ]));
                }
            }
            assert_eq!(
                fixture.eval("keys.splice(0)"),
                serde_json::to_string(&expected).unwrap()
            );
        }
    }
}

#[test]
fn pointer_boundaries_capture_focus_and_mask_updates() {
    let mut fixture = Fixture::script(
        r#"
      globalThis.events = [];
      const panel = document.body.appendChild(document.createElement("section"));
      const button = panel.appendChild(document.createElement("button"));
      const other = panel.appendChild(document.createElement("button"));
      panel.id = "panel";
      button.id = "button";
      other.id = "other";
      for (const type of ["pointerover", "pointerout", "pointerdown", "mousedown", "focusin",
          "gotpointercapture", "pointermove", "pointerup", "mouseup", "lostpointercapture", "click"]) {
        panel.addEventListener(type, e => events.push([
          e.type, e.target.id, e.relatedTarget?.id ?? null, e.timeStamp, e.isTrusted, e.button ?? null, e.buttons ?? null
        ]));
      }
      button.addEventListener("pointerdown", e => {
        if (!(e instanceof PointerEvent) || e.x !== 4 || e.pressure !== 0.5) throw Error("pointer fields");
        button.setPointerCapture(e.pointerId);
      });
    "#,
    );
    let first = fixture.node("button");
    let second = fixture.node("other");
    fixture.input(pointer(fixture.origin, first, PointerKind::Move, 3));
    fixture.input(pointer(fixture.origin, first, PointerKind::Down, 4));
    assert_eq!(fixture.eval("document.activeElement.id"), "\"button\"");
    let mut moving = pointer(fixture.origin, second, PointerKind::Move, 5);
    if let Input::Pointer { buttons, .. } = &mut moving.input {
        *buttons = 1;
    }
    fixture.input(moving);
    fixture.input(pointer(fixture.origin, second, PointerKind::Up, 6));
    assert_eq!(fixture.eval("events"), r#"[
        ["pointerover","button",null,3,true,-1,0],["pointermove","button",null,3,true,-1,0],
        ["pointerdown","button",null,4,true,0,1],["mousedown","button",null,4,true,0,1],["focusin","button",null,4,true,null,null],
        ["gotpointercapture","button",null,5,true,-1,1],["pointermove","button",null,5,true,-1,1],
        ["pointerup","button",null,6,true,0,0],["mouseup","button",null,6,true,0,0],["lostpointercapture","button",null,6,true,0,0],
        ["click","button",null,6,true,0,0],["pointerout","button","other",6,true,0,0],["pointerover","other","button",6,true,0,0]
    ]"#.split_whitespace().collect::<String>());
    fixture.key("Tab", "Tab", 7);
    assert_eq!(fixture.eval("document.activeElement.id"), "\"other\"");
    fixture.runner.frame(tick(fixture.origin, 2, 16)).unwrap();

    let records = &fixture.journal.borrow().records;
    let state = |node: NodeId, mask: ElementState| {
        records
            .iter()
            .filter(|record| {
                record.op == Opcode::SetState
                    && record.a == node.raw()
                    && record.c & mask.bits() != 0
            })
            .map(|record| record.b & mask.bits() != 0)
            .collect::<Vec<_>>()
    };
    assert_eq!(state(first, ElementState::HOVER), [true, false]);
    assert_eq!(state(first, ElementState::ACTIVE), [true, false]);
    assert_eq!(state(first, ElementState::FOCUS), [true, false]);
    assert_eq!(
        state(fixture.node("panel"), ElementState::FOCUS_WITHIN),
        [true]
    );
    assert_eq!(state(second, ElementState::FOCUS_VISIBLE), [true]);
    assert_eq!(state(second, ElementState::HOVER), [true]);
}

#[test]
fn activation_cancellation_and_keyboard_defaults() {
    let mut fixture = Fixture::script(
        r#"
      globalThis.events = [];
      globalThis.form = document.body.appendChild(document.createElement("form"));
      function add(tag, id) {
        const element = form.appendChild(document.createElement(tag));
        element.id = id;
        return element;
      }
      globalThis.label = add("label", "label");
      globalThis.checkbox = add("input", "box");
      checkbox.type = "checkbox";
      label.htmlFor = "box";
      globalThis.radioA = add("input", "a");
      globalThis.radioB = add("input", "b");
      radioA.type = radioB.type = "radio";
      radioA.name = radioB.name = "choice";
      radioA.checked = true;
      globalThis.text = add("input", "text");
      globalThis.submit = add("button", "submit");
      form.addEventListener("input", e => events.push(["input", e.target.id, e.target.value]));
      form.addEventListener("change", e => events.push(["change", e.target.id, e.target.checked]));
      form.addEventListener("submit", e => { events.push(["submit", e.submitter.id, e.isTrusted]); e.preventDefault(); });
      checkbox.addEventListener("click", e => events.push(["click", e.target.id, checkbox.checked]));
      radioB.addEventListener("click", e => { events.push(["try", radioA.checked, radioB.checked]); e.preventDefault(); }, { once: true });
      text.addEventListener("beforeinput", e => {
        if (e.data === "x") { events.push([e.inputType, e.data, text.value]); e.preventDefault(); }
      });
    "#,
    );
    fixture.click("label", 1);
    assert_eq!(
        fixture.eval("[checkbox.checked, document.activeElement.id]"),
        r#"[true,"box"]"#
    );
    assert!(fixture.click("b", 3).default_prevented);
    assert_eq!(
        fixture.eval("[radioA.checked, radioB.checked]"),
        "[true,false]"
    );
    fixture.click("b", 5);
    assert_eq!(
        fixture.eval("[radioA.checked, radioB.checked]"),
        "[false,true]"
    );
    fixture.click("submit", 7);
    fixture.click("text", 9);
    fixture.key("k", "KeyK", 11);
    assert!(fixture.key("x", "KeyX", 12).default_prevented);
    fixture.key("Enter", "Enter", 13);
    assert_eq!(
        fixture.eval("events"),
        r#"[
        ["click","box",true],["input","box","on"],["change","box",true],
        ["try",false,true],["input","b","on"],["change","b",true],["submit","submit",true],
        ["input","text","k"],["insertText","x","k"],["submit","submit",true]
    ]"#
        .split_whitespace()
        .collect::<String>()
    );

    fixture.eval(
        r#"
        checkbox.addEventListener("mousedown", e => e.preventDefault(), { once: true });
        checkbox.indeterminate = true;
        checkbox.addEventListener("click", e => e.preventDefault(), { once: true });
    "#,
    );
    assert!(fixture.click("box", 14).default_prevented);
    assert_eq!(
        fixture.eval("[checkbox.checked, checkbox.indeterminate, document.activeElement.id]"),
        r#"[true,true,"text"]"#
    );
    fixture.eval(
        r#"
        checkbox.focus();
        checkbox.addEventListener("keydown", e => e.preventDefault(), { once: true });
    "#,
    );
    let origin = fixture.origin;
    let space_up = |time| {
        let mut task = key_task(origin, " ", "Space", time);
        if let Input::Key { kind, .. } = &mut task.input {
            *kind = KeyKind::Up;
        }
        task
    };
    assert!(fixture.key(" ", "Space", 16).default_prevented);
    fixture.input(space_up(17));
    assert_eq!(
        fixture.eval("[checkbox.checked, checkbox.indeterminate]"),
        "[true,true]"
    );
    fixture.key(" ", "Space", 18);
    fixture.input(space_up(19));
    assert_eq!(
        fixture.eval("[checkbox.checked, checkbox.indeterminate]"),
        "[false,false]"
    );
    fixture.eval("checkbox.checked = true; text.focus();");
    fixture.eval("checkbox.disabled = true; text.readOnly = true;");
    fixture.click("label", 20);
    fixture.key("z", "KeyZ", 22);
    assert_eq!(
        fixture.eval("[checkbox.checked, text.value]"),
        r#"[true,"k"]"#
    );
}

#[test]
fn channel_coalesces_moves_and_returns_wheel_decisions() {
    let fixture = Fixture::script(
        r#"
      globalThis.events = [];
      globalThis.target = document.body.appendChild(document.createElement("button"));
      target.id = "target";
      const quiet = () => {};
      const abort = new AbortController();
      target.addEventListener("touchmove", quiet, { signal: abort.signal });
      target.addEventListener("touchmove", quiet, { signal: abort.signal });
      target.addEventListener("wheel", e => {
        e.preventDefault();
        events.push([e.type, e.deltaX, e.deltaY, e.deltaMode, e.ctrlKey, e.cancelable, e.defaultPrevented]);
      }, { once: true });
      document.addEventListener("wheel", e => {
        e.preventDefault();
        events.push(["document", e.cancelable, e.defaultPrevented]);
      });
      addEventListener("touchmove", quiet, { passive: false });
      removeEventListener("touchmove", quiet);
      abort.abort();
      target.addEventListener("pointermove", e => events.push([
        e.clientX, e.getCoalescedEvents().map(s => [s.clientX, s.timeStamp, s.isTrusted, s.target === target]),
        innerWidth,
      ]));
      target.addEventListener("pointerdown", () => events.push("down"));
      target.addEventListener("pointerup", () => events.push("up"));
      target.addEventListener("click", () => events.push("click"));
    "#,
    );
    let target = fixture.node("target");
    let stale = NodeId::from_raw(target.raw() | (1 << 24));
    let (send, receive) = mpsc::channel();
    let (reply, responses) = mpsc::channel();
    let (eval_tx, eval_rx) = mpsc::channel();
    let mut next_tick = tick(fixture.origin, 2, 16);
    next_tick.viewport.css.width = 480.;
    for i in [2, 3] {
        send.send(DocumentTask::Input {
            task: pointer(fixture.origin, target, PointerKind::Move, i),
            reply: None,
        })
        .unwrap();
    }
    for kind in [PointerKind::Down, PointerKind::Up] {
        send.send(DocumentTask::Input {
            task: pointer(
                fixture.origin,
                target,
                kind,
                4 + u32::from(kind == PointerKind::Up),
            ),
            reply: Some(reply.clone()),
        })
        .unwrap();
    }
    send.send(DocumentTask::Input {
        task: pointer(fixture.origin, stale, PointerKind::Move, 6),
        reply: Some(reply.clone()),
    })
    .unwrap();
    send.send(DocumentTask::Input {
        task: wheel(fixture.origin, target, 7),
        reply: Some(reply.clone()),
    })
    .unwrap();
    send.send(DocumentTask::Input {
        task: wheel(fixture.origin, target, 8),
        reply: None,
    })
    .unwrap();
    send.send(DocumentTask::Eval {
        script: "events".into(),
        reply: eval_tx,
    })
    .unwrap();
    send.send(next_tick.into()).unwrap();
    fixture
        .runner
        .run(receive, |_| {
            send.send(DocumentTask::Shutdown).unwrap();
        })
        .unwrap();

    let decisions: Vec<_> = responses
        .try_iter()
        .map(Result::unwrap)
        .map(|reply| (reply.request, reply.default_prevented))
        .collect();
    assert_eq!(decisions, [(4, false), (5, false), (6, false), (7, true)]);
    assert_eq!(
        eval_rx.try_recv().unwrap().unwrap(),
        r#"[
       [3,[[2,2,true,true]],480],"down","up","click",
       ["wheel",2,-5,1,true,true,true],["document",true,true],["document",false,false]
    ]"#
        .split_whitespace()
        .collect::<String>()
    );
    let flags = fixture
        .journal
        .borrow()
        .records
        .iter()
        .filter(|record| record.op == Opcode::Listeners)
        .map(|record| (record.a, record.b))
        .collect::<Vec<_>>();
    assert_eq!(
        flags,
        [
            (target.raw(), 2),
            (target.raw(), 3),
            (0, 2),
            (0, 0),
            (target.raw(), 1),
            (target.raw(), 0)
        ]
    );
}

#[test]
fn overlay_under_stationary_pointer_updates_hover_and_only_boundary_events() {
    let mut fixture = Fixture::script(
        r#"
      globalThis.events = [];
      const shared = document.body.appendChild(document.createElement("main"));
      shared.id = "shared";
      const underlay = shared.appendChild(document.createElement("button"));
      underlay.id = "underlay";
      underlay.style.cssText = "position:absolute; left:20px; top:20px; width:160px; height:160px";
      const overlay = shared.appendChild(document.createElement("div"));
      overlay.id = "overlay";
      overlay.style.cssText = "position:fixed; left:20px; top:20px; width:160px; height:160px; z-index:10; display:none";
      for (const target of [underlay, overlay, shared]) {
        for (const type of ["pointerout", "pointerleave", "pointerover", "pointerenter", "pointermove",
            "mouseout", "mouseleave", "mouseover", "mouseenter", "mousemove"]) {
          target.addEventListener(type, event => {
            if (event.target !== target) return;
            events.push([type, target.id, event.relatedTarget?.id ?? null, event.clientX, event.clientY, event.isTrusted]);
          });
        }
      }
    "#,
    );
    let underlay = fixture.node("underlay");
    let overlay = fixture.node("overlay");
    let stationary_move = |target, request, boundary| {
        let mut task = pointer(fixture.origin, target, PointerKind::Move, request);
        if let Input::Pointer {
            css,
            boundary_update,
            ..
        } = &mut task.input
        {
            *css = Point { x: 60., y: 60. };
            *boundary_update = boundary;
        }
        task
    };
    let enter = stationary_move(underlay, 1, false);
    let mut reveal = stationary_move(overlay, 2, true);
    reveal.related = underlay;
    fixture.input(enter);
    fixture.eval("events.length = 0; overlay.style.display = 'block';");
    fixture.runner.frame(tick(fixture.origin, 2, 16)).unwrap();
    fixture.journal.borrow_mut().records.clear();
    assert!(!fixture.input(reveal).default_prevented);
    assert_eq!(
        fixture.eval("events"),
        concat!(
            r#"[["pointerout","underlay","overlay",60,60,true],["pointerleave","underlay","overlay",60,60,true],"#,
            r#"["pointerover","overlay","underlay",60,60,true],["pointerenter","overlay","underlay",60,60,true],"#,
            r#"["mouseout","underlay","overlay",60,60,true],["mouseleave","underlay","overlay",60,60,true],"#,
            r#"["mouseover","overlay","underlay",60,60,true],["mouseenter","overlay","underlay",60,60,true]]"#
        )
    );
    fixture.runner.frame(tick(fixture.origin, 3, 32)).unwrap();
    let states = fixture
        .journal
        .borrow()
        .records
        .iter()
        .filter(|record| {
            record.op == Opcode::SetState && record.c & ElementState::HOVER.bits() != 0
        })
        .map(|record| (record.a, record.b & ElementState::HOVER.bits() != 0))
        .collect::<Vec<_>>();
    assert_eq!(states, [(underlay.raw(), false), (overlay.raw(), true)]);
}

#[test]
fn boundary_updates_do_not_coalesce_with_physical_moves() {
    let fixture = Fixture::script(
        r#"
      globalThis.moves = [];
      document.body.addEventListener("pointermove", event => moves.push([
        event.clientX, event.getCoalescedEvents().length
      ]));
    "#,
    );
    let (send, receive) = mpsc::channel();
    let (reply, response) = mpsc::channel();
    for request in 1..=3 {
        let mut task = pointer(fixture.origin, BODY, PointerKind::Move, request);
        if let Input::Pointer {
            boundary_update, ..
        } = &mut task.input
        {
            *boundary_update = request == 2;
        }
        send.send(DocumentTask::Input { task, reply: None })
            .unwrap();
    }
    send.send(tick(fixture.origin, 2, 16).into()).unwrap();
    let mut send = Some(send);
    fixture
        .runner
        .run(receive, |_| {
            let send = send.take().unwrap();
            send.send(DocumentTask::Eval {
                script: "moves".into(),
                reply: reply.clone(),
            })
            .unwrap();
        })
        .unwrap();
    assert_eq!(response.try_recv().unwrap().unwrap(), "[[1,0],[3,0]]");
}

#[test]
fn frame_coalescing_carries_moves_across_channel_drains() {
    let fixture = Fixture::script(
        r#"
      globalThis.moves = [];
      document.body.addEventListener("pointermove", e => moves.push([
        e.clientX, e.getCoalescedEvents().length, e.getCoalescedEvents().at(-1)?.clientX
      ]));
    "#,
    );
    let (send, receive) = mpsc::channel();
    let (reply, response) = mpsc::channel();
    for i in 1..=70 {
        send.send(DocumentTask::Input {
            task: pointer(fixture.origin, BODY, PointerKind::Move, i),
            reply: None,
        })
        .unwrap();
    }
    send.send(tick(fixture.origin, 2, 80).into()).unwrap();
    let mut send = Some(send);
    fixture
        .runner
        .run(receive, |_| {
            let send = send.take().unwrap();
            send.send(DocumentTask::Eval {
                script: "moves".into(),
                reply: reply.clone(),
            })
            .unwrap();
        })
        .unwrap();
    assert_eq!(response.try_recv().unwrap().unwrap(), "[[70,69,69]]");
}

#[test]
fn todo_edit_gesture_dispatches_native_double_click_and_select_all() {
    let mut fixture = Fixture::script(
        r#"
        const make = (tag, id) => {
          const node = document.body.appendChild(document.createElement(tag));
          node.id = id;
          return node;
        };
        const label = make("label", "label");
        make("label", "other");
        const edit = make("input", "edit");
        const notes = make("textarea", "notes");
        edit.value = "todo 🖊";
        notes.value = "a\nb";
        notes.readOnly = true;
        globalThis.clicks = [];
        globalThis.selections = [];
        for (const type of ["mousedown", "mouseup", "click"]) {
          label.addEventListener(type, e => clicks.push([type, e.detail]));
        }
        label.ondblclick = e => {
          clicks.push([e.type, e.detail, e.isTrusted, e instanceof MouseEvent]);
          edit.focus();
        };
        for (const control of [edit, notes]) {
          control.onselect = e => selections.push([control.id, control.selectionStart, control.selectionEnd, e.isTrusted]);
        }
    "#,
    );

    let mut click = |target: &str, time: u32, x: f32| {
        let node = fixture.node(target);
        for (kind, ms) in [(PointerKind::Down, time), (PointerKind::Up, time + 1)] {
            let mut task = pointer(fixture.origin, node, kind, ms);
            if let Input::Pointer { css, .. } = &mut task.input {
                css.x = x;
            }
            fixture.input(task);
        }
    };
    click("label", 10, 5.);
    click("label", 100, 6.);
    click("label", 750, 6.); // Beyond the time window.
    click("label", 850, 10.); // Beyond the distance tolerance.
    click("other", 900, 10.);
    click("label", 950, 10.); // Different target in between.
    assert_eq!(
        fixture.eval("clicks"),
        r#"[["mousedown",1],["mouseup",1],["click",1],["mousedown",2],["mouseup",2],["click",2],["dblclick",2,true,true],["mousedown",1],["mouseup",1],["click",1],["mousedown",1],["mouseup",1],["click",1],["mousedown",1],["mouseup",1],["click",1]]"#
    );

    fixture.eval("edit.focus(); edit.setSelectionRange(2, 2);");
    fixture.runner.run_ready().unwrap();
    fixture.eval("selections.length = 0;");
    let mut ctrl = key_task(fixture.origin, "a", "KeyA", 1100);
    ctrl.modifiers.ctrl = true;
    fixture.input(ctrl.clone());
    assert_eq!(
        fixture.eval("[edit.selectionStart, edit.selectionEnd, selections.length]"),
        "[0,7,0]"
    );
    fixture.runner.run_ready().unwrap();
    fixture.key("x", "KeyX", 1101);
    assert_eq!(fixture.eval("edit.value"), r#""x""#);

    fixture.eval("notes.focus();");
    let mut meta = key_task(fixture.origin, "A", "KeyA", 1102);
    meta.modifiers.meta = true;
    fixture.input(meta);
    fixture.runner.run_ready().unwrap();
    assert_eq!(
        fixture.eval("selections"),
        r#"[["edit",0,7,true],["notes",0,3,true]]"#
    );

    fixture.eval(
        "edit.focus(); edit.addEventListener('keydown', e => e.preventDefault(), { once: true });",
    );
    assert!(fixture.input(ctrl).default_prevented);
    assert_eq!(
        fixture.eval("[edit.selectionStart, edit.selectionEnd]"),
        "[1,1]"
    );
}
