// SPDX-License-Identifier: MIT

use std::time::Instant;

use squark_assets::Pack;
use squark_journal::{BatchDecoder, BatchWriter};
use squark_layout::StaticDocument;
use squark_types::{
    NodeId, Point, Rect,
    engine::{DecodedBatch, DocumentEngine, FrameInput, Query, QueryValue, SelectMode},
    fragment::{
        DecorationKind, Fragment, FragmentContent, FragmentKind, FragmentTree, PaintColor,
        PathCommand, SvgPaint,
    },
    journal::{ElementState as State, FOCUS_PREVENT_SCROLL, Opcode, SELECTION_BACKWARD},
    render::{FrameTick, Viewport, resource_channel},
};

use super::{BODY, Fixture, VIEWPORT, layout};

fn box_fragment(tree: &FragmentTree, owner: u32) -> &Fragment {
    tree.nodes
        .values()
        .find(|f| {
            f.owner.raw() == owner && matches!(f.kind, FragmentKind::Replaced | FragmentKind::Box)
        })
        .unwrap()
}

fn glyphs(tree: &FragmentTree, owner: u32) -> Vec<u32> {
    tree.nodes
        .values()
        .filter(|f| f.owner.raw() == owner)
        .flat_map(|f| match &f.content {
            FragmentContent::Text(runs) => runs.as_ref(),
            _ => &[],
        })
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
        .collect()
}

fn frame() -> FrameInput<'static> {
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

fn decoder(pack: &Pack) -> BatchDecoder {
    let document = pack.document().unwrap();
    let mut decoder = BatchDecoder::new(document.id).unwrap();
    for bytes in document.batches() {
        decoder.validate(bytes).unwrap().commit();
    }
    decoder
}

fn apply(engine: &mut StaticDocument<'_>, decoder: &mut BatchDecoder, writer: BatchWriter) {
    for bytes in writer.finish() {
        let batch = decoder.validate(&bytes).unwrap();
        let records: Vec<_> = batch.records().collect();
        let strings = batch.strings();
        let storage: Vec<Vec<_>> = (0..strings.len())
            .map(|index| strings.get(index as u32).unwrap().units().collect())
            .collect();
        let strings: Vec<_> = storage.iter().map(Vec::as_slice).collect();
        engine
            .flush(DecodedBatch {
                records: &records,
                strings: &strings,
                staged_bytes: &[],
            })
            .unwrap();
        batch.commit();
    }
}

fn caret(engine: &mut StaticDocument<'_>, field: u32, offset: u32) -> Rect {
    let QueryValue::Rect(rect) = engine
        .query(&frame(), Query::CaretRect(NodeId::from_raw(field), offset))
        .unwrap()
    else {
        panic!("expected caret rect")
    };
    rect
}

#[test]
fn paints_preedit_and_selection_and_round_trips_utf16_positions() {
    let mut fixture = Fixture::new("
        body { margin: 0 }
        input { font: 20px monospace; width: 200px; caret-color: green; transform: translate(7px, 9px) }
        input::selection { background-color: red; color: yellow }
    ");
    let field = fixture.element(BODY, "input", &[("value", "A😀BC")]);
    let empty = fixture.element(BODY, "textarea", &[("placeholder", "Write here")]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut journal = decoder(&pack);
    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::SetSelection, SELECTION_BACKWARD, field, 1, 3)
        .unwrap();
    writer
        .record(Opcode::SetValue, 0, field, "A😀BC", 0)
        .unwrap();
    writer
        .record(Opcode::Focus, 0, field, FOCUS_PREVENT_SCROLL, 0)
        .unwrap();
    apply(&mut engine, &mut journal, writer);

    let start = caret(&mut engine, field, 1);
    let end = caret(&mut engine, field, 3);
    assert!(start.origin.y >= 9.0 && start.origin.x >= 7.0);
    assert!(end.origin.x > start.origin.x + 6.0);
    let offset = |engine: &mut StaticDocument<'_>, rect: Rect| {
        engine
            .query(
                &frame(),
                Query::OffsetFromPoint(
                    NodeId::from_raw(field),
                    Point {
                        x: rect.origin.x,
                        y: rect.origin.y + rect.size.height / 2.0,
                    },
                ),
            )
            .unwrap()
    };
    assert_eq!(offset(&mut engine, start), QueryValue::Number(1.0));
    assert_eq!(offset(&mut engine, end), QueryValue::Number(3.0));
    let has_fill = |tree: &FragmentTree, color| {
        tree.nodes.values().any(|fragment| {
            fragment.owner.raw() == field
                && matches!(&fragment.content, FragmentContent::Svg(geometry)
                if geometry.paths[0].fill == Some(SvgPaint::Color(color)))
        })
    };
    assert!(has_fill(
        engine.snapshot().0,
        PaintColor::srgb(1.0, 0.0, 0.0, 1.0)
    ));
    assert!(engine.snapshot().0.nodes.values().any(|fragment| {
        fragment.owner.raw() == field
            && fragment.style.art.color == PaintColor::srgb(1.0, 1.0, 0.0, 1.0)
            && matches!(&fragment.content, FragmentContent::Text(runs) if runs.iter().any(|run| !run.glyphs.is_empty()))
    }));
    let base = glyphs(engine.snapshot().0, field);

    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::SetPreedit, 0, field, "xy", 1)
        .unwrap();
    writer
        .record(Opcode::SetValue, 0, field, "A😀BC", 0)
        .unwrap();
    apply(&mut engine, &mut journal, writer);
    let cursor = caret(&mut engine, field, 1);
    assert!(cursor.origin.x > start.origin.x);
    assert_eq!(offset(&mut engine, cursor), QueryValue::Number(1.0));
    let tree = engine.snapshot().0;
    assert_ne!(glyphs(tree, field), base);
    assert!(has_fill(
        tree,
        PaintColor::srgb(0.0, 128.0 / 255.0, 0.0, 1.0)
    ));
    assert!(
        tree.nodes
            .values()
            .any(|fragment| matches!(&fragment.content,
        FragmentContent::Text(runs) if runs.iter().flat_map(|r| r.decoration_spans.iter())
            .any(|span| span.kind == DecorationKind::Underline)))
    );

    let mut writer = BatchWriter::new();
    writer.record(Opcode::SetPreedit, 0, field, "", 0).unwrap();
    apply(&mut engine, &mut journal, writer);
    assert_eq!(caret(&mut engine, field, 1), start);
    assert_eq!(glyphs(engine.snapshot().0, field), base);
    let empty_caret = caret(&mut engine, empty, 0);
    assert!(empty_caret.size.height > 0.0);
    assert_eq!(
        engine
            .query(
                &frame(),
                Query::OffsetFromPoint(
                    NodeId::from_raw(empty),
                    Point {
                        x: empty_caret.origin.x + 24.0,
                        y: empty_caret.origin.y + 4.0,
                    }
                )
            )
            .unwrap(),
        QueryValue::Number(0.0)
    );
}

#[test]
fn reveals_the_caret_in_scrolled_inputs_and_wrapped_textareas() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        input, textarea { width: 95px; height: 34px; font: 16px/18px monospace }
        textarea { box-sizing: border-box }
    ",
    );
    let input = fixture.element(BODY, "input", &[]);
    let textarea = fixture.element(BODY, "textarea", &[]);
    let unwrapped = fixture.element(BODY, "textarea", &[("wrap", "off")]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut journal = decoder(&pack);
    let value = "one two three four five six seven eight nine ten";
    let end = value.len() as u32;
    for (field, want_horizontal) in [(input, true), (textarea, false), (unwrapped, true)] {
        let mut writer = BatchWriter::new();
        writer.record(Opcode::SetValue, 0, field, value, 0).unwrap();
        writer
            .record(Opcode::SetSelection, 0, field, end, end)
            .unwrap();
        writer
            .record(Opcode::Focus, 0, field, FOCUS_PREVENT_SCROLL, 0)
            .unwrap();
        apply(&mut engine, &mut journal, writer);
        let last = caret(&mut engine, field, end);
        let scroll = box_fragment(engine.snapshot().0, field).scroll.unwrap();
        assert!(scroll.generation > 0);
        assert_eq!(scroll.offset.x > 0.0, want_horizontal);
        assert_eq!(scroll.offset.y > 0.0, !want_horizontal);
        assert!(last.max_x() <= scroll.scrollport.max_x() + 0.01);
        assert!(
            last.max_y() <= scroll.scrollport.max_y() + 0.01,
            "{field}: caret {last:?} exceeded {scroll:?}"
        );
        assert_eq!(
            engine
                .query(
                    &frame(),
                    Query::OffsetFromPoint(
                        NodeId::from_raw(field),
                        Point {
                            x: last.origin.x,
                            y: last.origin.y + last.size.height / 2.0,
                        }
                    )
                )
                .unwrap(),
            QueryValue::Number(f64::from(end))
        );

        let mut writer = BatchWriter::new();
        writer.record(Opcode::ScrollTo, 0, field, 0, 0).unwrap();
        apply(&mut engine, &mut journal, writer);
        let first = caret(&mut engine, field, 0);
        let last = caret(&mut engine, field, end);
        if want_horizontal {
            assert!(last.origin.x > first.origin.x + 100.0);
        } else {
            assert!(last.origin.y > first.origin.y + 36.0);
        }
    }
}

#[test]
fn measures_control_intrinsic_sizes_and_respects_appearance_none() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        .row { display: flex; align-items: start; gap: 6px }
        .custom { appearance: none; width: 38px; height: 17px; border: 1px solid red }
    ",
    );
    let row = fixture.element(BODY, "div", &[("class", "row")]);
    let input = fixture.element(row, "input", &[("value", "Hello")]);
    let smaller = fixture.element(row, "input", &[("size", "10")]);
    let button = fixture.element(row, "button", &[]);
    fixture.text(button, "Action");
    let checkbox = fixture.element(row, "input", &[("type", "checkbox"), ("checked", "")]);
    let radio = fixture.element(row, "input", &[("type", "radio")]);
    let custom = fixture.element(
        row,
        "input",
        &[("type", "checkbox"), ("checked", ""), ("class", "custom")],
    );
    let plain = fixture.element(
        row,
        "input",
        &[("type", "radio"), ("style", "appearance:none")],
    );
    let select = fixture.element(BODY, "select", &[]);
    let option = fixture.element(select, "option", &[]);
    fixture.text(option, "First");
    let wide_option = fixture.element(select, "option", &[]);
    fixture.text(wide_option, "Longer Option");
    let tree = layout(&fixture.pack());
    let size = |id| {
        let r = box_fragment(&tree, id).border_box;
        (r.size.width, r.size.height)
    };
    assert_eq!(size(checkbox), (13.0, 13.0));
    assert_eq!(size(radio), (13.0, 13.0));
    assert_eq!(size(custom), (38.0, 17.0));
    assert_eq!(size(plain), (0.0, 0.0));
    assert!(matches!(
        box_fragment(&tree, checkbox).content,
        FragmentContent::Svg(_)
    ));
    assert!(matches!(
        box_fragment(&tree, custom).content,
        FragmentContent::None
    ));
    let (w, h) = size(input);
    let (w_small, h_small) = size(smaller);
    #[cfg(target_os = "macos")]
    assert_eq!((w, w_small), (153.0, 83.0));
    // Pinned Chromium 159.0.8154.6 on Windows x64 (13.3333px Arial).
    #[cfg(target_os = "windows")]
    assert_eq!((w, w_small), (177.0, 107.0));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    assert!(
        (183.0..=187.0).contains(&w) && (103.0..=107.0).contains(&w_small),
        "{w}, {w_small}"
    );
    assert_eq!((h, h_small), (21.0, 21.0));
    let (w, h) = size(button);
    assert!(
        (52.0..=54.0).contains(&w) && (20.0..=22.0).contains(&h),
        "{w}, {h}"
    );
    assert_eq!(size(select), (107.0, 19.0));
    assert_eq!(glyphs(&tree, select).len(), "First".len());
}

#[cfg(target_os = "macos")]
#[test]
fn matches_chromium_text_input_widths_across_sizes_and_fonts_on_macos() {
    // Expected border-box sizes measured directly in pinned Chromium
    // 159.0.8154.6 (`scripts/chromium-path.mjs`) via Playwright on macOS arm64.
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        .row { display: flex; align-items: start; gap: 6px }
        .arial16 { font: 16px Arial }
        .helvetica { font: 13.3333px Helvetica }
    ",
    );
    let row = fixture.element(BODY, "div", &[("class", "row")]);
    let size5 = fixture.element(row, "input", &[("size", "5")]);
    let arial16_20 = fixture.element(row, "input", &[("class", "arial16")]);
    let arial16_5 = fixture.element(row, "input", &[("size", "5"), ("class", "arial16")]);
    let helv_20 = fixture.element(row, "input", &[("class", "helvetica")]);
    let helv_5 = fixture.element(row, "input", &[("size", "5"), ("class", "helvetica")]);
    let tree = layout(&fixture.pack());
    let size = |id| {
        let r = box_fragment(&tree, id).border_box;
        (r.size.width, r.size.height)
    };
    assert_eq!(size(size5), (48.0, 21.0));
    assert_eq!(size(arial16_20), (174.0, 24.0));
    assert_eq!(size(arial16_5), (54.0, 24.0));
    // Helvetica is in Blink's invalid-avg-char-width list, so widths fall back
    // to `'0'` advance with no extra max_char_width allowance.
    assert_eq!(size(helv_20).0, 157.0);
    assert_eq!(size(helv_5).0, 46.0);
}

#[cfg(target_os = "windows")]
#[test]
fn matches_chromium_text_input_widths_across_sizes_and_fonts_on_windows() {
    // Expected border-box widths measured directly in pinned Chromium
    // 159.0.8154.6 (`scripts/chromium-path.mjs`) via Playwright on Windows x64,
    // with the fonts Windows ships. Each is 'x' advance * size plus the head
    // x range minus the 'x' advance, plus 8px of padding and border.
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        .row { display: flex; align-items: start; gap: 6px; flex-wrap: wrap }
        .arial16 { font: 16px Arial }
        .times { font: 13.3333px 'Times New Roman' }
        .consolas { font: 14px Consolas }
        .courier { font: 13.3333px 'Courier New' }
    ",
    );
    let row = fixture.element(BODY, "div", &[("class", "row")]);
    let cases = [
        ("", "5", 72.0),
        ("arial16", "20", 203.0),
        ("arial16", "5", 83.0),
        ("times", "20", 176.0),
        ("times", "5", 71.0),
        ("consolas", "20", 176.0),
        ("consolas", "5", 56.0),
        ("courier", "20", 170.0),
    ];
    let inputs: Vec<_> = cases
        .iter()
        .map(|(class, size, _)| fixture.element(row, "input", &[("class", class), ("size", size)]))
        .collect();
    let tree = layout(&fixture.pack());
    let widths: Vec<_> = inputs
        .iter()
        .map(|&id| box_fragment(&tree, id).border_box.size.width)
        .collect();
    let expected: Vec<_> = cases.iter().map(|case| case.2).collect();
    assert_eq!(widths, expected);
}

#[test]
fn authored_line_height_preserves_centered_control_baseline() {
    // Measured in pinned Chromium 159.0.8154.6 (`scripts/chromium-path.mjs`) on
    // macOS arm64: for `font: 14px/40px Arial; padding: 0; border: 0; appearance: none`
    // in `display: flex; align-items: baseline`, a 10x10 sibling box sits at
    // y = 15px (bottom / baseline = 25px) beside both `<input>` and
    // `<input type="submit">`.
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        .row { display: flex; align-items: baseline }
        input {
            font: 14px/40px Arial, Liberation Sans, sans-serif;
            padding: 0;
            border: 0;
            margin: 0;
            appearance: none;
        }
        .marker { width: 10px; height: 10px }
    ",
    );
    let input_row = fixture.element(BODY, "div", &[("class", "row")]);
    let input = fixture.element(input_row, "input", &[("value", "A")]);
    let input_marker = fixture.element(input_row, "div", &[("class", "marker")]);

    let submit_row = fixture.element(BODY, "div", &[("class", "row")]);
    let submit = fixture.element(submit_row, "input", &[("type", "submit"), ("value", "A")]);
    let submit_marker = fixture.element(submit_row, "div", &[("class", "marker")]);

    let tree = layout(&fixture.pack());
    for (label, control, marker) in [
        ("input[type=text]", input, input_marker),
        ("input[type=submit]", submit, submit_marker),
    ] {
        let control_box = box_fragment(&tree, control).border_box;
        let marker_box = box_fragment(&tree, marker).border_box;
        assert_eq!(control_box.size.height, 40.0, "{label} height");
        // The 10x10 marker has no text, so its baseline is its bottom edge.
        // Its distance from the control's top equals the baseline squark-layout reported.
        let baseline = (marker_box.origin.y + marker_box.size.height) - control_box.origin.y;
        assert!(
            (24.0..=26.0).contains(&baseline),
            "{label} baseline should include half-leading (~25px in Chromium), got {baseline}"
        );
        // The text fragment inside the 40px control must be vertically centered
        // by half-leading (~12px below the control's top), not pinned near y = 0.
        let text_fragment = tree
            .nodes
            .values()
            .find(|f| f.owner.raw() == control && matches!(f.content, FragmentContent::Text(_)))
            .unwrap();
        let text_offset = text_fragment.border_box.origin.y - control_box.origin.y;
        assert!(
            (10.0..=14.0).contains(&text_offset),
            "{label} inner text offset should be centered (~12px half-leading), got {text_offset}"
        );
    }
}

#[test]
fn shapes_set_value_and_masks_passwords_without_moving_following_boxes() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        #row { display: flex; align-items: start; gap: 6px }
        input { width: 120px; font: 16px/19px Arial, Liberation Sans, sans-serif; appearance: none }
    ",
    );
    let row = fixture.element(BODY, "div", &[("id", "row")]);
    let input = fixture.element(row, "input", &[("value", "Old")]);
    let expected = fixture.element(row, "input", &[("value", "New!")]);
    let password = fixture.element(row, "input", &[("type", "password"), ("value", "gyp")]);
    let bullets = fixture.element(row, "input", &[("value", "•••")]);
    let pack = fixture.pack();
    let (sender, _receiver) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://forms.test/".parse().unwrap(), sender).unwrap();
    let mut decoder = decoder(&pack);
    let input_frame = frame();
    engine.update_layout(&input_frame).unwrap();
    let (before, _) = engine.snapshot();
    assert_eq!(glyphs(before, input).len(), 3);
    assert_eq!(glyphs(before, password), glyphs(before, bullets));
    let following = box_fragment(before, expected).border_box;
    let mut value = BatchWriter::new();
    value.record(Opcode::SetValue, 0, input, "New!", 0).unwrap();
    apply(&mut engine, &mut decoder, value);
    engine.update_layout(&input_frame).unwrap();
    let (updated, _) = engine.snapshot();
    assert_eq!(glyphs(updated, input), glyphs(updated, expected));
    assert_eq!(box_fragment(updated, expected).border_box, following);
    let mut value = BatchWriter::new();
    value.record(Opcode::SetValue, 0, input, "", 0).unwrap();
    apply(&mut engine, &mut decoder, value);
    engine.update_layout(&input_frame).unwrap();
    assert!(glyphs(engine.snapshot().0, input).is_empty());
}

#[test]
fn restyles_control_pseudo_classes_from_masked_journal_state() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        input { appearance: none; height: 15px; width: 20px; }
        input:hover { width: 31px; }
        input:active { width: 42px; }
        input:focus { width: 53px; }
        input:focus-visible { width: 64px; }
        input:checked { width: 75px; }
        input:disabled { width: 86px; }
        .group:has(> :checked) > button { color: red; }
    ",
    );
    let group = fixture.element(BODY, "div", &[("class", "group")]);
    let input = fixture.element(group, "input", &[("type", "checkbox"), ("checked", "")]);
    let sibling = fixture.element(group, "button", &[]);
    fixture.text(sibling, "Check");
    let pack = fixture.pack();
    let (sender, _receiver) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://forms.test/".parse().unwrap(), sender).unwrap();
    let input_frame = frame();
    let mut decoder = decoder(&pack);
    engine.update_layout(&input_frame).unwrap();
    let (before, _) = engine.snapshot();
    assert_eq!(box_fragment(before, input).border_box.size.width, 75.0);
    assert_eq!(
        box_fragment(before, sibling).style.art.color,
        PaintColor::srgb(1.0, 0.0, 0.0, 1.0)
    );
    let mask = [
        State::CHECKED,
        State::HOVER,
        State::ACTIVE,
        State::FOCUS,
        State::FOCUS_VISIBLE,
        State::DISABLED,
    ]
    .into_iter()
    .fold(0, |mask, bit| mask | bit.bits());
    for (state, selector, width) in [
        (State::default(), ":checked", 20.0),
        (State::HOVER, ":hover", 31.0),
        (State::ACTIVE, ":active", 42.0),
        (State::FOCUS, ":focus", 53.0),
        (State::FOCUS_VISIBLE, ":focus-visible", 64.0),
        (State::CHECKED, ":checked", 75.0),
        (State::DISABLED, ":disabled", 86.0),
    ] {
        let mut update = BatchWriter::new();
        update
            .record(Opcode::SetState, 0, input, state.bits(), mask)
            .unwrap();
        apply(&mut engine, &mut decoder, update);
        assert_eq!(
            engine
                .query(
                    &input_frame,
                    Query::Selector {
                        scope: NodeId::from_raw(input),
                        selector,
                        mode: SelectMode::Match,
                    }
                )
                .unwrap(),
            QueryValue::Bool(state.bits() != 0),
            "{selector}"
        );
        engine.update_layout(&input_frame).unwrap();
        let (after, _) = engine.snapshot();
        assert_eq!(
            box_fragment(after, input).border_box.size.width,
            width,
            "{selector}"
        );
        let expected_color = if state == State::CHECKED {
            PaintColor::srgb(1.0, 0.0, 0.0, 1.0)
        } else {
            PaintColor::BLACK
        };
        assert_eq!(
            box_fragment(after, sibling).style.art.color,
            expected_color,
            "{selector}"
        );
    }
}

#[test]
fn reapplies_disabled_state_to_fieldset_controls_outside_the_first_legend() {
    let mut fixture = Fixture::new(
        "
        input { box-sizing: border-box; width: 50px; }
        input:disabled { width: 90px; }
    ",
    );
    let fieldset = fixture.element(BODY, "fieldset", &[]);
    let first_legend = fixture.element(fieldset, "legend", &[]);
    let exempt = fixture.element(first_legend, "input", &[]);
    let second_legend = fixture.element(fieldset, "legend", &[]);
    let second = fixture.element(second_legend, "input", &[]);
    let control = fixture.element(fieldset, "input", &[]);
    let pack = fixture.pack();
    let (send, _receiver) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://forms.test/".parse().unwrap(), send).unwrap();
    let mut decoder = decoder(&pack);
    let frame = frame();
    engine.update_layout(&frame).unwrap();
    let width = |engine: &StaticDocument<'_>, id| {
        box_fragment(engine.snapshot().0, id).border_box.size.width
    };
    assert_eq!(width(&engine, control), 50.0);
    let mut update = BatchWriter::new();
    update
        .record(
            Opcode::SetState,
            0,
            fieldset,
            State::DISABLED.bits(),
            State::DISABLED.bits(),
        )
        .unwrap();
    apply(&mut engine, &mut decoder, update);
    engine.update_layout(&frame).unwrap();
    assert_eq!(
        [
            width(&engine, exempt),
            width(&engine, second),
            width(&engine, control)
        ],
        [50.0, 90.0, 90.0]
    );

    let mut update = BatchWriter::new();
    update
        .record(Opcode::Remove, 0, fieldset, second_legend, 0)
        .unwrap();
    update
        .record(Opcode::Insert, 0, fieldset, second_legend, first_legend)
        .unwrap();
    apply(&mut engine, &mut decoder, update);
    engine.update_layout(&frame).unwrap();
    assert_eq!(
        [
            width(&engine, exempt),
            width(&engine, second),
            width(&engine, control)
        ],
        [90.0, 50.0, 90.0]
    );
}

#[test]
fn native_select_uses_normal_line_height_with_layered_author_styles() {
    let mut fixture = Fixture::new(
        "
        @layer theme, utilities;
        @property --field-padding { syntax: '<length>'; inherits: false; initial-value: 6px }
        @layer theme {
            select { font: 14px/40px Arial; border: 1px solid oklch(.8 .01 255);
                     padding: var(--field-padding) 12px;
                     background: color-mix(in oklch, white 80%, black); }
        }
        @layer utilities { .plain { appearance: none } }
        ",
    );
    let menu = fixture.element(BODY, "select", &[]);
    let rtl = fixture.element(BODY, "select", &[("dir", "rtl")]);
    let plain = fixture.element(BODY, "select", &[("class", "plain")]);
    let list = fixture.element(BODY, "select", &[("size", "2")]);
    for select in [menu, rtl, plain, list] {
        let option = fixture.element(select, "option", &[]);
        fixture.text(option, "Engineer");
    }
    let pack = fixture.pack();
    let frame = frame();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    for (select, expected) in [(menu, "normal"), (plain, "40px"), (list, "40px")] {
        assert_eq!(
            engine
                .query(
                    &frame,
                    Query::ComputedStyle {
                        node: NodeId::from_raw(select),
                        pseudo: None,
                        property: "line-height",
                    },
                )
                .unwrap(),
            QueryValue::String(expected.to_owned())
        );
    }
    let (tree, _) = engine.snapshot();
    assert_eq!(box_fragment(tree, menu).border_box.size.height, 32.0);
    assert_eq!(box_fragment(tree, plain).border_box.size.height, 54.0);
    assert_ne!(
        box_fragment(tree, menu).style.art.background_color,
        PaintColor::srgb(1.0, 1.0, 1.0, 1.0)
    );
    for (select, right) in [(menu, true), (rtl, false)] {
        let fragment = box_fragment(tree, select);
        let FragmentContent::Svg(geometry) = &fragment.content else {
            panic!("expected native select arrow");
        };
        let PathCommand::Line(tip) = geometry.paths[0].commands[1] else {
            panic!("expected arrow tip");
        };
        let x = fragment.border.left + fragment.padding.left + tip.x;
        let expected = if right {
            fragment.border_box.size.width - 9.0
        } else {
            9.0
        };
        assert_eq!(x, expected);
    }
}

#[test]
fn grid_normal_alignment_stretches_controls_and_preserves_explicit_alignment() {
    let mut fixture = Fixture::new(
        "
        form { display: grid; grid-template-columns: 220px; align-items: start }
        #start { justify-self: start }
        #wide { width: 70px }
        ",
    );
    let form = fixture.element(BODY, "form", &[]);
    let input = fixture.element(form, "input", &[]);
    let area = fixture.element(form, "textarea", &[]);
    let menu = fixture.element(form, "select", &[]);
    let option = fixture.element(menu, "option", &[]);
    fixture.text(option, "One");
    let check = fixture.element(form, "input", &[("type", "checkbox")]);
    let start = fixture.element(form, "input", &[("id", "start")]);
    let wide = fixture.element(form, "input", &[("id", "wide")]);
    let image = fixture.element(
        form,
        "img",
        &[("src", "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='40' height='20'/%3E")],
    );

    let tree = layout(&fixture.pack());
    let width = |owner| box_fragment(&tree, owner).border_box.size.width;
    for owner in [input, area, menu] {
        assert_eq!(width(owner), 220.0);
    }
    assert_eq!(width(check), 213.0);
    assert!(width(start) < 220.0);
    assert_eq!(width(wide), 78.0);
    assert_eq!(width(image), 40.0);
}

#[test]
fn editor_scroll_range_includes_end_padding_and_clamps_caret_reveal() {
    let mut fixture = Fixture::new(
        "
        #preview { box-sizing: border-box; width: 220px; height: 100px;
                   padding: 16px; overflow: auto }
        #child { height: 150px; margin-bottom: 16px }
        textarea { box-sizing: border-box; width: 216.5px; height: 100px;
                   padding: 16px; border: 1px solid; font: 16px/20px 'Liberation Mono', monospace }
        ",
    );
    let preview = fixture.element(BODY, "div", &[("id", "preview")]);
    fixture.element(preview, "div", &[("id", "child")]);
    let editor = fixture.element(BODY, "textarea", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut journal = decoder(&pack);
    let value = ["1234567890123456789"; 20].join("\n");
    let end = value.len() as u32;
    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::SetValue, 0, editor, value.as_str(), 0)
        .unwrap();
    writer
        .record(Opcode::SetSelection, 0, editor, end, end)
        .unwrap();
    writer
        .record(Opcode::Focus, 0, editor, FOCUS_PREVENT_SCROLL, 0)
        .unwrap();
    apply(&mut engine, &mut journal, writer);

    let mut metrics = |owner| match engine
        .query(&frame(), Query::BoxMetrics(NodeId::from_raw(owner)))
        .unwrap()
    {
        QueryValue::BoxMetrics(metrics) => metrics,
        value => panic!("unexpected metrics result: {value:?}"),
    };
    let preview_metrics = metrics(preview);
    assert_eq!(
        (preview_metrics.client_height, preview_metrics.scroll_height),
        (100, 198)
    );
    let editor_metrics = metrics(editor);
    assert_eq!(
        (editor_metrics.client_height, editor_metrics.scroll_height),
        (98, 432)
    );
    assert_eq!(editor_metrics.scroll_width, editor_metrics.client_width);
    let scroll = box_fragment(engine.snapshot().0, editor).scroll.unwrap();
    assert_eq!(scroll.offset.x, 0.0);
    assert!(scroll.offset.y > 0.0 && scroll.offset.y <= 334.0);
    assert_eq!(scroll.content.max_y() - scroll.scrollport.max_y(), 334.0);
}

#[test]
fn hanging_textarea_spaces_do_not_expand_horizontal_overflow() {
    let mut fixture = Fixture::new(
        "textarea { width: 40px; height: 44px; padding: 0; border: 0; overflow: hidden;
                    font: 20px/20px 'Liberation Mono', monospace }",
    );
    let ltr = fixture.element(BODY, "textarea", &[("dir", "ltr")]);
    let rtl = fixture.element(BODY, "textarea", &[("dir", "rtl")]);
    let trailing = fixture.element(BODY, "textarea", &[]);
    let nowrap = fixture.element(BODY, "textarea", &[("wrap", "off")]);
    for (owner, text) in [
        (ltr, "A B C"),
        (rtl, "A B C"),
        (trailing, "AB    "),
        (nowrap, "AB    "),
    ] {
        fixture.text(owner, text);
    }
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let mut scroll_width = |owner| {
        let QueryValue::BoxMetrics(metrics) = engine
            .query(&frame(), Query::BoxMetrics(NodeId::from_raw(owner)))
            .unwrap()
        else {
            panic!("expected textarea box metrics");
        };
        assert_eq!(metrics.client_width, 40);
        metrics.scroll_width
    };
    for owner in [ltr, rtl, trailing] {
        assert_eq!(scroll_width(owner), 40);
    }
    assert!(scroll_width(nowrap) > 40);
}

#[test]
fn shows_the_textarea_resize_grip_with_scrollable_overflow() {
    let mut fixture = Fixture::new(
        "
        textarea { width: 90px; height: 40px }
        .off { overflow: visible }
        .clip { overflow: clip }
        .plain { appearance: none }
        ",
    );
    let default = fixture.element(BODY, "textarea", &[]);
    let off = fixture.element(BODY, "textarea", &[("class", "off")]);
    let clipped = fixture.element(BODY, "textarea", &[("class", "clip")]);
    let plain = fixture.element(BODY, "textarea", &[("class", "plain")]);
    let tree = layout(&fixture.pack());
    for owner in [default, plain] {
        assert!(matches!(
            &box_fragment(&tree, owner).content,
            FragmentContent::Svg(geometry) if geometry.paths.len() == 2
        ));
    }
    for owner in [off, clipped] {
        assert!(matches!(
            box_fragment(&tree, owner).content,
            FragmentContent::None
        ));
    }
}

#[test]
fn publishes_viewport_ime_caret_and_preedit_bounds_with_scroll_and_focus() {
    use squark_types::journal::FOCUS_BLUR;

    let mut fixture = Fixture::new(
        "body { margin: 0 } div { height: 30px; overflow: auto; transform: translateX(10px) }
         textarea { display: block; margin-top: 20px; width: 160px; height: 60px;
                    font: 16px/20px monospace }",
    );
    let scroller = fixture.element(BODY, "div", &[]);
    let editor = fixture.element(scroller, "textarea", &[]);
    let button = fixture.element(BODY, "button", &[]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/".parse().unwrap(), send).unwrap();
    let mut journal = decoder(&pack);

    engine.update_layout(&frame()).unwrap();
    assert_eq!(engine.snapshot().0.ime_area, None);

    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::SetValue, 0, editor, "abc", 0)
        .unwrap();
    writer
        .record(Opcode::Focus, 0, editor, FOCUS_PREVENT_SCROLL, 0)
        .unwrap();
    apply(&mut engine, &mut journal, writer);
    let initial = caret(&mut engine, editor, 3);
    assert_eq!(engine.snapshot().0.ime_area, Some(initial));
    assert!(initial.origin.x > 10.0);

    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::ScrollTo, 0, scroller, 0, 18f32.to_bits())
        .unwrap();
    apply(&mut engine, &mut journal, writer);
    engine.update_layout(&frame()).unwrap();
    let scrolled = engine.snapshot().0.ime_area.unwrap();
    assert_eq!(scrolled.origin.y, initial.origin.y - 18.0);

    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::SetPreedit, 0, editor, "漢字", 1)
        .unwrap();
    apply(&mut engine, &mut journal, writer);
    engine.update_layout(&frame()).unwrap();
    let composing = engine.snapshot().0.ime_area.unwrap();
    assert!((composing.origin.x - scrolled.origin.x).abs() < 1.0);
    assert_eq!(composing.origin.y, scrolled.origin.y);
    assert!(composing.size.width > 1.0);

    let mut writer = BatchWriter::new();
    writer
        .record(Opcode::Focus, 0, editor, FOCUS_BLUR, 0)
        .unwrap();
    writer
        .record(Opcode::Focus, 0, button, FOCUS_PREVENT_SCROLL, 0)
        .unwrap();
    apply(&mut engine, &mut journal, writer);
    engine.update_layout(&frame()).unwrap();
    assert_eq!(engine.snapshot().0.ime_area, None);
}

#[test]
fn retains_unrelated_fragments_across_textarea_value_and_selection_edits() {
    let mut fixture = Fixture::new(
        "body { display: grid; grid-template-columns: 200px 200px }
         textarea { width: 90%; padding: 5%; height: 60px; font: 16px/20px monospace }",
    );
    let field = fixture.element(BODY, "textarea", &[]);
    let aside = fixture.element(BODY, "aside", &[]);
    fixture.text(aside, "Retained preview.");
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/".parse().unwrap(), send).unwrap();
    let mut journal = decoder(&pack);
    engine.update_layout(&frame()).unwrap();
    let preview = box_fragment(engine.snapshot().0, aside);
    let preview_geometry = preview.border_box;
    let preview_style = preview.style.clone();
    let before = box_fragment(engine.snapshot().0, field).border_box;

    let value = "first\nsecond\nthird\nfourth";
    let mut writer = BatchWriter::new();
    writer.record(Opcode::SetValue, 0, field, value, 0).unwrap();
    apply(&mut engine, &mut journal, writer);
    engine.update_layout(&frame()).unwrap();
    let preview = box_fragment(engine.snapshot().0, aside);
    let editor = box_fragment(engine.snapshot().0, field);
    assert!(std::sync::Arc::ptr_eq(&preview.style, &preview_style));
    assert_eq!(preview.border_box, preview_geometry);
    assert_eq!(editor.border_box, before);
    assert!(editor.scroll.unwrap().content.size.height > before.size.height);

    // A forced full rebuild must produce the same caret and field geometry.
    let after = caret(&mut engine, field, value.len() as u32);
    let mut changed = frame();
    changed.tick.viewport.epoch += 1;
    engine.update_layout(&changed).unwrap();
    assert_eq!(caret(&mut engine, field, value.len() as u32), after);
    assert_eq!(box_fragment(engine.snapshot().0, field).border_box, before);
}
