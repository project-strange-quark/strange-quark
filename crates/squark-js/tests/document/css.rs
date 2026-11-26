// SPDX-License-Identifier: MIT

use std::sync::atomic::{AtomicUsize, Ordering};

use squark_js::document::{AnimationEvent, AnimationEventType};
use squark_types::render::FrameDemand;

use super::*;

struct Fixture {
    runner: DocumentRunner<'static>,
    engine: Rc<RefCell<CssDouble>>,
    journal: Rc<RefCell<JournalLog>>,
    trace: Trace,
    origin: Instant,
}

fn fixture_runner(
    packed: &[u8],
    sources: ScriptSources,
    demand: FrameDemand,
    seed: Option<&str>,
) -> Fixture {
    support::initialize_v8();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let css = engine.css.clone();
    let journal = engine.journal.clone();
    if let Some(text) = seed {
        journal.borrow_mut().sheets.insert(5, vec![text.into()]);
    }
    let origin = Instant::now();
    let mut settings = options(origin, true);
    settings.frame_demand = demand;
    let runner = DocumentRunner::new(
        settings,
        packed,
        sources,
        Box::new(engine),
        Box::new(Paint(trace.clone())),
        move || origin,
    )
    .unwrap();
    Fixture {
        runner,
        engine: css,
        journal,
        trace,
        origin,
    }
}

#[test]
fn packed_sheet_is_expanded_on_access_and_text_replaces_cssom_rules() {
    let css = "a { color: red; }\nb { color: blue; }";
    let fixture = fixture_with_style(&[], Some(css));
    let Fixture {
        mut runner,
        journal,
        trace,
        origin,
        ..
    } = fixture_runner(
        &fixture,
        sources(|_| None),
        FrameDemand::default(),
        Some(css),
    );
    assert_eq!(runner.eval("document.styleSheets.length").unwrap(), "1");
    assert!(journal.borrow().records.is_empty());
    assert!(trace.borrow().is_empty());
    assert_eq!(
        runner
            .eval(
                r#"
        globalThis.owner = document.getElementsByTagName("style")[0];
        globalThis.sheet = owner.sheet;
        globalThis.rules = sheet.cssRules;
        [Array.from(rules, rule => rule.cssText), sheet === document.styleSheets.item(0),
         rules[0] instanceof CSSRule, rules.item(1).parentStyleSheet === sheet]
    "#
            )
            .unwrap(),
        r#"[["a { color: red; }","b { color: blue; }"],true,true,true]"#
    );
    assert_eq!(runner.eval(r#"
        globalThis.removed = rules.item(0);
        sheet.deleteRule(0);
        sheet.insertRule("em{opacity:0}", 1);
        let rejected = false;
        try { sheet.insertRule("broken", 2); } catch (error) { rejected = error.name === "SyntaxError"; }
        [rules.length, rules[1].cssText, removed.parentStyleSheet, rejected]
    "#).unwrap(), r#"[2,"em { opacity: 0; }",null,true]"#);
    assert!(
        journal.borrow().records.is_empty(),
        "cssParseRule must not drain the journal"
    );
    runner.frame(tick(origin, 1, 1)).unwrap();
    assert_eq!(
        journal.borrow().sheets[&5],
        ["b { color: blue; }", "em { opacity: 0; }"]
    );

    assert_eq!(
        runner
            .eval(
                r#"
        owner.firstChild.data = "strong { color: green; }";
        [sheet.ownerNode, owner.sheet.cssRules[0].cssText, rules.length, owner.textContent]
    "#
            )
            .unwrap(),
        r#"[null,"strong { color: green; }",2,"strong { color: green; }"]"#
    );
    runner.frame(tick(origin, 2, 2)).unwrap();
    assert_eq!(journal.borrow().sheets[&5], ["strong { color: green; }"]);
    assert_eq!(
        runner
            .eval("owner.remove(); [document.styleSheets.length, owner.sheet]")
            .unwrap(),
        "[0,null]"
    );
    runner.frame(tick(origin, 3, 3)).unwrap();
    assert!(journal.borrow().sheets[&5].is_empty());
}

#[test]
fn emotion_and_styled_components_insert_update_and_remove_styles() {
    let bundle = crate::timeout::output(
        esbuild()
            .arg(key("crates/squark-js/tests/fixtures/css-in-js.mjs"))
            .args([
                "--bundle",
                "--platform=browser",
                "--format=esm",
                "--define:process.env.NODE_ENV=\"production\"",
            ]),
        Duration::from_secs(30),
    )
    .expect("run esbuild for the CSS-in-JS fixture");
    assert!(
        bundle.status.success(),
        "Run npm install first. {}",
        String::from_utf8_lossy(&bundle.stderr)
    );
    let fixture = fixture(&[(ScriptKind::ExternalModule, "/css-in-js.js", "/css-in-js.js")]);
    let Fixture {
        mut runner,
        origin,
        journal,
        ..
    } = fixture_runner(
        &fixture,
        sources(move |path| {
            (path == "/css-in-js.js").then(|| ("text/javascript".into(), bundle.stdout.clone()))
        }),
        FrameDemand::default(),
        None,
    );
    assert_eq!(runner.eval(r#"
        renderCss("tomato");
        const emotion = document.getElementById("emotion");
        const styled = document.getElementById("styled");
        globalThis.classes = [emotion.className, styled.className];
        const rules = Array.from(document.styleSheets).flatMap(sheet => Array.from(sheet.cssRules, rule => rule.cssText));
        [emotion.textContent, styled.textContent, rules.some(text => text.includes(classes[0]) && text.includes("tomato")),
         rules.some(text => text.includes(classes[1].split(" ").at(-1)) && text.includes("tomato")),
         rules.some(text => text.includes("@keyframes")), rules.some(text => text.includes("lavender"))]
    "#).unwrap(), r#"["tomato","tomato",true,true,true,true]"#);
    runner.frame(tick(origin, 1, 1)).unwrap();
    assert!(
        journal
            .borrow()
            .sheets
            .values()
            .flatten()
            .any(|rule| rule.contains("tomato"))
    );
    assert_eq!(runner.eval(r#"
        renderCss("rebeccapurple");
        [document.getElementById("emotion").className !== classes[0],
         document.getElementById("styled").className !== classes[1],
         Array.from(document.styleSheets).flatMap(sheet => Array.from(sheet.cssRules, rule => rule.cssText))
            .filter(text => text.includes("rebeccapurple")).length]
    "#).unwrap(), "[true,true,2]");
    assert_eq!(
        runner
            .eval(
                r#"
        renderCss("royalblue", true);
        Array.from(document.getElementsByTagName("style")).some(
          tag => tag.hasAttribute("data-styled") && tag.textContent.includes("royalblue"))
    "#
            )
            .unwrap(),
        "true"
    );
    assert_eq!(runner.eval(r#"
        unmountCss();
        Array.from(document.getElementsByTagName("style")).every(tag => !tag.hasAttribute("data-emotion"))
    "#).unwrap(), "true");
    runner.frame(tick(origin, 2, 2)).unwrap();
    assert!(
        journal
            .borrow()
            .records
            .iter()
            .any(|record| record.op == Opcode::SheetDelete)
    );
}

#[test]
fn viewport_and_animation_tasks_dispatch_typed_events_in_order() {
    let Fixture {
        mut runner, origin, ..
    } = fixture_runner(
        &fixture(&[]),
        sources(|_| None),
        FrameDemand::default(),
        None,
    );
    runner.eval(r#"
        globalThis.events = [];
        globalThis.width = matchMedia("(min-width:400px)");
        globalThis.dark = matchMedia("(prefers-color-scheme: dark)");
        width.addListener(event => events.push([event.media, event.matches, event instanceof MediaQueryListEvent, event.isTrusted]));
        dark.onchange = event => events.push([event.type, dark.matches]);
        for (const type of ["transitionrun","transitionstart","transitionend","transitioncancel",
                            "animationstart","animationiteration","animationend","animationcancel"]) {
          document.addEventListener(type, event => events.push([event.type, event.propertyName ?? event.animationName,
              event.elapsedTime, event.pseudoElement, event.isTrusted, event.target === document.body]));
        }
        requestAnimationFrame(() => events.push(["raf", width.matches, dark.matches]));
    "#).unwrap();
    let mut next = tick(origin, 1, 8);
    next.viewport.css.width = 500.;
    let (send, receive) = mpsc::channel();
    send.send(DocumentTask::Media(MediaState {
        color_scheme: squark_types::engine::ColorScheme::Dark,
        ..Default::default()
    }))
    .unwrap();
    send.send(next.into()).unwrap();
    let (reply_tx, reply_rx) = mpsc::channel();
    let mut send = Some(send);
    runner
        .run(receive, |_| {
            let send = send.take().unwrap();
            for kind in [
                AnimationEventType::TransitionRun,
                AnimationEventType::TransitionStart,
                AnimationEventType::TransitionEnd,
                AnimationEventType::TransitionCancel,
                AnimationEventType::AnimationStart,
                AnimationEventType::AnimationIteration,
                AnimationEventType::AnimationEnd,
                AnimationEventType::AnimationCancel,
            ] {
                send.send(DocumentTask::AnimationEvent(AnimationEvent {
                    target: BODY,
                    pseudo: Some("::before".into()),
                    kind,
                    name: "effect".into(),
                    elapsed_time: 0.125,
                }))
                .unwrap();
            }
            send.send(DocumentTask::Eval {
                script: "events".into(),
                reply: reply_tx.clone(),
            })
            .unwrap();
        })
        .unwrap();
    let outcome = reply_rx.try_recv().unwrap().unwrap();
    assert!(outcome.starts_with(
        r#"[["(min-width: 400px)",true,true,true],["change",true],["raf",true,true],"#
    ));
    assert!(outcome.ends_with(r#"["animationcancel","effect",0.125,"::before",true,true]]"#));
    assert_eq!(
        outcome
            .matches(r#""effect",0.125,"::before",true,true"#)
            .count(),
        8
    );
}

fn resize(id: u32, depth: u32) -> ResizeDelivery {
    ResizeDelivery {
        entries: vec![ResizeEntry {
            observer: id,
            target: BODY,
            depth,
            content_rect: RECT,
            content_box: vec![LogicalSize {
                inline_size: 100.,
                block_size: 50.,
            }],
            border_box: Vec::new(),
            device_pixel_content_box: Vec::new(),
        }],
        next_depth: depth,
        skipped: false,
    }
}

fn intersection() -> IntersectionEntry {
    IntersectionEntry {
        observer: 2,
        target: BODY,
        time: Duration::from_millis(11),
        root_bounds: Some(RECT),
        bounds: RECT,
        intersection: Rect::new(10., 20., 100., 100.),
        ratio: 0.5,
        is_intersecting: true,
    }
}

#[test]
fn observer_registrations_resize_relayout_and_queued_intersection_delivery() {
    let Fixture {
        mut runner,
        engine,
        origin,
        trace,
        ..
    } = fixture_runner(
        &fixture(&[]),
        sources(|_| None),
        FrameDemand::default(),
        None,
    );
    runner.eval(r#"
        globalThis.events = [];
        globalThis.ro = new ResizeObserver((entries, self) => {
          const entry = entries[0];
          events.push(["resize", self === ro, entry.target === document.body,
            entry.contentRect instanceof DOMRectReadOnly, entry.contentBoxSize[0].inlineSize,
            entry.target.getBoundingClientRect().width]);
          document.body.setAttribute("id", "resize-write");
        });
        globalThis.io = new IntersectionObserver((entries, self) => {
          const entry = entries[0];
          events.push(["intersection", self === io, entry.target.id, entry.time, entry.isIntersecting,
            entry.intersectionRatio, entry.rootBounds.width, entry.intersectionRect.width]);
        }, { root: document, rootMargin: "4px 2%", threshold: [1, 0, 0.5] });
        addEventListener("error", event => { events.push(["error", event.message]); event.preventDefault(); });
        ro.observe(document.body, { box: "border-box" });
        ro.observe(document.body, { box: "border-box" });
        io.observe(document.body);
    "#).unwrap();
    engine.borrow_mut().resize.extend([
        resize(1, 1),
        ResizeDelivery {
            skipped: true,
            ..Default::default()
        },
    ]);
    engine.borrow_mut().intersections.push(intersection());
    let result = runner.frame(tick(origin, 1, 12)).unwrap();
    assert!(result.resize.skipped);
    assert_eq!(
        engine.borrow().registrations,
        ["resize:1:3:BorderBox", "intersection:2:3"]
    );
    assert!(trace.borrow().windows(5).any(|w| w
        == [
            "resize-write",
            "layout:1",
            "resize:1",
            "intersection",
            "paint:1"
        ]));
    assert_eq!(
        runner.eval("events").unwrap(),
        r#"[["resize",true,true,true,100,200],["error","ResizeObserver loop completed with undelivered notifications."]]"#
    );
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("events.at(-1)").unwrap(),
        r#"["intersection",true,"resize-write",11,true,0.5,200,100]"#
    );
    engine.borrow_mut().intersections.push(intersection());
    runner.frame(tick(origin, 2, 16)).unwrap();
    assert_eq!(runner.eval("io.takeRecords().length").unwrap(), "1");
    runner.run_ready().unwrap();
    assert_eq!(runner.eval("events.length").unwrap(), "3");
    runner.eval("ro.unobserve(document.body); ro.observe(document.body); ro.disconnect(); io.unobserve(document.body); io.observe(document.body); io.disconnect();").unwrap();
    runner.frame(tick(origin, 3, 20)).unwrap();
    assert_eq!(
        &engine.borrow().registrations[2..],
        [
            "unresize:1:3",
            "resize:1:3:ContentBox",
            "disconnect-resize:1",
            "unintersection:2:3",
            "intersection:2:3",
            "disconnect-intersection:2"
        ]
    );
}

#[test]
fn frame_demand_wakes_for_raf_dirty_tasks_animations_and_resources() {
    let reported = Arc::new(AtomicUsize::new(0));
    let demand = FrameDemand::new({
        let reported = reported.clone();
        move || {
            reported.fetch_add(1, Ordering::Relaxed);
        }
    });
    let Fixture {
        mut runner,
        engine,
        origin,
        ..
    } = fixture_runner(&fixture(&[]), sources(|_| None), demand, None);
    let count = || reported.load(Ordering::Relaxed);
    runner
        .eval(r#"requestAnimationFrame(() => requestAnimationFrame(() => {}))"#)
        .unwrap();
    assert_eq!(count(), 1);
    runner.eval(r#"document.body.id = "one""#).unwrap();
    assert_eq!(count(), 1);
    runner.frame(tick(origin, 1, 4)).unwrap();
    assert_eq!(count(), 2);
    runner.frame(tick(origin, 2, 8)).unwrap();
    assert_eq!(count(), 2);
    runner
        .eval(r#"setTimeout(() => document.body.id = "timer", 0)"#)
        .unwrap();
    runner.run_ready().unwrap();
    assert_eq!(count(), 3);
    runner.frame(tick(origin, 3, 12)).unwrap();
    runner
        .eval(r#"document.body.id = "sync"; document.body.getBoundingClientRect();"#)
        .unwrap();
    assert_eq!(count(), 4);
    engine.borrow_mut().animations = true;
    runner.frame(tick(origin, 4, 16)).unwrap();
    assert_eq!(count(), 5);
    engine.borrow_mut().animations = false;
    runner.frame(tick(origin, 5, 20)).unwrap();
    runner.resource_layout_changed();
    runner.resource_layout_changed();
    assert_eq!(count(), 6);
}

#[test]
fn initial_hover_media_matches_before_startup_and_on_the_first_frame() {
    use squark_js::document::InitialViewport;
    use squark_types::engine::{ColorScheme, PointerMedia};

    support::initialize_v8();
    let origin = Instant::now();
    let (tasks, pending) = mpsc::channel();
    let provisions = MediaState {
        primary_hover: true,
        any_hover: true,
        primary_pointer: PointerMedia::Fine,
        any_fine_pointer: true,
        color_scheme: ColorScheme::Dark,
        ..Default::default()
    };
    tasks.send(DocumentTask::Media(provisions)).unwrap();
    tasks.send(tick(origin, 1, 0).into()).unwrap();
    tasks.send(DocumentTask::Shutdown).unwrap();
    let initial = InitialViewport::receive(&pending).unwrap();
    assert_eq!(initial.media, provisions);
    assert!(matches!(
        pending.try_recv().unwrap(),
        DocumentTask::Shutdown
    ));

    let pack = Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    );
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, _updates) = resource_channel();
    let mailbox = Arc::new(FrameMailbox::default());
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources.clone()).unwrap();
    let painter = Painter::new(mailbox, resources);
    let mut settings = options(origin, true);
    settings.initial_tick = initial.tick;
    let mut runner = DocumentRunner::new_with_media(
        settings,
        bytes,
        sources({
            let pack = pack.clone();
            move |path| {
                if path == "/assets/main.js" {
                    return Some(("text/javascript".into(), br##"
                        const sheet = document.createElement("style");
                        sheet.textContent = "#app { box-sizing: border-box; width: 19px } @media (hover: hover) { #app { width: 77px } }";
                        document.head.append(sheet);
                        globalThis.startupMedia = [
                            matchMedia("(hover: hover)").matches,
                            matchMedia("(any-hover: hover) and (any-pointer: fine) and (pointer: fine)").matches,
                            matchMedia("(prefers-color-scheme: dark)").matches,
                            getComputedStyle(document.getElementById("app")).width
                        ];
                    "##.to_vec()));
                }
                let entry = pack.entry(path)?;
                Some((entry.mime.to_owned(), entry.bytes.to_vec()))
            }
        }),
        Box::new(engine),
        Box::new(painter),
        Instant::now,
        initial.media,
    )
    .unwrap();
    assert_eq!(
        runner.eval("startupMedia").unwrap(),
        r#"[true,true,true,"77px"]"#
    );
    runner.frame(initial.tick).unwrap();
    assert_eq!(runner.query_rect("#app").unwrap().unwrap().size.width, 77.0);
}
