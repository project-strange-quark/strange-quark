// SPDX-License-Identifier: MIT

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use squark_types::render::FrameDemand;

fn native_font_runner(
    pack: &Pack,
) -> (
    DocumentRunner<'_>,
    mpsc::Receiver<()>,
    mpsc::Receiver<squark_types::render::ResourceMessage>,
    Instant,
) {
    support::initialize_v8();
    let origin = Instant::now();
    let (resources, updates) = resource_channel();
    let (wake, frames) = mpsc::channel();
    let demand = FrameDemand::new(move || {
        let _ = wake.send(());
    });
    let engine = StaticDocument::from_pack(pack, URL.parse().unwrap(), resources)
        .unwrap()
        .with_image_frame_demand(demand.clone());
    let mut settings = options(origin, true);
    settings.frame_demand = demand;
    let runner = DocumentRunner::new(
        settings,
        pack.entry(DOCUMENT_PATH).unwrap().bytes,
        sources(|path| (path == "/assets/main.js").then(|| ("text/javascript".into(), vec![]))),
        Box::new(engine),
        Box::new(Paint(Trace::default())),
        Instant::now,
    )
    .unwrap();
    (runner, frames, updates, origin)
}

#[test]
fn engine_font_completions_settle_promises_and_dispatch_events() {
    let pack =
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap();
    let (mut runner, frames, _updates, origin) = native_font_runner(&pack);
    let font = STANDARD.encode(include_bytes!(
        "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
    ));
    assert_eq!(
        runner
            .eval(&format!(
                r#"
        globalThis.events = [];
        for (const name of ["loading", "loadingdone", "loadingerror"]) {{
          document.fonts.addEventListener(name, e => events.push([name, e.fontfaces.map(face => face.family)]));
        }}
        globalThis.good = new FontFace("Fixture", "url(data:font/woff2;base64,{font})");
        globalThis.bad = new FontFace("Broken", "url(data:font/woff2;base64,AA==)");
        document.fonts.add(good).add(bad);
        globalThis.result = {{}};
        good.loaded.then(face => result.face = face === good);
        bad.loaded.catch(error => result.error = error.name);
        const samePromise = good.load() === good.loaded;
        void bad.load();
        document.fonts.load("16px Fixture", "ABC").then(faces => result.match = faces[0] === good);
        document.fonts.ready.then(set => result.ready = set === document.fonts);
        [samePromise, good.status, bad.status, document.fonts.status, Object.keys(result)]
    "#
            ))
            .unwrap(),
        r#"[true,"loading","loading","loading",[]]"#
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seq = 1;
    while runner.eval("events.length === 3").unwrap() != "true" {
        frames
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("font completion should request a frame");
        runner.frame(tick(origin, seq, seq * 16)).unwrap();
        runner.run_ready().unwrap();
        seq += 1;
    }
    assert_eq!(
        runner
            .eval(r#"[good.status, bad.status, document.fonts.status, result.face, result.match, result.ready, result.error, events]"#)
            .unwrap(),
        r#"["loaded","error","loaded",true,true,true,"NetworkError",[["loading",[]],["loadingdone",["Fixture"]],["loadingerror",["Broken"]]]]"#
    );
}

#[test]
fn rendered_text_can_start_scripted_faces_without_an_explicit_load() {
    let pack =
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap();
    let (mut runner, frames, _updates, origin) = native_font_runner(&pack);
    let font = STANDARD.encode(include_bytes!(
        "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
    ));
    assert_eq!(
        runner.eval(&format!(r#"
        globalThis.good = new FontFace("Welcome", "url(data:font/woff2;base64,{font})");
        globalThis.bad = new FontFace("Broken", "url(data:font/woff2;base64,AA==)");
        document.fonts.add(good).add(bad);
        globalThis.events = [];
        globalThis.result = {{}};
        globalThis.goodPromise = good.loaded;
        globalThis.badPromise = bad.loaded;
        goodPromise.then(face => result.good = face === good);
        badPromise.catch(error => result.bad = error.name);
        document.fonts.addEventListener("loading", () => {{
          events.push(["loading", good.status, bad.status, document.fonts.status]);
          document.fonts.ready.then(set => result.ready = set === document.fonts);
        }});
        for (const name of ["loadingdone", "loadingerror"]) {{
          document.fonts.addEventListener(name, e => events.push([name, e.fontfaces.map(face => face.family)]));
        }}
        for (const family of ["Welcome", "Broken"]) {{
          const text = document.createElement("span");
          text.textContent = "Draw a shape";
          text.style.fontFamily = family;
          document.body.append(text);
        }}
        [good.status, bad.status, document.fonts.status]
        "#)).unwrap(),
        r#"["unloaded","unloaded","loaded"]"#
    );
    runner.frame(tick(origin, 1, 16)).unwrap();
    runner.run_ready().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seq = 2;
    while runner.eval("events.length === 3").unwrap() != "true" {
        frames
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("the automatic load should request a completion frame");
        runner.frame(tick(origin, seq, seq * 16)).unwrap();
        runner.run_ready().unwrap();
        seq += 1;
    }
    assert_eq!(
        runner
            .eval(
                r#"
        [good.status, bad.status, document.fonts.status, result.good, result.bad, result.ready,
          good.load() === goodPromise, bad.load() === badPromise, events]
        "#
            )
            .unwrap(),
        r#"["loaded","error","loaded",true,"NetworkError",true,true,true,[["loading","loading","loading","loading"],["loadingdone",["Welcome"]],["loadingerror",["Broken"]]]]"#
    );
}

#[test]
fn font_faces_flush_descriptors_and_settle_on_resource_tasks() {
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
        Box::new(Paint(trace.clone())),
        move || origin,
    )
    .unwrap();
    assert_eq!(runner.eval(r#"
        globalThis.events = [];
        for (const name of ["loading", "loadingdone", "loadingerror"]) {
          document.fonts.addEventListener(name, e => events.push([name, e.fontfaces.map(face => face.family)]));
        }
        globalThis.first = new FontFace("UI", "url(/fonts/app.woff2)", {unicodeRange:"U+0000-007F"});
        const data = Uint8Array.of(0, 1, 2, 3, 4);
        globalThis.second = new FontFace("UI", data.subarray(1, 4));
        data[1] = 99;
        document.fonts.add(first).add(second);
        globalThis.points = [];
        globalThis.loaded = first.load();
        loaded.then(face => points.push(["face", face === first]));
        document.fonts.load("16px UI", "A").then(faces => points.push(["match", faces[0] === first]));
        document.fonts.load("16px UI", "💡").catch(e => points.push(["error", e.name]));
        document.fonts.ready.then(set => points.push(["ready", set === document.fonts]));
        let syntax;
        try { document.fonts.check("bad"); } catch (e) { syntax = e.name; }
        [first.status, second.status, document.fonts.status, document.fonts.check("16px UI","A"),
          loaded === first.loaded, syntax, points.length]
    "#).unwrap(), r#"["loading","loading","loading",false,true,"SyntaxError",0]"#);
    {
        let log = journal.borrow();
        assert_eq!(
            log.fonts
                .iter()
                .map(|record| (record.id, record.flags))
                .collect::<Vec<_>>(),
            [(1, 0), (2, 0), (1, 1), (2, 1)]
        );
        assert_eq!(log.fonts[0].family, "UI");
        assert_eq!(log.fonts[0].descriptor["unicode-range"], "U+0000-007F");
        assert_eq!(
            log.fonts[0].descriptor["source"],
            serde_json::json!({"css":"url(/fonts/app.woff2)", "baseUrl":URL})
        );
        assert_eq!(log.fonts[1].bytes, Some(vec![1, 2, 3]));
    }
    assert_eq!(
        trace
            .borrow()
            .iter()
            .filter(|event| event.starts_with("font load:"))
            .count(),
        2
    );
    runner.font_result(FaceId(1), Ok(())).unwrap();
    assert_eq!(runner.eval(r#"
        [first.status, second.status, document.fonts.check("16px UI","A"), document.fonts.status, points]
    "#).unwrap(), r#"["loaded","loading",true,"loading",[["face",true],["match",true]]]"#);
    runner
        .font_result(FaceId(2), Err("bad font data".into()))
        .unwrap();
    runner.run_ready().unwrap();
    let result = runner.eval(r#"
        [document.fonts.status, second.status, points.some(item => item[0] === "ready" && item[1]),
          points.find(item => item[0] === "error")[1], events, document.fonts.delete(first), document.fonts.size]
    "#).unwrap();
    assert_eq!(
        result,
        r#"["loaded","error",true,"NetworkError",[["loading",[]],["loadingdone",["UI"]],["loadingerror",["UI"]]],true,1]"#
    );
    runner.frame(tick(origin, 1, 0)).unwrap();
    assert!(
        journal
            .borrow()
            .fonts
            .iter()
            .any(|record| record.id == 1 && record.flags == 2)
    );
}
