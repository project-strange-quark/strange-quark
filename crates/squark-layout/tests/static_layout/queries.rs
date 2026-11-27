// SPDX-License-Identifier: MIT
use std::time::{Duration, Instant};

use squark_layout::StaticDocument;
use squark_types::{
    Point, Rect,
    engine::{
        AnimationEventType as Event, ColorScheme, CssLength, DecodedBatch, Direction,
        DocumentEngine, EngineError, FrameInput, IntersectionOptions, LogicalSize, MeasureText,
        ParsedNode, Pseudo, Query, QueryValue, ResizeBox, ScrollSync, SelectMode,
    },
    journal::{FOCUS_PREVENT_SCROLL, Opcode, Record},
    render::{FrameTick, Viewport, resource_channel},
};

use super::{BODY, Fixture, NodeId, VIEWPORT};

fn id(raw: u32) -> NodeId {
    NodeId::from_raw(raw)
}
fn input() -> FrameInput<'static> {
    let now = Instant::now();
    FrameInput {
        tick: FrameTick {
            sequence: 1,
            vsync: now,
            present_at: now,
            commit_deadline: now,
            viewport: Viewport {
                css: VIEWPORT,
                device: [640, 480],
                scale: 1.0,
                epoch: 1,
            },
        },
        media: Default::default(),
        scroll: Default::default(),
    }
}

fn record(op: Opcode, a: u32, b: u32, c: u32) -> Record {
    Record {
        op,
        flags: 0,
        a,
        b,
        c,
    }
}
fn flush(engine: &mut StaticDocument<'_>, records: &[Record]) {
    engine
        .flush(DecodedBatch {
            records,
            strings: &[],
            staged_bytes: &[],
        })
        .unwrap();
}

#[test]
fn samples_transitions_and_keyframes_and_delivers_each_boundary_once() {
    let mut f = Fixture::new(
        "
        body { margin: 0 }
        #move { width:20px; height:20px; opacity:1; transition: width 1s linear, opacity 1s linear }
        #pulse { width: 60px; height:20px; animation: pulse 1s linear .1s 2 alternate forwards }
        @keyframes pulse { from { width:20px } to { width:100px } }
    ",
    );
    let moving = f.element(BODY, "div", &[("id", "move")]);
    let pulsing = f.element(BODY, "div", &[("id", "pulse")]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let initial = input();
    let at = |ms| {
        let mut frame = initial.clone();
        frame.tick.vsync += Duration::from_millis(ms);
        frame
    };
    let width = |doc: &mut StaticDocument<'_>, time, node| {
        let QueryValue::Rect(rect) = doc.query(&at(time), Query::BoundingRect(id(node))).unwrap()
        else {
            panic!("expected animated bounds");
        };
        rect.size.width
    };
    assert_eq!(width(&mut engine, 0, pulsing), 60.0);
    assert!(engine.has_running_animations());
    assert!(engine.take_animation_events().is_empty());

    let values: Vec<Vec<u16>> = ["width", "100px", "opacity", "0.2"]
        .iter()
        .map(|s| s.encode_utf16().collect())
        .collect();
    let strings: Vec<_> = values.iter().map(Vec::as_slice).collect();
    engine
        .flush(DecodedBatch {
            records: &[
                record(Opcode::DefineAtom, 1000, 0, 3),
                record(Opcode::DefineAtom, 1001, 2, 3),
                record(Opcode::SetStyle, moving, 1000, 1),
                record(Opcode::SetStyle, moving, 1001, 3),
            ],
            strings: &strings,
            staged_bytes: &[],
        })
        .unwrap();
    assert_eq!(width(&mut engine, 0, moving), 20.0);
    let started = engine.take_animation_events();
    assert_eq!(started.len(), 4);
    assert!(started.iter().all(|e| e.target == id(moving)
        && matches!(e.kind, Event::TransitionRun | Event::TransitionStart)));
    assert_eq!(width(&mut engine, 500, moving), 60.0);
    assert!((width(&mut engine, 500, pulsing) - 52.0).abs() < 0.01);
    assert_eq!(
        engine.take_animation_events()[0].kind,
        Event::AnimationStart
    );
    assert_eq!(width(&mut engine, 1000, moving), 100.0);
    assert_eq!(
        engine
            .take_animation_events()
            .iter()
            .filter(|e| e.kind == Event::TransitionEnd)
            .count(),
        2
    );
    assert!((width(&mut engine, 1600, pulsing) - 60.0).abs() < 0.01);
    let iteration = engine.take_animation_events();
    assert_eq!(iteration.len(), 1);
    assert_eq!(iteration[0].kind, Event::AnimationIteration);
    assert_eq!(iteration[0].elapsed_time, 1.0);
    assert_eq!(width(&mut engine, 2200, pulsing), 20.0);
    assert!(!engine.has_running_animations());
    assert_eq!(engine.take_animation_events()[0].kind, Event::AnimationEnd);
    assert_eq!(width(&mut engine, 2300, pulsing), 20.0);
    assert!(engine.take_animation_events().is_empty());
}

#[test]
fn publishes_timed_transform_opacity_and_filter_tracks_for_paint() {
    use squark_types::fragment::{AnimatedValue, AnimationDirection, AnimationFill, Easing};
    let mut f = Fixture::new(
        "
        #motion { animation: effect 2s linear .25s 3 alternate-reverse both }
        @keyframes effect {
            from { transform: translateX(0px); opacity: 0.2; filter: blur(0px); }
            50% { opacity: 0.6; animation-timing-function: ease-in; }
            to { transform: translateX(100px); opacity: 1; filter: blur(4px); }
        }
    ",
    );
    let target = id(f.element(BODY, "div", &[("id", "motion")]));
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut frame = input();
    let tracks = |engine: &StaticDocument<'_>| {
        engine
            .snapshot()
            .0
            .nodes
            .values()
            .find(|fragment| {
                fragment.owner == target && !fragment.style.properties.animations.is_empty()
            })
            .expect("animated fragment")
            .style
            .properties
            .animations
            .to_vec()
    };
    engine.update_layout(&frame).unwrap();
    let initial = tracks(&engine);
    assert_eq!(initial.len(), 3);
    let opacity = initial
        .iter()
        .find(|track| matches!(track.keyframes[0].value, AnimatedValue::Opacity(_)))
        .unwrap();
    assert_eq!(opacity.start, frame.tick.vsync);
    assert_eq!(opacity.duration, Duration::from_secs(2));
    assert_eq!(opacity.delay_seconds, 0.25);
    assert_eq!(opacity.iterations, 3.0);
    assert_eq!(opacity.fill, AnimationFill::Both);
    assert_eq!(opacity.direction, AnimationDirection::AlternateReverse);
    assert_eq!(opacity.keyframes.len(), 3);
    assert_eq!(opacity.keyframes[1].offset, 0.5);
    assert_eq!(opacity.keyframes[1].value, AnimatedValue::Opacity(0.6));
    assert_eq!(
        opacity.keyframes[1].easing,
        Easing::CubicBezier([0.42, 0.0, 1.0, 1.0])
    );
    assert_eq!(
        initial
            .iter()
            .filter(|track| track.keyframes.len() == 2)
            .count(),
        2
    );
    frame.tick.vsync += Duration::from_millis(2500);
    engine.update_layout(&frame).unwrap();
    assert_eq!(tracks(&engine), initial);
}

#[test]
fn publishes_a_spinners_full_turn_as_a_rotation_not_a_matrix() {
    use squark_types::fragment::{AnimatedValue, Transform};
    // The usual loading spinner. As matrices both ends are the identity, so
    // the compositor needs the angle to turn it.
    let mut f = Fixture::new(
        "
        #spinner { width: 20px; height: 20px; animation: spin 1s linear infinite }
        @keyframes spin { to { transform: rotate(360deg) } }
    ",
    );
    let target = id(f.element(BODY, "div", &[("id", "spinner")]));
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    engine.update_layout(&input()).unwrap();
    let snapshot = engine.snapshot();
    let tracks = &snapshot
        .0
        .nodes
        .values()
        .find(|fragment| {
            fragment.owner == target && !fragment.style.properties.animations.is_empty()
        })
        .expect("animated fragment")
        .style
        .properties
        .animations;
    assert_eq!(tracks.len(), 1);
    let values: Vec<_> = tracks[0]
        .keyframes
        .iter()
        .map(|key| key.value.clone())
        .collect();
    // Stylo fills the implicit 0% keyframe with rotate(0deg).
    let turn = |angle| {
        AnimatedValue::Transform(Box::new([Transform::Rotate {
            axis: [0.0, 0.0, 1.0],
            angle,
        }]))
    };
    assert_eq!(values, [turn(0.0), turn(std::f32::consts::TAU)]);
}

#[test]
fn parses_rules_and_reads_the_new_media_device_before_layout() {
    let mut f = Fixture::new(
        "
        #target {width: 20px} @media (min-width: 700px) { #target {width:40px} }
        @media (prefers-reduced-motion) and (prefers-contrast: more) { #target {width:80px} }
    ",
    );
    let target = f.element(BODY, "div", &[("id", "target")]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut frame = input();
    let units = |value: &str| value.encode_utf16().collect::<Vec<_>>();
    let query = units("(min-width:700px) and (prefers-color-scheme: DARK)");
    engine.set_media_state(&frame.tick.viewport, &frame.media);
    let (serialized, matched) = engine.evaluate_media(&query);
    assert!(!matched);
    assert_eq!(
        String::from_utf16(&serialized).unwrap(),
        "(min-width: 700px) and (prefers-color-scheme: dark)"
    );
    frame.tick.viewport.css.width = 800.0;
    frame.media.color_scheme = ColorScheme::Dark;
    engine.set_media_state(&frame.tick.viewport, &frame.media);
    assert!(engine.evaluate_media(&query).1);
    let QueryValue::Rect(rect) = engine
        .query(&frame, Query::BoundingRect(id(target)))
        .unwrap()
    else {
        panic!("expected target rect");
    };
    assert_eq!(rect.size.width, 40.0);
    frame.media.reduced_motion = true;
    frame.media.contrast = squark_types::engine::Contrast::More;
    engine.set_media_state(&frame.tick.viewport, &frame.media);
    assert!(
        engine
            .evaluate_media(&units(
                "(prefers-reduced-motion: reduce) and (not (prefers-contrast: less))"
            ))
            .1
    );
    assert!(
        !engine
            .evaluate_media(&units("(prefers-reduced-motion: no-preference)"))
            .1
    );
    let QueryValue::Rect(rect) = engine
        .query(&frame, Query::BoundingRect(id(target)))
        .unwrap()
    else {
        panic!("expected target rect");
    };
    assert_eq!(rect.size.width, 80.0);
    let read = |text: &[u16]| {
        engine
            .parse_rule(text)
            .map(|v| String::from_utf16(&v).unwrap())
    };
    assert_eq!(
        read(&units("div{ margin: 2px; }")).as_deref(),
        Some("div { margin: 2px; }")
    );
    assert!(read(&units("div {color:red} p {color:blue}")).is_none());
    assert!(read(&units("broken selector @ {color:red}")).is_none());
    let QueryValue::Strings(rules) = engine.query(&frame, Query::SheetRules(id(3))).unwrap() else {
        panic!("expected rules");
    };
    assert_eq!(rules.len(), 3);
    assert!(rules[1].starts_with("@media"));
    assert!(rules[2].contains("(prefers-reduced-motion)"));
}

#[test]
fn observer_geometry_retries_shallow_resizes_and_reports_intersection_thresholds() {
    let mut f = Fixture::new("
        body { margin: 0 }
        #port { position: relative; width: 100px; height: 100px; overflow: hidden }
        #child { position:absolute; width: 20px; height: 20px; top:90px; padding: 2px; border:1px solid }
    ");
    let port = f.element(BODY, "div", &[("id", "port")]);
    let child = f.element(port, "div", &[("id", "child")]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut frame = input();
    frame.tick.viewport.scale = 2.0;
    engine.observe_resize(1, id(port), ResizeBox::BorderBox);
    engine.observe_resize(1, id(child), ResizeBox::DevicePixelContentBox);
    engine.observe_intersection(
        2,
        id(child),
        IntersectionOptions {
            root: Some(id(port)),
            root_margin: [CssLength::Px(0.0); 4],
            thresholds: vec![0.0, 0.5, 1.0],
        },
    );
    engine.observe_intersection(
        3,
        id(child),
        IntersectionOptions {
            root: Some(id(port)),
            root_margin: [
                CssLength::Px(0.0),
                CssLength::Px(0.0),
                CssLength::Percent(20.0),
                CssLength::Px(0.0),
            ],
            thresholds: vec![1.0],
        },
    );
    engine.update_layout(&frame).unwrap();
    let resizes = engine.resize_entries(0);
    assert_eq!(resizes.entries.len(), 2);
    assert_eq!(
        resizes.entries[1].content_rect,
        Rect::new(2.0, 2.0, 20.0, 20.0)
    );
    assert_eq!(
        resizes.entries[1].device_pixel_content_box,
        [LogicalSize {
            inline_size: 40.0,
            block_size: 40.0
        }]
    );
    assert!(engine.resize_entries(resizes.next_depth).entries.is_empty());
    let first = engine.intersection_entries(frame.tick.vsync);
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].intersection.size.height, 10.0);
    assert!((first[0].ratio - 10.0 / 26.0).abs() < 1e-6);
    assert_eq!(first[1].ratio, 1.0);
    assert!(engine.intersection_entries(frame.tick.vsync).is_empty());

    let property: Vec<_> = "height".encode_utf16().collect();
    let height: Vec<_> = "82px".encode_utf16().collect();
    engine
        .flush(DecodedBatch {
            records: &[
                record(Opcode::DefineAtom, 1000, 0, 3),
                record(Opcode::SetStyle, port, 1000, 1),
            ],
            strings: &[&property, &height],
            staged_bytes: &[],
        })
        .unwrap();
    engine.update_layout(&frame).unwrap();
    let skipped = engine.resize_entries(resizes.next_depth);
    assert!(skipped.skipped && skipped.entries.is_empty());
    let retry = engine.resize_entries(0);
    assert_eq!(retry.entries.len(), 1);
    assert_eq!(retry.entries[0].target, id(port));
    let second = engine.intersection_entries(frame.tick.vsync);
    assert_eq!(second.len(), 2);
    assert!(!second[0].is_intersecting);
    engine.unobserve_resize(1, id(port));
    engine.disconnect_resize(1);
    engine.unobserve_intersection(2, id(child));
    engine.disconnect_intersection(3);
    assert!(engine.resize_entries(0).entries.is_empty());
    assert!(engine.intersection_entries(frame.tick.vsync).is_empty());
}

#[test]
fn resolves_geometry_and_styles_after_scroll() {
    let mut f = Fixture::new(
        "
        body { margin: 0 }
        #scroll { position: relative; overflow: auto; width: 200px; height: 100px;
                  padding: 10px; border: 5px solid }
        #fixed { position: fixed; left: 12px; top: 20px; width: 20px; height: 10px }
        #target { position: relative; left: 5%; width: 50%; height: 30px;
                  padding: 2% 10px; margin: 0 5%; transform: translateX(7px);
                  --accent: orange }
        #target::before { content: ''; display: block; color: rebeccapurple; height: 2px }
        #spacer { height: 130px }
    ",
    );
    let scroller = f.element(BODY, "div", &[("id", "scroll")]);
    f.element(scroller, "div", &[("id", "spacer")]);
    let target = f.element(scroller, "div", &[("id", "target")]);
    let fixed = f.element(scroller, "div", &[("id", "fixed")]);
    let hidden = f.element(BODY, "div", &[("style", "display:none;width:25%")]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let offsets = [squark_types::ScrollOffset {
        owner: id(scroller),
        offset: Point { x: 0.0, y: 45.0 },
        generation: 1,
    }];
    let mut frame = input();
    frame.scroll = ScrollSync {
        frame: 4,
        tick: 7,
        offsets: &offsets,
    };
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap(),
        QueryValue::Rect(Rect::new(42.0, 100.0, 120.0, 38.0))
    );
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(fixed)))
            .unwrap(),
        QueryValue::Rect(Rect::new(12.0, 20.0, 20.0, 10.0))
    );
    assert_eq!(
        engine
            .query(&input(), Query::BoundingRect(id(target)))
            .unwrap(),
        QueryValue::Rect(Rect::new(42.0, 100.0, 120.0, 38.0))
    );
    assert_eq!(
        engine
            .query(&frame, Query::ClientRects(id(hidden)))
            .unwrap(),
        QueryValue::Rects(Vec::new())
    );
    let QueryValue::BoxMetrics(metrics) =
        engine.query(&frame, Query::BoxMetrics(id(target))).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(metrics.offset_parent, Some(id(scroller)));
    assert_eq!(
        (
            metrics.offset_left,
            metrics.offset_top,
            metrics.offset_width,
            metrics.offset_height
        ),
        (30, 140, 120, 38)
    );
    assert_eq!((metrics.client_width, metrics.client_height), (120, 38));
    assert_eq!((metrics.scroll_width, metrics.scroll_height), (120, 38));
    let mut style = |node, pseudo, property| {
        engine
            .query(
                &frame,
                Query::ComputedStyle {
                    node: id(node),
                    pseudo,
                    property,
                },
            )
            .unwrap()
    };
    for (property, value) in [
        ("width", "100px"),
        ("inline-size", "100px"),
        ("left", "10px"),
        ("margin", "0px 10px"),
        ("padding", "4px 10px"),
        ("--accent", "orange"),
        ("transform", "matrix(1, 0, 0, 1, 7, 0)"),
        ("transform-origin", "60px 19px"),
        ("unknown-property", ""),
    ] {
        assert_eq!(
            style(target, None, property),
            QueryValue::String(value.into()),
            "{property}"
        );
    }
    assert_eq!(
        style(target, Some(Pseudo::Before), "height"),
        QueryValue::String("2px".into())
    );
    assert_eq!(
        style(hidden, None, "width"),
        QueryValue::String("25%".into())
    );
}

#[test]
fn focuses_once_and_keeps_prevent_scroll_and_fixed_geometry() {
    let mut f = Fixture::new(
        "
        body { margin: 0 }
        #scroller { overflow: auto; width: 120px; height: 100px }
        #scroller > div { height: 140px }
        input { display: block; box-sizing: border-box; width: 50px; height: 20px }
    ",
    );
    let scroller = f.element(BODY, "div", &[("id", "scroller")]);
    f.element(scroller, "div", &[]);
    let target = f.element(scroller, "input", &[]);
    f.element(scroller, "div", &[]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let frame = input();
    flush(
        &mut engine,
        &[record(Opcode::Focus, target, FOCUS_PREVENT_SCROLL, 0)],
    );
    assert_eq!(
        engine
            .query(&frame, Query::FocusScroll(id(target)))
            .unwrap(),
        QueryValue::Void
    );
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap(),
        QueryValue::Rect(Rect::new(0.0, 140.0, 50.0, 20.0))
    );
    flush(&mut engine, &[record(Opcode::Focus, target, 0, 0)]);
    engine
        .query(&frame, Query::FocusScroll(id(target)))
        .unwrap();
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap(),
        QueryValue::Rect(Rect::new(0.0, 40.0, 50.0, 20.0))
    );
    let offsets = [squark_types::ScrollOffset {
        owner: id(scroller),
        offset: Point { x: 0.0, y: 80.0 },
        generation: 2,
    }];
    let frame = FrameInput {
        scroll: ScrollSync {
            frame: 3,
            tick: 1,
            offsets: &offsets,
        },
        ..frame
    };
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap(),
        QueryValue::Rect(Rect::new(0.0, 60.0, 50.0, 20.0))
    );
}

#[test]
fn matches_selectors_uses_tree_order_and_reads_inline_fragments() {
    let mut f = Fixture::new("body { margin: 0 } #list { width: 45px; font: 16px/20px monospace }");
    let list = f.element(BODY, "section", &[("id", "list")]);
    let first = f.element(list, "span", &[("class", "child")]);
    f.text(first, "one two three");
    let second = f.element(list, "span", &[("class", "child")]);
    f.text(second, "four");
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let frame = input();
    let mut select = |scope, selector, mode| {
        engine.query(
            &frame,
            Query::Selector {
                scope: id(scope),
                selector,
                mode,
            },
        )
    };
    assert_eq!(
        select(list, ":scope > .child:first-child", SelectMode::First).unwrap(),
        QueryValue::Node(Some(id(first)))
    );
    assert_eq!(
        select(list, "#list, :scope > .child", SelectMode::All).unwrap(),
        QueryValue::Nodes(vec![id(first), id(second)])
    );
    assert_eq!(
        select(first, ":scope.child", SelectMode::Match).unwrap(),
        QueryValue::Bool(true)
    );
    assert_eq!(
        select(first, "#list", SelectMode::Closest).unwrap(),
        QueryValue::Node(Some(id(list)))
    );
    assert_eq!(
        select(first, ":not(", SelectMode::Match),
        Err(EngineError::InvalidSelector)
    );
    assert_eq!(
        select(999, "span", SelectMode::Match),
        Err(EngineError::MissingNode(id(999)))
    );
    let QueryValue::Rects(rects) = engine.query(&frame, Query::ClientRects(id(first))).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(rects.len(), 3);
    assert!(
        rects
            .windows(2)
            .all(|pair| pair[0].origin.y < pair[1].origin.y)
    );
    flush(
        &mut engine,
        &[
            record(Opcode::Remove, list, second, 0),
            record(Opcode::Insert, list, second, first),
            record(Opcode::Remove, list, first, 0),
        ],
    );
    let select = |scope, selector, mode| Query::Selector {
        scope: id(scope),
        selector,
        mode,
    };
    assert_eq!(
        engine
            .query(
                &frame,
                select(list, ":scope > .child:first-child", SelectMode::First)
            )
            .unwrap(),
        QueryValue::Node(Some(id(second)))
    );
    assert_eq!(
        engine
            .query(&frame, select(first, ".child", SelectMode::Match))
            .unwrap(),
        QueryValue::Bool(true)
    );
    assert_eq!(
        engine
            .query(&frame, Query::BoundingRect(id(first)))
            .unwrap(),
        QueryValue::Rect(Rect::default())
    );
}

#[test]
fn parses_detached_html_in_context_and_round_trips_utf16() {
    let mut f = Fixture::new("");
    let table = f.element(BODY, "table", &[]);
    let select = f.element(BODY, "select", &[]);
    let textarea = f.element(BODY, "textarea", &[]);
    let form = f.element(BODY, "form", &[]);
    let form_context = f.element(form, "div", &[]);
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    flush(&mut engine, &[record(Opcode::Remove, BODY, table, 0)]);
    let parse = |node, html: &str| {
        engine
            .parse_fragment(id(node), &html.encode_utf16().collect::<Vec<_>>())
            .unwrap()
    };
    let [ParsedNode::Element { name, children, .. }] = &parse(table, "<tr><td>A</td></tr>")[..]
    else {
        panic!("expected tbody");
    };
    assert_eq!(name.local, "tbody");
    assert!(matches!(&children[..], [ParsedNode::Element { name, .. }] if name.local == "tr"));
    let selected = parse(select, "<b>outside</b><option>A<option>B");
    assert!(
        matches!(&selected[..], [ParsedNode::Element { name: bold, .. }, ParsedNode::Element { name, .. }, ParsedNode::Element { name: second, .. }] if bold.local == "b" && name.local == "option" && second.local == "option")
    );
    let text = parse(textarea, "<b>&amp;");
    assert_eq!(text, [ParsedNode::Text("<b>&".encode_utf16().collect())]);
    assert!(
        matches!(&parse(form_context, "<form><input></form>")[..], [ParsedNode::Element { name, .. }] if name.local == "input")
    );
    let html: Vec<_> = "<template><svg viewBox='0 0 2 2'><use xlink:href='#a'/></svg><!--"
        .encode_utf16()
        .chain([0xd801])
        .chain("-->".encode_utf16())
        .chain([0xd800])
        .chain("&#xF0000;</template>".encode_utf16())
        .collect();
    let result = engine.parse_fragment(id(form_context), &html).unwrap();
    let [ParsedNode::Element { name, children, .. }] = &result[..] else {
        panic!("expected template");
    };
    assert_eq!(name.local, "template");
    let [
        ParsedNode::Element {
            name,
            attrs,
            children: svg_children,
        },
        ParsedNode::Comment(comment),
        ParsedNode::Text(text),
    ] = &children[..]
    else {
        panic!("{children:?}");
    };
    assert_eq!(name.namespace, "http://www.w3.org/2000/svg");
    assert_eq!(attrs[0].0.local, "viewBox");
    let [ParsedNode::Element { attrs, .. }] = &svg_children[..] else {
        panic!()
    };
    assert_eq!(attrs[0].0.namespace, "http://www.w3.org/1999/xlink");
    assert_eq!(attrs[0].0.prefix.as_deref(), Some("xlink"));
    assert_eq!(comment, &[0xd801]);
    assert_eq!(text, &[0xd800, 0xdb80, 0xdc00]);
}

#[test]
fn measures_svg_objects_and_shapes_canvas_text_with_font_environment() {
    let mut f = Fixture::new("body { margin: 0; font-size: 20px } text { font: 20px monospace }");
    f.ns = f
        .journal
        .intern(
            squark_types::journal::AtomKind::Namespace,
            &"http://www.w3.org/2000/svg"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let svg = f.element(
        BODY,
        "svg",
        &[
            ("width", "200"),
            ("height", "100"),
            ("viewBox", "0 0 100 50"),
        ],
    );
    let group = f.element(svg, "g", &[("transform", "translate(5 10)")]);
    let rect = f.element(
        group,
        "rect",
        &[
            ("x", "10"),
            ("y", "12"),
            ("width", "25"),
            ("height", "5"),
            ("transform", "translate(2 3)"),
        ],
    );
    let text = f.element(svg, "text", &[("x", "10"), ("y", "35")]);
    f.text(text, "gA");
    let pack = f.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let frame = input();
    assert_eq!(
        engine.query(&frame, Query::SvgBBox(id(rect))).unwrap(),
        QueryValue::Rect(Rect::new(10.0, 12.0, 25.0, 5.0))
    );
    assert_eq!(
        engine.query(&frame, Query::SvgBBox(id(group))).unwrap(),
        QueryValue::Rect(Rect::new(12.0, 15.0, 25.0, 5.0))
    );
    assert_eq!(
        engine.query(&frame, Query::BoundingRect(id(rect))).unwrap(),
        QueryValue::Rect(Rect::new(34.0, 50.0, 50.0, 10.0))
    );
    let units: Vec<_> = "gA".encode_utf16().collect();
    let request = MeasureText {
        font_css: "1em monospace",
        direction: Direction::Ltr,
        text: &units,
        font_environment: Some(id(BODY)),
        letter_spacing: 0.0,
        word_spacing: 0.0,
    };
    let result = engine.measure_text(request).unwrap();
    assert!(result.width > 20.0 && result.width < 30.0, "{result:?}");
    assert!(
        result.actual_ascent > 10.0 && result.actual_descent > 1.0 && result.font_ascent > 0.0,
        "{result:?}"
    );
    assert_eq!(
        engine
            .query(&frame, Query::SvgComputedTextLength(id(text)))
            .unwrap(),
        QueryValue::Number(result.width)
    );
    assert!(
        engine
            .measure_text(MeasureText {
                letter_spacing: 2.0,
                ..request
            })
            .unwrap()
            .width
            > result.width + 1.0
    );
    let empty = engine
        .measure_text(MeasureText {
            text: &[],
            ..request
        })
        .unwrap();
    let QueryValue::Rect(text_bounds) = engine.query(&frame, Query::SvgBBox(id(text))).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(text_bounds.origin.x, 10.0);
    assert!((f64::from(text_bounds.size.width) - result.width).abs() < 0.001);
    let QueryValue::Rect(svg_bounds) = engine.query(&frame, Query::SvgBBox(id(svg))).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(svg_bounds.origin.x, 10.0);
    assert!(svg_bounds.max_y() >= text_bounds.max_y());
    let QueryValue::Rect(text_client) =
        engine.query(&frame, Query::BoundingRect(id(text))).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(text_client.origin.x, 20.0);
    assert!((text_client.size.width - text_bounds.size.width * 2.0).abs() < 0.001);
    assert_eq!(empty.width, 0.0);
    assert_eq!(empty.actual_ascent, 0.0);
    assert_eq!(empty.font_ascent, result.font_ascent);
}

#[test]
fn orders_scrolling_per_owner_and_clamps_to_current_layout() {
    use squark_types::ScrollOffset;

    let mut fixture = Fixture::new(
        "body { margin: 0 }
         #scroll { width: 100px; height: calc(100vh - 400px); overflow: auto }
         #before { height: 140px } #target { height: 20px } #after { height: 70px }",
    );
    let scroller = fixture.element(BODY, "div", &[("id", "scroll")]);
    fixture.element(scroller, "div", &[("id", "before")]);
    let target = fixture.element(scroller, "div", &[("id", "target")]);
    fixture.element(scroller, "div", &[("id", "after")]);
    let pack = fixture.pack();
    let (send, _receiver) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let offset_at = |y, generation| ScrollOffset {
        owner: id(scroller),
        offset: Point { x: 0.0, y },
        generation,
    };
    let first_sync = [offset_at(45.0, 3)];
    let mut read = input();
    read.scroll = ScrollSync {
        frame: 3,
        tick: 1,
        offsets: &first_sync,
    };
    let geometry = |engine: &mut StaticDocument<'_>, frame: &FrameInput<'_>| {
        let QueryValue::Rect(rect) = engine
            .query(frame, Query::BoundingRect(id(target)))
            .unwrap()
        else {
            unreachable!()
        };
        rect.origin.y
    };
    let offset = |engine: &StaticDocument<'_>| {
        let frame = engine
            .snapshot()
            .0
            .nodes
            .values()
            .filter_map(|fragment| fragment.scroll)
            .find(|frame| frame.owner == id(scroller))
            .unwrap();
        (frame.offset.x, frame.offset.y, frame.generation)
    };
    assert_eq!(geometry(&mut engine, &read), 95.0);
    assert_eq!(offset(&engine), (0.0, 45.0, 3));

    flush(
        &mut engine,
        &[record(
            Opcode::ScrollTo,
            scroller,
            (-10.0f32).to_bits(),
            500.0f32.to_bits(),
        )],
    );
    assert_eq!(geometry(&mut engine, &read), -10.0);
    assert_eq!(offset(&engine), (0.0, 150.0, 4));
    // scrollIntoView computes its alignment in squark-dom and journals ScrollTo.
    flush(
        &mut engine,
        &[record(
            Opcode::ScrollTo,
            scroller,
            0.0f32.to_bits(),
            140.0f32.to_bits(),
        )],
    );
    assert_eq!(geometry(&mut engine, &read), 0.0);
    assert_eq!(offset(&engine), (0.0, 140.0, 5));

    let equal_sync = [offset_at(12.0, 5)];
    let mut equal = read.clone();
    equal.scroll = ScrollSync {
        frame: 9,
        tick: 9,
        offsets: &equal_sync,
    };
    assert_eq!(geometry(&mut engine, &equal), 0.0);
    let newer_sync = [offset_at(31.0, 6)];
    read.scroll.offsets = &newer_sync;
    assert_eq!(geometry(&mut engine, &read), 109.0);
    assert_eq!(offset(&engine), (0.0, 31.0, 6));

    read.tick.viewport.css.height = 620.0;
    read.tick.viewport.epoch = 2;
    assert_eq!(geometry(&mut engine, &read), 130.0);
    assert_eq!(offset(&engine), (0.0, 10.0, 7));
}

#[test]
fn zero_duration_keyframes_keep_the_underlying_style_and_emit_no_events() {
    let mut fixture = Fixture::new(
        "
        div { width: 20px; animation: size 0s both }
        @keyframes size { from { width: 30px } to { width: 40px } }
        ",
    );
    let target = fixture.element(BODY, "div", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut frame = input();
    for _ in 0..3 {
        let QueryValue::Rect(rect) = engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap()
        else {
            panic!("expected bounds");
        };
        assert_eq!(rect.size.width, 20.0);
        assert!(!engine.has_running_animations());
        assert!(engine.take_animation_events().is_empty());
        frame.tick.vsync += Duration::from_millis(16);
        frame.tick.sequence += 1;
    }
}

#[test]
fn hover_rules_use_the_pointer_capabilities_supplied_by_the_host() {
    use squark_types::{engine::PointerMedia, journal::ElementState};
    let mut fixture = Fixture::new(
        "
        @layer utilities {
            button { width: 20px }
            @media (hover: hover) { button:hover { width: 40px } }
        }
        ",
    );
    let target = fixture.element(BODY, "button", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    flush(
        &mut engine,
        &[record(
            Opcode::SetState,
            target,
            ElementState::HOVER.bits(),
            ElementState::HOVER.bits(),
        )],
    );
    let mut frame = input();
    for (hover, expected) in [(false, 20.0), (true, 40.0), (false, 20.0)] {
        frame.media.primary_pointer = PointerMedia::Fine;
        frame.media.primary_hover = hover;
        engine.set_media_state(&frame.tick.viewport, &frame.media);
        let QueryValue::Rect(rect) = engine
            .query(&frame, Query::BoundingRect(id(target)))
            .unwrap()
        else {
            panic!("expected bounds");
        };
        assert_eq!(rect.size.width, expected);
    }
}

#[test]
fn css_supports_uses_stylo_for_declarations_and_selectors() {
    let pack = Fixture::new("").pack();
    let (send, _updates) = resource_channel();
    let engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let supports = |text: &str| engine.css_supports(&text.encode_utf16().collect::<Vec<_>>());
    assert!(supports("(display: grid)"));
    assert!(!supports("(display: absent-layout-mode)"));
    assert!(supports("selector(figure:has(figcaption))"));
    assert!(!supports("(display: grid) nonsense"));
}

#[test]
#[ignore = "release timing probe"]
fn markdown_fragment_parse_timing() {
    use std::{fmt::Write, hint::black_box};

    let mut fixture = Fixture::new("");
    let preview = fixture.element(BODY, "article", &[]);
    let pack = fixture.pack();
    let (sender, _updates) = resource_channel();
    let engine =
        StaticDocument::from_pack(&pack, "https://app.test/".parse().unwrap(), sender).unwrap();
    let mut markup = String::new();
    for n in 0..400 {
        writeln!(
            markup,
            "<h2>Section {n}</h2>\n<p>Some <em>emphasis</em>, <strong>bold</strong> and <code>code</code> in paragraph {n}, plus <a href=\"https://example.com/{n}\">a link</a>.</p>\n<ul>\n<li>item a</li>\n<li>item b</li>\n</ul>"
        )
        .unwrap();
    }
    let markup: Vec<_> = markup.encode_utf16().collect();
    let mut timings = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        let parsed = engine
            .parse_fragment(id(preview), black_box(&markup))
            .unwrap();
        timings.push(start.elapsed());
        assert_eq!(black_box(parsed).len(), 2400);
    }
    timings.remove(0);
    timings.sort_unstable();
    eprintln!(
        "parse 3,600 elements, p50={:?}, p95={:?}",
        timings[timings.len() / 2],
        timings[timings.len() * 95 / 100]
    );
}

#[test]
fn measures_table_rows_and_sections_from_their_tracks() {
    let mut fixture = Fixture::new(
        "body { margin: 0 } table { border-spacing: 4px; transform: translateX(10px) }
         th, td { width: 60px; height: 20px; padding: 0 }",
    );
    let table = fixture.element(BODY, "table", &[]);
    let head = fixture.element(table, "thead", &[]);
    let heading = fixture.element(head, "tr", &[]);
    for text in ["x", "y"] {
        let cell = fixture.element(heading, "th", &[]);
        fixture.text(cell, text);
    }
    let body = fixture.element(table, "tbody", &[]);
    let first = fixture.element(body, "tr", &[]);
    let last = fixture.element(body, "tr", &[]);
    for row in [first, last] {
        let cell = fixture.element(row, "td", &[]);
        fixture.text(cell, "z");
    }
    let blank = fixture.element(body, "tr", &[]);
    let empty = fixture.element(table, "tbody", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    for (node, y, h) in [
        (head, 4.0, 20.0),
        (heading, 4.0, 20.0),
        (body, 28.0, 48.0),
        (first, 28.0, 20.0),
        (last, 52.0, 20.0),
        (blank, 76.0, 0.0),
        (empty, 76.0, 0.0),
    ] {
        let want = Rect::new(14.0, y, 124.0, h);
        let QueryValue::Rects(rects) = engine
            .query(&input(), Query::ClientRects(id(node)))
            .unwrap()
        else {
            panic!("expected client rects");
        };
        assert_eq!(rects, [want]);
        let QueryValue::Rect(bounds) = engine
            .query(&input(), Query::BoundingRect(id(node)))
            .unwrap()
        else {
            panic!("expected bounding rect");
        };
        assert_eq!(bounds, want);
    }
}

#[test]
fn applies_chromium_ua_list_markers_and_horizontal_rule_style() {
    let mut fixture = Fixture::new("body { color: red; margin: 0 }");
    let outer = fixture.element(BODY, "ul", &[]);
    let item = fixture.element(outer, "li", &[]);
    fixture.text(item, "outer");
    let middle = fixture.element(item, "menu", &[]);
    let item = fixture.element(middle, "li", &[]);
    fixture.text(item, "middle");
    let ordered = fixture.element(item, "ol", &[]);
    let item = fixture.element(ordered, "li", &[]);
    let inner = fixture.element(item, "ul", &[]);
    let inner_item = fixture.element(inner, "li", &[]);
    fixture.text(inner_item, "inner");
    let rule = fixture.element(BODY, "hr", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    for (node, name, expected) in [
        (outer, "list-style-type", "disc"),
        (middle, "list-style-type", "circle"),
        (ordered, "list-style-type", "decimal"),
        (inner_item, "list-style-type", "square"),
        (rule, "color", "rgb(128, 128, 128)"),
        (rule, "border-top-color", "rgb(128, 128, 128)"),
        (rule, "border-top-style", "inset"),
        (rule, "overflow", "hidden"),
    ] {
        let QueryValue::String(value) = engine
            .query(
                &input(),
                Query::ComputedStyle {
                    node: id(node),
                    pseudo: None,
                    property: name,
                },
            )
            .unwrap()
        else {
            panic!("expected computed style");
        };
        assert_eq!(value, expected);
    }
    let QueryValue::Rect(bounds) = engine
        .query(&input(), Query::BoundingRect(id(rule)))
        .unwrap()
    else {
        panic!("expected rule bounds");
    };
    assert_eq!(bounds.size.height, 2.0);
}

#[test]
fn resolves_pointer_events_keywords_through_variables_and_visibility() {
    use squark_types::fragment::{FragmentKind, PointerEvents, Visibility};

    let mut fixture = Fixture::new(
        "#parent { pointer-events: none }
         .hit { pointer-events: var(--clickable); width: 20px; height: 20px }
         .hidden { visibility: hidden }",
    );
    let parent = fixture.element(BODY, "div", &[("id", "parent")]);
    let all = fixture.element(
        parent,
        "div",
        &[("class", "hit"), ("style", "--clickable: all")],
    );
    let hidden = fixture.element(
        parent,
        "div",
        &[("class", "hit hidden"), ("style", "--clickable: all")],
    );
    let inherited = fixture.element(parent, "div", &[("class", "hit")]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    for keyword in [
        "visiblePainted",
        "visibleFill",
        "visibleStroke",
        "visible",
        "painted",
        "fill",
        "stroke",
        "all",
    ] {
        let condition: Vec<_> = format!("(pointer-events: {keyword})")
            .encode_utf16()
            .collect();
        assert!(engine.css_supports(&condition));
    }
    for (node, expected) in [
        (parent, "none"),
        (all, "all"),
        (hidden, "all"),
        (inherited, "none"),
    ] {
        let result = engine
            .query(
                &input(),
                Query::ComputedStyle {
                    node: id(node),
                    pseudo: None,
                    property: "pointer-events",
                },
            )
            .unwrap();
        assert_eq!(result, QueryValue::String(expected.into()));
    }
    let tree = engine.snapshot().0;
    let values = |node| {
        &tree
            .nodes
            .values()
            .find(|fragment| fragment.owner == id(node) && fragment.kind == FragmentKind::Box)
            .unwrap()
            .style
    };
    assert_eq!(values(all).interaction.pointer_events, PointerEvents::Auto);
    assert_eq!(values(all).stacking.visibility, Visibility::Visible);
    assert_eq!(values(hidden).stacking.visibility, Visibility::Hidden);
    assert_eq!(
        values(inherited).interaction.pointer_events,
        PointerEvents::None
    );
}

#[test]
fn links_inserted_after_load_read_same_origin_sheets_from_the_pack() {
    let mut f = Fixture::new("body { margin: 0 }");
    let loaded = f.element(
        BODY,
        "link",
        &[("rel", "stylesheet"), ("href", "/assets/late%20one.css")],
    );
    let missing = f.element(
        BODY,
        "link",
        &[("rel", "stylesheet"), ("href", "/assets/missing.css")],
    );
    let foreign = f.element(
        BODY,
        "link",
        &[
            ("rel", "stylesheet"),
            ("href", "https://cdn.test/assets/late%20one.css"),
        ],
    );
    let target = f.element(BODY, "div", &[("id", "target")]);
    let pack = f.pack_with_assets(&[(
        "/assets/late one.css",
        3,
        b"#target { width: 42px; height: 1px }",
    )]);
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let frame = input();
    let loaded_state = |engine: &mut StaticDocument, node| {
        engine.query(&frame, Query::SheetLoaded(id(node))).unwrap()
    };
    assert_eq!(loaded_state(&mut engine, loaded), QueryValue::Bool(true));
    assert_eq!(loaded_state(&mut engine, missing), QueryValue::Bool(false));
    assert_eq!(loaded_state(&mut engine, foreign), QueryValue::Bool(false));
    let QueryValue::Rect(rect) = engine
        .query(&frame, Query::BoundingRect(id(target)))
        .unwrap()
    else {
        panic!("expected target rect");
    };
    assert_eq!(rect.size.width, 42.0);
}

const TEXT_CSS: &str = "body { margin: 0; font: 16px/20px 'Liberation Mono', monospace }";

type Updates = std::sync::mpsc::Receiver<squark_types::render::ResourceMessage>;

fn text_engine(pack: &squark_assets::Pack) -> (StaticDocument<'_>, Updates) {
    let (send, updates) = resource_channel();
    let engine =
        StaticDocument::from_pack(pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    (engine, updates)
}

/// The painted text boxes of a Text node, in line order.
fn text_boxes(engine: &mut StaticDocument<'_>, node: u32) -> Vec<Rect> {
    engine.update_layout(&input()).unwrap();
    let mut boxes: Vec<_> = engine
        .snapshot()
        .0
        .nodes
        .values()
        .filter(|fragment| {
            fragment.owner == id(node)
                && fragment.kind == squark_types::fragment::FragmentKind::Text
        })
        .map(|fragment| fragment.border_box)
        .collect();
    boxes.sort_by(|a, b| a.origin.y.total_cmp(&b.origin.y));
    boxes
}

fn text_rects(
    engine: &mut StaticDocument<'_>,
    frame: &FrameInput<'_>,
    node: u32,
    range: [u32; 2],
) -> Vec<Rect> {
    match engine
        .query(
            frame,
            Query::TextRects {
                node: id(node),
                start_utf16: range[0],
                end_utf16: range[1],
            },
        )
        .unwrap()
    {
        QueryValue::Rects(rects) => rects,
        other => panic!("expected rects, got {other:?}"),
    }
}

fn caret_at(
    engine: &mut StaticDocument<'_>,
    frame: &FrameInput<'_>,
    hit: u32,
    x: f32,
    y: f32,
) -> Option<(u32, u32)> {
    match engine
        .query(
            frame,
            Query::CaretAtHit {
                hit: id(hit),
                point: Point { x, y },
            },
        )
        .unwrap()
    {
        QueryValue::Caret(caret) => caret.map(|(node, offset)| (node.raw(), offset)),
        other => panic!("expected a caret, got {other:?}"),
    }
}

#[track_caller]
fn assert_rects(actual: &[Rect], expected: &[Rect]) {
    let close = |a: &Rect, b: &Rect| {
        [
            (a.origin.x, b.origin.x),
            (a.origin.y, b.origin.y),
            (a.size.width, b.size.width),
            (a.size.height, b.size.height),
        ]
        .iter()
        .all(|(a, b)| (a - b).abs() < 0.01)
    };
    assert!(
        actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, b)| close(a, b)),
        "{actual:?} != {expected:?}"
    );
}

/// The advance of one character, checked against Liberation Mono's
/// 1229/2048 em where the test font is installed.
fn advance(text: Rect, chars: f32) -> f32 {
    let advance = text.size.width / chars;
    if !super::liberation_mono_missing_on_windows() {
        assert!((advance - 16.0 * 1229.0 / 2048.0).abs() < 0.01, "{advance}");
    }
    advance
}

#[test]
fn text_rects_cover_one_box_per_line_and_collapse_to_carets() {
    let mut f = Fixture::new(&format!(
        "{TEXT_CSS}
        #tall {{ width: 300px; padding: 10px; line-height: 40px }}
        #narrow {{ width: 60px }}
        #spans {{ width: 300px }}
        #gap {{ padding-left: 5px }}
        #spaces {{ width: 300px; padding-left: 20px }}
        #hidden {{ display: none }}"
    ));
    let tall = f.element(BODY, "div", &[("id", "tall")]);
    f.text(tall, "hello world");
    let narrow = f.element(BODY, "div", &[("id", "narrow")]);
    f.text(narrow, "hello world");
    let spans = f.element(BODY, "div", &[("id", "spans")]);
    f.text(spans, "ab");
    let gap = f.element(spans, "span", &[("id", "gap")]);
    f.text(gap, "cd");
    f.text(spans, "ef");
    let spaces = f.element(BODY, "div", &[("id", "spaces")]);
    f.text(spaces, "x   y");
    let hidden = f.element(BODY, "div", &[("id", "hidden")]);
    f.text(hidden, "gone");
    let pack = f.pack();
    let (mut engine, _updates) = text_engine(&pack);
    let frame = input();
    let [tall_text, narrow_text, ab, cd, x_y, gone] = [
        tall + 1,
        narrow + 1,
        spans + 1,
        gap + 1,
        spaces + 1,
        hidden + 1,
    ];

    let [text] = text_boxes(&mut engine, tall_text)[..] else {
        panic!("one text box");
    };
    let em = advance(text, 11.0);
    let (x, y, height) = (text.origin.x, text.origin.y, text.size.height);
    // Chromium sizes a range's rects, collapsed or not, to the text box of
    // the font rather than the 40px line box.
    assert!(height < 40.0 && y > 10.0);
    assert_rects(
        &text_rects(&mut engine, &frame, tall_text, [1, 4]),
        &[Rect::new(x + em, y, 3.0 * em, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, tall_text, [2, 2]),
        &[Rect::new(x + 2.0 * em, y, 0.0, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, tall_text, [11, 11]),
        &[Rect::new(x + 11.0 * em, y, 0.0, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, tall_text, [0, 99]),
        &[Rect::new(x, y, 11.0 * em, height)],
    );

    // The space a soft wrap leaves at the end of a line takes no width.
    let [first, second] = text_boxes(&mut engine, narrow_text)[..] else {
        panic!("two lines");
    };
    assert!(second.origin.y > first.origin.y);
    assert_rects(
        &text_rects(&mut engine, &frame, narrow_text, [0, 11]),
        &[
            Rect::new(first.origin.x, first.origin.y, 5.0 * em, height),
            Rect::new(second.origin.x, second.origin.y, 5.0 * em, height),
        ],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, narrow_text, [3, 8]),
        &[
            Rect::new(first.origin.x + 3.0 * em, first.origin.y, 2.0 * em, height),
            Rect::new(second.origin.x, second.origin.y, 2.0 * em, height),
        ],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, narrow_text, [5, 5]),
        &[Rect::new(
            first.origin.x + 5.0 * em,
            first.origin.y,
            0.0,
            height,
        )],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, narrow_text, [6, 6]),
        &[Rect::new(second.origin.x, second.origin.y, 0.0, height)],
    );

    // A caret at the end of a node stays before the next inline's padding.
    let line = text_boxes(&mut engine, ab)[0];
    assert_rects(
        &text_rects(&mut engine, &frame, ab, [2, 2]),
        &[Rect::new(
            line.origin.x + 2.0 * em,
            line.origin.y,
            0.0,
            height,
        )],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, cd, [0, 0]),
        &[Rect::new(
            line.origin.x + 2.0 * em + 5.0,
            line.origin.y,
            0.0,
            height,
        )],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, cd, [0, 2]),
        &[Rect::new(
            line.origin.x + 2.0 * em + 5.0,
            line.origin.y,
            2.0 * em,
            height,
        )],
    );

    // Collapsed white space maps to the one space that renders.
    let line = text_boxes(&mut engine, x_y)[0];
    assert_eq!(line.origin.x, 20.0);
    assert_rects(
        &text_rects(&mut engine, &frame, x_y, [1, 4]),
        &[Rect::new(20.0 + em, line.origin.y, em, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, x_y, [2, 4]),
        &[Rect::new(20.0 + 2.0 * em, line.origin.y, 0.0, height)],
    );

    assert!(text_rects(&mut engine, &frame, gone, [0, 4]).is_empty());
    assert!(text_rects(&mut engine, &frame, spans, [0, 1]).is_empty());
    flush(&mut engine, &[record(Opcode::Remove, tall, tall_text, 0)]);
    assert!(text_rects(&mut engine, &frame, tall_text, [0, 4]).is_empty());
}

#[test]
fn text_rects_and_carets_follow_transforms_and_scroll_offsets() {
    let mut f = Fixture::new(&format!(
        "{TEXT_CSS}
        #box {{ position: absolute; left: 400px; top: 0; width: 100px;
                transform: translate(10px, 20px) scale(2); transform-origin: 0 0 }}
        #scroller {{ position: absolute; left: 400px; top: 200px; width: 200px; height: 40px;
                     overflow: auto }}
        #spacer {{ height: 60px }}
        #after {{ height: 100px }}"
    ));
    let transformed = f.element(BODY, "div", &[("id", "box")]);
    f.text(transformed, "abcdef");
    let scroller = f.element(BODY, "div", &[("id", "scroller")]);
    f.element(scroller, "div", &[("id", "spacer")]);
    let span = f.element(scroller, "span", &[]);
    f.text(span, "scrolled text");
    f.element(scroller, "div", &[("id", "after")]);
    let pack = f.pack();
    let (mut engine, _updates) = text_engine(&pack);
    let offsets = [squark_types::ScrollOffset {
        owner: id(scroller),
        offset: Point { x: 0.0, y: 50.0 },
        generation: 1,
    }];
    let mut frame = input();
    frame.scroll = ScrollSync {
        frame: 2,
        tick: 3,
        offsets: &offsets,
    };

    let [text] = text_boxes(&mut engine, transformed + 1)[..] else {
        panic!("one text box");
    };
    let em = advance(text, 6.0);
    assert_eq!(text.origin.x, 400.0);
    let client = |x: f32, y: f32| (410.0 + 2.0 * (x - 400.0), 20.0 + 2.0 * y);
    let (left, top) = client(text.origin.x + em, text.origin.y);
    assert_rects(
        &text_rects(&mut engine, &frame, transformed + 1, [1, 3]),
        &[Rect::new(left, top, 4.0 * em, 2.0 * text.size.height)],
    );
    let middle = text.origin.y + text.size.height / 2.0;
    for (column, offset) in [(1.3, 1), (1.7, 2)] {
        let (x, y) = client(400.0 + column * em, middle);
        assert_eq!(
            caret_at(&mut engine, &frame, transformed, x, y),
            Some((transformed + 1, offset))
        );
    }

    let [text] = text_boxes(&mut engine, span + 1)[..] else {
        panic!("one text box");
    };
    assert!(text.origin.y > 250.0);
    let top = text.origin.y - 50.0;
    assert_rects(
        &text_rects(&mut engine, &frame, span + 1, [0, 3]),
        &[Rect::new(400.0, top, 3.0 * em, text.size.height)],
    );
    for (hit, column, offset) in [(span, 1.3, 1), (scroller, 4.6, 5)] {
        assert_eq!(
            caret_at(
                &mut engine,
                &frame,
                hit,
                400.0 + column * em,
                top + text.size.height / 2.0
            ),
            Some((span + 1, offset))
        );
    }
}

#[test]
fn caret_positions_round_to_glyph_edges_and_clamp_to_line_boundaries() {
    let mut f = Fixture::new(&format!(
        "{TEXT_CSS}
        #tall {{ width: 300px; padding: 10px; line-height: 40px }}
        #narrow {{ width: 60px }}
        #spans {{ width: 300px }}
        #gap {{ padding-left: 5px }}
        #empty {{ width: 50px; height: 30px }}
        #outer {{ padding: 10px 0 30px }}
        p {{ margin: 0 0 20px }}
        #block {{ display: inline-block; width: 30px; height: 10px }}
        canvas {{ width: 20px; height: 10px }}"
    ));
    let tall = f.element(BODY, "div", &[("id", "tall")]);
    f.text(tall, "hello world");
    let narrow = f.element(BODY, "div", &[("id", "narrow")]);
    f.text(narrow, "hello world");
    let spans = f.element(BODY, "div", &[("id", "spans")]);
    f.text(spans, "ab");
    let gap = f.element(spans, "span", &[("id", "gap")]);
    f.text(gap, "cd");
    f.text(spans, "ef");
    f.element(spans, "br", &[]);
    f.text(spans, "gh");
    let empty = f.element(BODY, "div", &[("id", "empty")]);
    let objects = f.element(BODY, "div", &[]);
    f.text(objects, "ab");
    let canvas = f.element(objects, "canvas", &[]);
    f.text(objects, "cd");
    let block = f.element(objects, "span", &[("id", "block")]);
    let outer = f.element(BODY, "div", &[("id", "outer")]);
    let first = f.element(outer, "p", &[]);
    f.text(first, "first para");
    let second = f.element(outer, "p", &[]);
    f.text(second, "second");
    let pack = f.pack();
    let (mut engine, _updates) = text_engine(&pack);
    let frame = input();
    let [text] = text_boxes(&mut engine, tall + 1)[..] else {
        panic!("one text box");
    };
    let em = advance(text, 11.0);
    let tree = engine.snapshot().0;
    let border_box = |node| {
        tree.nodes
            .values()
            .find(|fragment| {
                fragment.owner == id(node)
                    && matches!(
                        fragment.kind,
                        squark_types::fragment::FragmentKind::Box
                            | squark_types::fragment::FragmentKind::Replaced
                    )
            })
            .unwrap()
            .border_box
    };
    let canvas_box = border_box(canvas);
    let outer_top = border_box(outer).origin.y;
    let mut at = |hit, x, y| caret_at(&mut engine, &frame, hit, x, y);

    // Over a glyph the caret goes to its nearer edge.
    let middle = text.origin.y + text.size.height / 2.0;
    for (column, offset) in [(0.2, 0), (1.3, 1), (1.7, 2), (4.49, 4), (10.6, 11)] {
        assert_eq!(
            at(tall, 10.0 + column * em, middle),
            Some((tall + 1, offset))
        );
    }
    // Beside a line the caret takes its nearer end, and past the top or
    // bottom of the paragraph its first or last position.
    assert_eq!(at(tall, 5.0, middle), Some((tall + 1, 0)));
    assert_eq!(at(tall, 250.0, middle), Some((tall + 1, 11)));
    assert_eq!(at(tall, 250.0, 12.0), Some((tall + 1, 11)));
    assert_eq!(at(tall, 30.0, 2.0), Some((tall + 1, 0)));
    assert_eq!(at(tall, 30.0, 55.0), Some((tall + 1, 11)));

    // Right of a soft-wrapped line is before the space the wrap removed.
    let narrow_lines = [70.0, 90.0];
    assert_eq!(at(narrow, 55.0, narrow_lines[0]), Some((narrow + 1, 5)));
    assert_eq!(at(narrow, 55.0, narrow_lines[1]), Some((narrow + 1, 11)));
    assert_eq!(at(narrow, 2.0, narrow_lines[1]), Some((narrow + 1, 6)));

    // Inline padding belongs to the text after it, and a forced break ends
    // the line at the text before it.
    let lines = [110.0, 130.0];
    let [ab, cd, ef, gh] = [spans + 1, gap + 1, gap + 2, gap + 4];
    assert_eq!(at(spans, 1.7 * em, lines[0]), Some((ab, 2)));
    for hit in [spans, gap] {
        assert_eq!(at(hit, 2.0 * em + 2.0, lines[0]), Some((cd, 0)));
    }
    assert_eq!(at(gap, 2.0 * em + 5.0 + 1.6 * em, lines[0]), Some((cd, 2)));
    assert_eq!(at(spans, 250.0, lines[0]), Some((ef, 2)));
    assert_eq!(at(spans, 250.0, lines[1]), Some((gh, 2)));
    assert_eq!(at(spans, 2.0, lines[1]), Some((gh, 0)));

    // Elements without text give a child index.
    assert_eq!(at(empty, 25.0, 155.0), Some((empty, 0)));
    let line = canvas_box.origin.y + canvas_box.size.height / 2.0;
    let canvas_start = 2.0 * em;
    assert_eq!(canvas_box.origin.x, canvas_start);
    assert_eq!(at(canvas, canvas_start + 4.0, line), Some((objects, 1)));
    assert_eq!(at(canvas, canvas_start + 16.0, line), Some((objects, 2)));
    assert_eq!(at(objects, canvas_start + 16.0, line), Some((objects, 2)));
    let block_start = canvas_start + 20.0 + 2.0 * em;
    assert_eq!(at(block, block_start + 5.0, line), Some((block, 0)));
    assert_eq!(at(objects, block_start + 25.0, line), Some((block, 0)));

    // Block children: the first one that ends below the point.
    assert_eq!(at(outer, 30.0, outer_top + 5.0), Some((first + 1, 0)));
    assert_eq!(at(outer, 30.0, outer_top + 35.0), Some((second + 1, 0)));
    assert_eq!(at(outer, 30.0, outer_top + 45.0), Some((second + 1, 0)));
    assert_eq!(at(outer, 500.0, outer_top + 80.0), Some((second + 1, 6)));
    // A point that misses every element resolves against the document.
    assert_eq!(at(1, 30.0, 470.0), Some((second + 1, 6)));
}

#[test]
fn measures_and_hits_editor_tokens_preserved_spaces_and_rtl_lines() {
    let mut f = Fixture::new(&format!(
        "{TEXT_CSS}
        .lines {{ position: absolute; left: 30px; top: 0; width: 400px; height: 100px;
                  transform: translate3d(0px, 0px, 0px); contain: strict }}
        .line {{ position: absolute; left: 0; width: 100%; height: 19px; line-height: 19px;
                 white-space: pre }}
        #wrap {{ position: absolute; top: 100px; width: 60px; white-space: pre-wrap }}
        #rtl {{ position: absolute; top: 200px; width: 300px; direction: rtl }}"
    ));
    // Monaco's view lines: a line span of token spans in a pre line.
    let lines = f.element(BODY, "div", &[("class", "lines")]);
    let mut tokens = Vec::new();
    for (top, words) in [(0, ["const", " x = ", "1;"]), (19, ["let", "  ", "y"])] {
        let line = f.element(
            lines,
            "div",
            &[("class", "line"), ("style", &format!("top: {top}px"))],
        );
        let content = f.element(line, "span", &[]);
        for word in words {
            let token = f.element(content, "span", &[("class", "token")]);
            f.text(token, word);
            tokens.push(token + 1);
        }
        tokens.push(line);
    }
    let wrap = f.element(BODY, "div", &[("id", "wrap")]);
    f.text(wrap, "hello world");
    let rtl = f.element(BODY, "div", &[("id", "rtl")]);
    f.text(rtl, "abc");
    let pack = f.pack();
    let (mut engine, _updates) = text_engine(&pack);
    let frame = input();
    let [konst, assign, one, first_line, _, spaces, y, second_line] = tokens[..] else {
        unreachable!()
    };
    let [text] = text_boxes(&mut engine, konst)[..] else {
        panic!("one text box");
    };
    let em = advance(text, 5.0);
    let (top, height) = (text.origin.y, text.size.height);
    assert_eq!(text.origin.x, 30.0);
    assert_rects(
        &text_rects(&mut engine, &frame, assign, [0, 5]),
        &[Rect::new(30.0 + 5.0 * em, top, 5.0 * em, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, assign, [1, 2]),
        &[Rect::new(30.0 + 6.0 * em, top, em, height)],
    );
    assert_rects(
        &text_rects(&mut engine, &frame, spaces, [0, 2]),
        &[Rect::new(30.0 + 3.0 * em, top + 19.0, 2.0 * em, height)],
    );
    let middle = top + height / 2.0;
    assert_eq!(
        caret_at(&mut engine, &frame, one - 1, 30.0 + 10.3 * em, middle),
        Some((one, 0))
    );
    assert_eq!(
        caret_at(&mut engine, &frame, one - 1, 30.0 + 10.6 * em, middle),
        Some((one, 1))
    );
    assert_eq!(
        caret_at(&mut engine, &frame, assign - 1, 30.0 + 9.7 * em, middle),
        Some((assign, 5))
    );
    assert_eq!(
        caret_at(&mut engine, &frame, first_line, 300.0, middle),
        Some((one, 2))
    );
    assert_eq!(
        caret_at(
            &mut engine,
            &frame,
            second_line,
            30.0 + 4.4 * em,
            middle + 19.0
        ),
        Some((spaces, 1))
    );
    assert_eq!(
        caret_at(&mut engine, &frame, second_line, 300.0, middle + 19.0),
        Some((y, 1))
    );

    // A preserved space hangs past a soft wrap with its width, in a box of
    // its own, and the caret can go after it.
    let [first, second] = text_boxes(&mut engine, wrap + 1)[..] else {
        panic!("two lines");
    };
    assert_rects(
        &text_rects(&mut engine, &frame, wrap + 1, [0, 11]),
        &[
            Rect::new(0.0, first.origin.y, 5.0 * em, height),
            Rect::new(5.0 * em, first.origin.y, em, height),
            Rect::new(0.0, second.origin.y, 5.0 * em, height),
        ],
    );
    assert_eq!(
        caret_at(&mut engine, &frame, wrap, 59.0, 110.0),
        Some((wrap + 1, 6))
    );

    // Left of an RTL line is its end.
    let [text] = text_boxes(&mut engine, rtl + 1)[..] else {
        panic!("one text box");
    };
    assert!((text.max_x() - 300.0).abs() < 0.01);
    assert_rects(
        &text_rects(&mut engine, &frame, rtl + 1, [0, 1]),
        &[Rect::new(300.0 - 3.0 * em, text.origin.y, em, height)],
    );
    assert_eq!(
        caret_at(&mut engine, &frame, rtl, 10.0, 210.0),
        Some((rtl + 1, 3))
    );
    assert_eq!(
        caret_at(&mut engine, &frame, rtl, 300.0 - 0.6 * em, 210.0),
        Some((rtl + 1, 2))
    );
}
