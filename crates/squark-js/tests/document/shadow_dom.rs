// SPDX-License-Identifier: MIT
//! Shadow DOM on V8 with the real engine, and trusted input into shadow trees.

use squark_types::host::{Input, InputTask, KeyKind, Modifiers, PointerKind, PointerType};

use super::*;

/// Monaco's context view: a shadow root with a style, a fixed view and a default slot.
const CONTEXT_VIEW: &str = r#"
    document.body.style.margin = "0";
    const page = document.createElement("style");
    page.textContent = ".item { color: rgb(255, 0, 0); height: 50px } .context-view { width: 1px } #host2 { color: rgb(255, 0, 0) }";
    document.head.append(page);
    const container = document.body.appendChild(document.createElement("div"));
    const host = container.appendChild(document.createElement("div"));
    host.className = "shadow-root-host";
    const root = host.attachShadow({ mode: "open" });
    const style = document.createElement("style");
    style.textContent = ":host { all: initial } .context-view { position: fixed; left: 40px; top: 30px; width: 120px }" +
      " .item { height: 20px; color: rgb(0, 0, 255); font: 12px monospace }";
    root.appendChild(style);
    const view = root.appendChild(document.createElement("div"));
    view.className = "context-view";
    root.appendChild(document.createElement("slot"));
    const items = [0, 1, 2].map(index => {
      const item = view.appendChild(document.createElement("div"));
      item.className = "item";
      item.textContent = "Item " + index;
      return item;
    });

    const host2 = document.body.appendChild(document.createElement("div"));
    host2.id = "host2";
    host2.style.cssText = "position: absolute; top: 150px; left: 200px; width: 100px";
    const light = host2.appendChild(document.createElement("p"));
    light.textContent = "slotted";
    const root2 = host2.attachShadow({ mode: "closed" });
    const wrapper = root2.appendChild(document.createElement("div"));
    wrapper.style.cssText = "position: relative; color: rgb(0, 128, 0); padding-top: 20px";
    const slot2 = wrapper.appendChild(document.createElement("slot"));
    const style2 = root2.appendChild(document.createElement("style"));
    style2.textContent = "::slotted(p) { margin: 0; height: 13px }";
    const rect = element => { const { x, y, width, height } = element.getBoundingClientRect(); return [x, y, width, height]; };
"#;

#[test]
fn shadow_trees_style_lay_out_and_hit_test_through_the_engine() {
    support::initialize_v8();
    let origin = Instant::now();
    let pack = Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    );
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, _updates) = resource_channel();
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources.clone()).unwrap();
    let painter = Painter::new(Arc::new(FrameMailbox::default()), resources);
    let mut runner = DocumentRunner::new(
        options(origin, true),
        bytes,
        sources({
            let pack = pack.clone();
            move |path| {
                let asset = pack.entry(path)?;
                Some((asset.mime.into(), asset.bytes.to_vec()))
            }
        }),
        Box::new(engine),
        Box::new(painter),
        Instant::now,
    )
    .unwrap();
    runner.eval(CONTEXT_VIEW).unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    assert_eq!(
        runner
            .eval(
                "[getComputedStyle(view).position, rect(view), rect(items[1]), getComputedStyle(items[0]).color,
                  getComputedStyle(root.querySelector('slot')).display]"
            )
            .unwrap(),
        r#"["fixed",[40,30,120,60],[40,50,120,20],"rgb(0, 0, 255)","contents"]"#
    );
    assert_eq!(
        runner
            .eval(
                "[rect(host2), rect(light), document.elementFromPoint(50, 55) === host, root.elementFromPoint(50, 55) === items[1],
                  document.elementFromPoint(210, 155) === host2, root2.elementFromPoint(210, 175) === light,
                  document.elementFromPoint(210, 175) === light,
                  document.querySelector('.item'), root.querySelectorAll('.item').length,
                  root.querySelector('.item:nth-child(2)') === items[1], root.querySelector(':host'),
                  items[0].offsetParent === view, light.offsetParent === host2]"
            )
            .unwrap(),
        "[[200,150,100,33],[200,170,100,13],true,true,true,true,true,null,3,true,null,true,true]"
    );
    assert_eq!(
        runner
            .eval(
                "[rect(light)[3], getComputedStyle(light).color, getComputedStyle(host2).color,
                  (() => { const range = document.createRange(); range.selectNodeContents(items[0].firstChild);
                           return range.getBoundingClientRect().y; })()]"
            )
            .unwrap(),
        r#"[13,"rgb(0, 128, 0)","rgb(255, 0, 0)",30]"#,
        "slotted nodes inherit through their slot"
    );

    runner
        .eval(
            r#"
            globalThis.sheet = new CSSStyleSheet();
            sheet.replaceSync("@import url(other.css); .item { font-size: 30px; height: 31px }");
            root.adoptedStyleSheets = [sheet];
            "#,
        )
        .unwrap();
    assert_eq!(
        runner
            .eval("[getComputedStyle(items[0]).fontSize, rect(items[2]), sheet.cssRules.length, sheet.cssRules[0].cssText]")
            .unwrap(),
        r#"["30px",[40,92,120,31],1,".item { font-size: 30px; height: 31px; }"]"#
    );
    runner
        .eval("sheet.insertRule('.context-view { left: 7px }', 1); sheet.disabled = false;")
        .unwrap();
    assert_eq!(runner.eval("rect(view)[0]").unwrap(), "7");
    runner.eval("sheet.disabled = true;").unwrap();
    assert_eq!(
        runner.eval("[rect(view)[0], rect(items[0])[3]]").unwrap(),
        "[40,20]"
    );

    // Web components commonly fill a shadow root through innerHTML.
    runner
        .eval(
            r#"
            globalThis.card = document.body.appendChild(document.createElement("x-card"));
            card.style.cssText = "position: absolute; top: 0; left: 0";
            card.innerHTML = '<b slot="title">T</b><i>body</i>';
            globalThis.cardRoot = card.attachShadow({ mode: "open" });
            cardRoot.innerHTML = '<style>:host { display: block; width: 33px } ::slotted(b) { display: block; height: 9px }' +
              ' .frame { padding-top: 5px }</style><div class="frame"><slot name="title"></slot><slot></slot></div>';
            "#,
        )
        .unwrap();
    assert_eq!(
        runner
            .eval(
                "[cardRoot.innerHTML.length > 0, cardRoot.querySelector('slot').assignedNodes().length,
                  rect(card)[2], rect(card.firstChild), cardRoot.styleSheets.length]"
            )
            .unwrap(),
        "[true,1,33,[0,5,33,9],1]"
    );
}

fn pointer(origin: Instant, target: NodeId, kind: PointerKind, request: u32) -> InputTask {
    let down = kind == PointerKind::Down;
    InputTask {
        request,
        target,
        related: NodeId::from_raw(0),
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
            pressure: if down { 0.5 } else { 0. },
            button: if matches!(kind, PointerKind::Down | PointerKind::Up) {
                0
            } else {
                -1
            },
            buttons: u16::from(down),
            samples: Box::new([]),
        },
    }
}

#[test]
fn trusted_input_in_a_shadow_tree_retargets_at_its_host() {
    support::initialize_v8();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let journal = engine.journal.clone();
    let origin = Instant::now();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(engine),
        Box::new(Paint(trace)),
        move || origin,
    )
    .unwrap();
    runner
        .eval(
            r#"
            const host = document.body.appendChild(document.createElement("div"));
            host.id = "host";
            const root = host.attachShadow({ mode: "open" });
            const field = root.appendChild(document.createElement("input"));
            field.id = "field";
            globalThis.log = [];
            const name = target => target === null ? null : target.id || target.nodeName;
            for (const type of ["pointerdown", "mousedown", "click", "focusin", "keydown"]) {
              document.addEventListener(type, event => log.push(["document", type, name(event.target), event.composed]));
              field.addEventListener(type, event => log.push(["field", type, name(event.target), event.composedPath().length]));
            }
            "#,
        )
        .unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    let field = journal.borrow().ids["field"];
    runner
        .input(&pointer(origin, field, PointerKind::Down, 5))
        .unwrap();
    runner
        .input(&pointer(origin, field, PointerKind::Up, 6))
        .unwrap();
    runner
        .input(&InputTask {
            request: 7,
            target: BODY,
            related: NodeId::from_raw(0),
            time: origin + Duration::from_millis(7),
            modifiers: Modifiers::default(),
            input: Input::Key {
                kind: KeyKind::Down,
                key: "a".into(),
                code: "KeyA".into(),
                location: 0,
                repeat: false,
                composing: false,
            },
        })
        .unwrap();
    assert_eq!(
        runner
            .eval("[log, document.activeElement === host, root.activeElement === field]")
            .unwrap(),
        concat!(
            r#"[[["field","pointerdown","field",7],["document","pointerdown","host",true],"#,
            r#"["field","mousedown","field",7],["document","mousedown","host",true],"#,
            r#"["field","focusin","field",7],["document","focusin","host",true],"#,
            r#"["field","click","field",7],["document","click","host",true],"#,
            r#"["field","keydown","field",7],["document","keydown","host",true]],true,true]"#
        )
    );
}
