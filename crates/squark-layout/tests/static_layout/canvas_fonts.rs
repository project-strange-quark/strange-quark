// SPDX-License-Identifier: MIT
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::json;
use squark_layout::StaticDocument;
use squark_types::{
    NodeId,
    engine::{DecodedBatch, Direction, DocumentEngine, FaceId, FrameInput, MeasureText},
    fragment::Filter,
    journal::{FontFaceAction, Opcode, Record},
    pack::MANIFEST_PATH,
    render::{FrameTick, ResourceUpdate, Viewport, resource_channel},
};

use super::{BODY, Fixture, VIEWPORT};

const WOFF2: &[u8] = include_bytes!(
    "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
);
const TTF: &[u8] =
    include_bytes!("../../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.ttf");
const CANVAS_TEST: &[u8] =
    include_bytes!("../../../../tests/wpt/canvas/vendor/fonts/CanvasTest.ttf");
const R3_SANS: &[u8] = include_bytes!(
    "../../../../tests/reftests/cases/r3-canvas/_assets/fonts/LiberationSans-Regular.woff2"
);

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

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn rect_text(font: &str, text: &str, align: u32) -> Vec<u8> {
    let mut fields = Vec::new();
    for v in [100.0, 50.0, f64::NAN] {
        fields.extend(v.to_le_bytes());
    }
    let font = units(font);
    let text = units(text);
    for word in [align, 3, 0, font.len() as u32, text.len() as u32] {
        fields.extend(word.to_le_bytes());
    }
    fields.extend(font.into_iter().chain(text).flat_map(u16::to_le_bytes));
    fields
}

fn command(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut command = vec![op, 0, 0, 0];
    command.extend((payload.len() as u32).to_le_bytes());
    command.extend(payload);
    command.resize(command.len().next_multiple_of(8), 0);
    command
}

fn canvas_stream(commands: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"JCV1".to_vec();
    let len = 12 + commands.iter().map(Vec::len).sum::<usize>();
    bytes.extend((len as u32).to_le_bytes());
    bytes.extend((commands.len() as u32).to_le_bytes());
    for command in commands {
        bytes.extend(command);
    }
    bytes
}

#[test]
fn canvas_uploads_decoded_element_images_before_their_first_draw() {
    const PNG: &[u8] = include_bytes!("../../../../tests/wpt/canvas/vendor/images/green.png");
    let mut fixture = Fixture::new("");
    let canvas = fixture.element(BODY, "canvas", &[]);
    let other_canvas = fixture.element(BODY, "canvas", &[]);
    let image = fixture.element(BODY, "img", &[("src", "/green.png")]);
    fixture
        .journal
        .record(Opcode::Remove, 0, BODY, image, 0)
        .unwrap();
    let empty = fixture.element(BODY, "img", &[]);
    let failed = fixture.element(BODY, "img", &[("src", "/missing.png")]);
    let pack = fixture.pack_with_assets(&[("/green.png", 3, PNG)]);
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();

    let draw = |kind: u32, id: u32| {
        let mut payload = kind.to_le_bytes().to_vec();
        payload.extend(id.to_le_bytes());
        for coordinate in [1.0_f64, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
            payload.extend(coordinate.to_le_bytes());
        }
        command(23, &payload)
    };
    let draws = canvas_stream(&[
        command(1, &[]),
        draw(0, empty),
        draw(0, image),
        draw(1, other_canvas),
        draw(0, failed),
        draw(0, image),
        command(2, &[]),
    ]);
    let submit = [Record {
        op: Opcode::CanvasSubmit,
        flags: 0,
        a: canvas,
        b: 1,
        c: draws.len() as u32,
    }];
    let batch = DecodedBatch {
        records: &submit,
        strings: &[],
        staged_bytes: &[(1, &draws)],
    };
    let mut messages = updates.try_iter();
    engine.flush(batch).unwrap();
    let ResourceUpdate::PutImage { key, bitmap } = messages.next().unwrap().update else {
        panic!("expected the source image before the canvas submit");
    };
    assert_eq!(&bitmap.pixels[..4], &[0, 255, 0, 255]);
    let wire = u32::try_from(key.0).unwrap();
    assert_ne!(wire, image);
    let expected = canvas_stream(&[
        command(1, &[]),
        draw(0, wire),
        draw(1, other_canvas),
        draw(0, wire),
        command(2, &[]),
    ]);
    for repeat in [false, true] {
        if repeat {
            engine.flush(batch).unwrap();
        }
        let ResourceUpdate::SubmitCanvas { id, stream } = messages.next().unwrap().update else {
            panic!("expected a draw using the existing image resource");
        };
        assert_eq!(id.raw(), canvas);
        assert_eq!(&*stream, expected);
        assert!(messages.next().is_none());
    }

    engine
        .flush(DecodedBatch {
            records: &[Record {
                op: Opcode::Insert,
                flags: 0,
                a: BODY,
                b: image,
                c: 0,
            }],
            strings: &[],
            staged_bytes: &[],
        })
        .unwrap();
    engine.update_layout(&input()).unwrap();
    assert!(matches!(
        super::find(engine.snapshot().0, image).content,
        squark_types::fragment::FragmentContent::Image { key: painted, .. } if painted == key
    ));
    assert!(messages.next().is_none());
}

#[test]
fn canvas_rewrites_text_and_preserves_resize_and_other_commands() {
    let mut f = Fixture::new(
        "@font-face { font-family: CanvasTest; src: url(/CanvasTest.ttf) }
         canvas { color: rgb(7, 128, 220) }",
    );
    let id = NodeId::from_raw(f.element(BODY, "canvas", &[]));
    let pack = f.pack_with_assets(&[("/CanvasTest.ttf", 5, CANVAS_TEST)]);
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    assert_eq!(
        engine.parse_color(id, &units("currentColor")),
        Some([7, 128, 220, 255])
    );
    assert_eq!(
        engine.parse_color(id, &units("color-mix(in srgb, red, blue)")),
        Some([128, 0, 128, 255])
    );
    assert!(engine.parse_color(id, &units("rgb(nope)")).is_none());
    assert!(engine.validate_filter(&units("invert(93%) hue-rotate(180deg)")));
    assert_eq!(
        engine
            .canvas_filters(Some(id), &units("invert(93%) hue-rotate(180deg)"))
            .as_deref(),
        Some(
            [
                Filter::Invert(0.93),
                Filter::HueRotate(std::f32::consts::PI)
            ]
            .as_slice()
        )
    );
    assert!(!engine.validate_filter(&units("blur(nonsense)")));
    assert_eq!(
        String::from_utf16(
            &engine
                .parse_canvas_font(id, &units("bold 20px/30px monospace"))
                .unwrap()
        )
        .unwrap(),
        "bold 20px monospace"
    );

    let font = "50px CanvasTest";
    let text = "A B";
    let width = engine
        .measure_text(MeasureText {
            font_css: font,
            text: &units(text),
            direction: Direction::Ltr,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            font_environment: Some(id),
        })
        .unwrap()
        .width;
    assert_eq!(width, 150.0);
    let stream = canvas_stream(&[
        command(1, &[]),
        command(26, &rect_text(font, text, 4)),
        command(2, &[]),
    ]);
    let records = [
        Record {
            op: Opcode::CanvasSize,
            flags: 0,
            a: id.raw(),
            b: 300,
            c: 150,
        },
        Record {
            op: Opcode::CanvasSubmit,
            flags: 0,
            a: id.raw(),
            b: 1,
            c: stream.len() as u32,
        },
    ];
    engine
        .flush(DecodedBatch {
            records: &records,
            strings: &[],
            staged_bytes: &[(1, &stream)],
        })
        .unwrap();
    let mut messages = updates.try_iter();
    assert!(
        matches!(messages.next().unwrap().update, ResourceUpdate::ResizeCanvas { id: canvas, size: [300,150] } if canvas==id)
    );
    let ResourceUpdate::PutFont {
        id: font_id,
        bytes,
        index,
        size_bits,
        ..
    } = messages.next().unwrap().update
    else {
        panic!("expected the font before the first canvas submit");
    };
    assert_eq!(&*bytes, CANVAS_TEST);
    assert_eq!(index, 0);
    assert_eq!(f32::from_bits(size_bits), 50.0);
    let ResourceUpdate::SubmitCanvas { id: canvas, stream } = messages.next().unwrap().update
    else {
        panic!("expected rewritten canvas stream");
    };
    assert_eq!(canvas, id);
    assert_eq!(&stream[..4], b"JCV1");
    let word = |at| u32::from_le_bytes(stream[at..at + 4].try_into().unwrap());
    assert_eq!(word(4) as usize, stream.len());
    assert_eq!(word(8), 3);
    assert_eq!(stream[12], 1);
    assert_eq!(stream[20], 24);
    assert_eq!(word(28), 0);
    assert_eq!(word(32), 1);
    assert_eq!(
        u64::from_le_bytes(stream[36..44].try_into().unwrap()),
        font_id.0
    );
    assert_eq!(word(44), 3);
    let first_x = f64::from_le_bytes(stream[52..60].try_into().unwrap());
    let first_y = f64::from_le_bytes(stream[60..68].try_into().unwrap());
    assert!((first_x - (100.0 - width / 2.0)).abs() < 0.01);
    assert!((first_y - 50.0).abs() < 0.01);
    assert_eq!(stream[stream.len() - 8], 2);

    let repeated = canvas_stream(&[command(26, &rect_text(font, text, 0))]);
    let different_size = canvas_stream(&[command(26, &rect_text("25px CanvasTest", text, 0))]);
    for (stream, new_size) in [(&repeated, false), (&different_size, true)] {
        engine
            .flush(DecodedBatch {
                records: &[Record {
                    c: stream.len() as u32,
                    ..records[1]
                }],
                strings: &[],
                staged_bytes: &[(1, stream)],
            })
            .unwrap();
        if new_size {
            let ResourceUpdate::PutFont {
                id,
                bytes: shared,
                size_bits,
                ..
            } = messages.next().unwrap().update
            else {
                panic!("expected a resource for the second font size");
            };
            assert_ne!(id, font_id);
            assert!(Arc::ptr_eq(&shared, &bytes));
            assert_eq!(f32::from_bits(size_bits), 25.0);
        }
        assert!(matches!(
            messages.next().unwrap().update,
            ResourceUpdate::SubmitCanvas { .. }
        ));
    }
    assert!(messages.next().is_none());
}

#[test]
fn parses_canvas_fonts_through_the_document_engine() {
    let mut fixture = Fixture::new("html { font-size: 12px } canvas { font-size: 24px }");
    let canvas = NodeId::from_raw(fixture.element(BODY, "canvas", &[]));
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    let engine: &dyn DocumentEngine = &engine;
    for (value, expected) in [
        ("20PX   SERIF", "20px serif"),
        ("  15px sans-serif  ", "15px sans-serif"),
        ("1.5em serif", "36px serif"),
        ("125% serif", "30px serif"),
        ("1.5rem serif", "18px serif"),
        ("bold 20px/30px monospace", "bold 20px monospace"),
        (
            "italic 20px Excalifont, Comic Shanns, sans-serif",
            "italic 20px Excalifont, \"Comic Shanns\", sans-serif",
        ),
    ] {
        assert_eq!(
            engine.parse_canvas_font(canvas, &units(value)),
            Some(units(expected)),
            "{value:?}"
        );
    }
    for invalid in ["nonsense", "inherit", "20px"] {
        assert_eq!(engine.parse_canvas_font(canvas, &units(invalid)), None);
    }
}

#[test]
fn canvas_uses_hinted_ink_bounds_and_typographic_baselines() {
    let mut fixture = Fixture::new("@font-face {font-family: TestSans; src: url(/sans.woff2)}");
    let canvas = NodeId::from_raw(fixture.element(BODY, "canvas", &[]));
    let pack = fixture.pack_with_assets(&[("/sans.woff2", 5, R3_SANS)]);
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    // Bounds captured with pinned Chromium 159. The last glyph's
    // fractional position contributes to the right edge after rounding.
    for (font, text, expected) in [
        ("16px TestSans", "A", [0.0, 11.0, 12.0, 0.0]),
        (
            "24px TestSans",
            "Excalidraw",
            [-1.0, 116.70703125, 17.0, 0.0],
        ),
        (
            "24px TestSans",
            "Wrapping Wg",
            [0.0, 143.6015625, 17.0, 5.0],
        ),
        ("24px TestSans", "j", [1.0, 4.0, 17.0, 5.0]),
        ("50px TestSans", "A", [0.0, 34.0, 35.0, 0.0]),
        ("50px TestSans", "A B", [0.0, 78.2412109375, 35.0, 0.0]),
    ] {
        let m = engine
            .measure_text(MeasureText {
                font_css: font,
                text: &units(text),
                direction: Direction::Ltr,
                letter_spacing: 0.0,
                word_spacing: 0.0,
                font_environment: Some(canvas),
            })
            .unwrap();
        assert_eq!(
            [
                m.actual_left,
                m.actual_right,
                m.actual_ascent,
                m.actual_descent
            ],
            expected,
            "{font} {text:?}"
        );
    }

    let commands: Vec<_> = (0_u32..6)
        .map(|baseline| {
            let mut text = rect_text("20px TestSans", "Wg1", 0);
            text[28..32].copy_from_slice(&baseline.to_le_bytes());
            command(26, &text)
        })
        .collect();
    let stream = canvas_stream(&commands);
    engine
        .flush(DecodedBatch {
            records: &[Record {
                op: Opcode::CanvasSubmit,
                flags: 0,
                a: canvas.raw(),
                b: 1,
                c: stream.len() as u32,
            }],
            strings: &[],
            staged_bytes: &[(1, &stream)],
        })
        .unwrap();
    let stream = updates
        .try_iter()
        .find_map(|m| match m.update {
            ResourceUpdate::SubmitCanvas { stream, .. } => Some(stream),
            _ => None,
        })
        .unwrap();
    let mut commands = &stream[12..];
    for offset in [15.515625, 14.4, 5.515625, 0.0, -4.0, -4.484375] {
        assert_eq!(commands[0], 24);
        let len = u32::from_le_bytes(commands[4..8].try_into().unwrap()) as usize;
        let y = f64::from_le_bytes(commands[40..48].try_into().unwrap());
        assert!((y - 50.0 - offset).abs() < 0.00001, "{y} vs {offset}");
        commands = &commands[8 + len.next_multiple_of(8)..];
    }
    assert!(commands.is_empty());
}

fn define(
    engine: &mut StaticDocument<'_>,
    id: u32,
    family: &str,
    json: serde_json::Value,
    buffer: Option<&[u8]>,
) {
    let descriptor = units(&json.to_string());
    let family = units(family);
    let records = [
        Record {
            op: Opcode::FontFace,
            flags: FontFaceAction::Define as u8,
            a: id,
            b: 0,
            c: 1,
        },
        Record {
            op: Opcode::FontFace,
            flags: FontFaceAction::Add as u8,
            a: id,
            b: 0,
            c: 1,
        },
    ];
    let staged: Vec<_> = buffer.map(|buffer| (1, buffer)).into_iter().collect();
    engine
        .flush(DecodedBatch {
            records: &records,
            strings: &[&family, &descriptor],
            staged_bytes: &staged,
        })
        .unwrap();
}

fn await_font(engine: &mut StaticDocument<'_>, id: u32) {
    engine.font_load(FaceId(id)).unwrap();
    engine.font_load(FaceId(id)).unwrap();
    let stop = Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine.take_font_completions();
        if let Some(completion) = events.into_iter().next() {
            assert_eq!(completion.face, FaceId(id));
            assert_eq!(completion.result, Ok(()));
            break;
        }
        assert!(Instant::now() < stop, "font completion timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(engine.take_font_completions().is_empty());
}

#[test]
fn canvas_measurement_preserves_every_space() {
    let mut fixture = Fixture::new("");
    let canvas = NodeId::from_raw(fixture.element(BODY, "canvas", &[]));
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    define(
        &mut engine,
        1,
        "CanvasTest",
        json!({"source":{"bufferId":1}}),
        Some(CANVAS_TEST),
    );
    await_font(&mut engine, 1);
    engine.update_layout(&input()).unwrap();
    let mut width = |text| {
        engine
            .measure_text(MeasureText {
                font_css: "50px CanvasTest",
                text: &units(text),
                direction: Direction::Ltr,
                letter_spacing: 0.0,
                word_spacing: 0.0,
                font_environment: Some(canvas),
            })
            .unwrap()
            .width
    };
    // CanvasTest gives each covered character a 50px advance in Chromium 159.
    for (text, expected) in [
        ("", 0.0),
        (" ", 50.0),
        ("A B", 150.0),
        ("A  B", 200.0),
        (" AB", 150.0),
        ("AB ", 150.0),
        ("AB\t", 150.0),
        ("AB\r", 150.0),
        ("\t", 50.0),
        ("A \t\n\x0c\r  \t\n\x0c\rB", 650.0),
        ("A \x0b B", 250.0),
    ] {
        assert_eq!(width(text), expected, "{text:?}");
    }
    let unspaced = width("ab");
    for (text, advance) in [
        ("a b", 50.0),
        ("a  b", 100.0),
        (" ab", 50.0),
        ("ab ", 50.0),
        (" a b ", 150.0),
    ] {
        assert!((width(text) - unspaced - advance).abs() < 0.001, "{text:?}");
    }
    assert_eq!(width("あ\t"), width("あ "));
}

#[test]
fn canvas_kerns_letters_within_words_and_keeps_space_advances() {
    const SANS: &[u8] = include_bytes!(
        "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
    );
    let mut fixture = Fixture::new(
        "@font-face {font-family: SpacingTest; src: url(/spacing.woff2)}
         #reference {display: inline-block; white-space: pre; font: 32px SpacingTest}",
    );
    let reference = fixture.element(BODY, "span", &[("id", "reference")]);
    fixture.text(reference, "A   B  ");
    let pack = fixture.pack_with_assets(&[("/spacing.woff2", 5, SANS)]);
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    let mut width = |text| {
        engine
            .measure_text(MeasureText {
                font_css: "32px SpacingTest",
                text: &units(text),
                direction: Direction::Ltr,
                letter_spacing: 0.0,
                word_spacing: 0.0,
                font_environment: None,
            })
            .unwrap()
            .width
    };
    // Chromium 159 keeps the AV pair kern and gives each canvas space its
    // full advance. Inline DOM text also kerns across the A-space pair.
    assert_eq!(width("AV"), 40.3125);
    assert_eq!(width("A B"), 51.578125);
    assert_eq!(width("A   B  "), 87.140625);
    let reference = super::find(engine.snapshot().0, reference);
    assert_eq!(reference.border_box.size.width, 85.375);
}

#[test]
fn starts_matching_scripted_faces_when_document_text_uses_them() {
    let mut fixture = Fixture::new(
        ".visible { font: 20px 'Shell Face', serif }
         .hidden { display: none; font: 20px 'Unused Face', serif }",
    );
    let visible = fixture.element(BODY, "div", &[("class", "visible")]);
    fixture.text(visible, "Visible");
    let hidden = fixture.element(BODY, "div", &[("class", "hidden")]);
    fixture.text(hidden, "Hidden");
    let pack = fixture.pack_with_assets(&[("/packed.woff2", 5, WOFF2)]);
    let (send, _updates) = resource_channel();
    let base = "https://example.test/";
    let mut engine = StaticDocument::from_pack(&pack, base.parse().unwrap(), send).unwrap();
    for (id, family, range) in [
        (1, "Shell Face", "U+0000-007F"),
        (2, "Shell Face", "U+4E00-9FFF"),
        (3, "Unused Face", "U+0000-007F"),
    ] {
        define(
            &mut engine,
            id,
            family,
            json!({"source":{"css":"url(/packed.woff2)","baseUrl":base},
                   "unicode-range":range}),
            None,
        );
    }
    engine.update_layout(&input()).unwrap();
    assert_eq!(engine.take_font_load_starts(), [FaceId(1)]);
    let stop = Instant::now() + Duration::from_secs(5);
    let loaded = loop {
        let events = engine.take_font_completions();
        if !events.is_empty() {
            break events;
        }
        assert!(Instant::now() < stop, "automatic font load timed out");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].face, FaceId(1));
    assert_eq!(loaded[0].result, Ok(()));
    engine.update_layout(&input()).unwrap();
    assert!(engine.take_font_load_starts().is_empty());
    assert!(engine.take_font_completions().is_empty());
}

#[test]
fn scripted_font_sets_the_editor_line_baselines() {
    use squark_types::fragment::FragmentContent;

    let mut fixture = Fixture::new(
        "textarea {font: 20px/20px 'Editor Face'; width: 62px; height: 64px;
                   padding: 0; border: 0; overflow: hidden}",
    );
    let editor = fixture.element(BODY, "textarea", &[]);
    fixture.text(editor, "AV CD EF\nAB");
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    define(
        &mut engine,
        1,
        "Editor Face",
        json!({"source":{"bufferId":1}, "unicode-range":"U+0000-007F"}),
        Some(WOFF2),
    );
    await_font(&mut engine, 1);
    engine.update_layout(&input()).unwrap();
    let tree = engine.snapshot().0;
    let origin = super::find(tree, editor).border_box.origin.y;
    let mut offsets: Vec<_> = tree
        .nodes
        .values()
        .filter(|fragment| fragment.owner.raw() == editor)
        .filter_map(|fragment| match &fragment.content {
            FragmentContent::Text(runs) => Some(runs.iter().map(|run| run.baseline - origin)),
            _ => None,
        })
        .flatten()
        .collect();
    offsets.sort_by(f32::total_cmp);
    offsets.dedup();
    // Chromium 159 places the first Liberation Mono baseline 15 px down
    // in a 20 px line box. Soft wraps and explicit breaks share that strut.
    assert_eq!(offsets, [15.0, 35.0, 55.0]);
}

#[test]
fn font_faces_load_packed_blob_and_binary_sources_and_match_ranges() {
    let pack = Fixture::new("").pack_with_assets(&[("/packed.woff2", 5, WOFF2)]);
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/app".parse().unwrap(), send)
            .unwrap()
            .with_blob_resolver(Arc::new(|url| {
                (url == "blob:https://example.test/face")
                    .then(|| (Arc::from(TTF), "font/ttf".into()))
            }));
    define(
        &mut engine,
        1,
        "Fixture Face",
        json!({
            "source":{"css":"url(/packed.woff2) format('woff2')","baseUrl":"https://example.test/app"},
            "font-style":"normal", "font-weight":"400", "unicode-range":"U+0041-0042",
        }),
        None,
    );
    define(
        &mut engine,
        2,
        "Fixture Face",
        json!({
            "source":{"bufferId":1}, "font-style":"italic", "font-weight":"700 900",
            "unicode-range":"U+0041-0042",
        }),
        Some(TTF),
    );
    define(
        &mut engine,
        3,
        "Fixture Face",
        json!({
            "source":{"css":"url(blob:https://example.test/face)","baseUrl":"https://example.test/app"},
            "font-style":"italic", "font-weight":"700 900", "unicode-range":"U+0058",
        }),
        None,
    );
    let font = units("italic 800 20px 'Fixture Face'");
    let matches = engine.match_font_faces(&font, &units("ABX0")).unwrap();
    assert_eq!(matches, [FaceId(2), FaceId(3)]);
    assert_eq!(
        engine
            .match_font_faces(&units("20px 'Fixture Face'"), &units("AB"))
            .unwrap(),
        [FaceId(1)]
    );
    assert!(
        engine
            .match_font_faces(&font, &units("0"))
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .match_font_faces(&units("bogus"), &units("A"))
            .is_err()
    );
    for id in [1, 2, 3] {
        await_font(&mut engine, id);
    }

    let mut measure = |font_css, text: &str| {
        engine
            .measure_text(MeasureText {
                font_css,
                text: &units(text),
                direction: Direction::Ltr,
                letter_spacing: 0.0,
                word_spacing: 0.0,
                font_environment: None,
            })
            .unwrap()
            .width
    };
    let family = "20px 'Fixture Face', serif";
    // The face is Liberation Mono, whose glyphs all advance 0.6 em, so "AB"
    // measures 24.00 at 20px. Compare with that, not with `monospace`: the
    // system picks `monospace`, and Ubuntu's fontconfig maps it to DejaVu Sans
    // Mono (24.08). Windows maps it to Consolas, as pinned Chromium
    // 159.0.8154.6 does there, and measures 21.99.
    assert!((measure(family, "AB") - 24.0).abs() < 0.01);
    #[cfg(target_os = "windows")]
    assert!((measure("20px monospace", "AB") - 21.99).abs() < 0.01);
    assert_eq!(measure(family, "Z"), measure("20px serif", "Z"));
}

#[test]
fn font_face_fetches_an_allowlisted_source() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let manifest = json!({"network":{"allow":[address.to_string()]}}).to_string();
    let pack = Fixture::new("").pack_with_assets(&[(MANIFEST_PATH, 4, manifest.as_bytes())]);
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    define(
        &mut engine,
        1,
        "Fetched Face",
        json!({
            "source":{"css":format!("url(http://{address}/face.ttf)"),"baseUrl":"https://example.test/"}
        }),
        None,
    );

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let stop = Instant::now()+Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream,_)) => break stream,
                    Err(error) if error.kind()==std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < stop, "no FontFace request arrived");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}")
                }
            };
            // On macOS/BSD, sockets accepted from a non-blocking listener inherit
            // O_NONBLOCK; switch back to blocking so read_line obeys the timeout
            // instead of failing immediately with WouldBlock (EAGAIN).
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "GET /face.ttf HTTP/1.1\r\n");
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap()==0 || line=="\r\n" { break; }
            }
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: font/ttf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", TTF.len()).unwrap();
            stream.write_all(TTF).unwrap();
        });
        await_font(&mut engine, 1);
        server.join().unwrap();
    });
}

#[test]
fn malformed_canvas_text_rejects_the_batch_before_resize_is_published() {
    let mut fixture = Fixture::new("");
    let canvas = fixture.element(BODY, "canvas", &[]);
    let pack = fixture.pack();
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    let stream = canvas_stream(&[command(26, &[0; 16])]);
    let ops = [
        Record {
            op: Opcode::CanvasSize,
            flags: 0,
            a: canvas,
            b: 40,
            c: 20,
        },
        Record {
            op: Opcode::CanvasSubmit,
            flags: 0,
            a: canvas,
            b: 1,
            c: stream.len() as u32,
        },
    ];
    assert!(
        engine
            .flush(DecodedBatch {
                records: &ops,
                strings: &[],
                staged_bytes: &[(1, &stream)],
            })
            .is_err()
    );
    assert!(updates.try_iter().next().is_none());
}

#[test]
fn sizes_canvas_as_replaced_content_and_projects_the_ancestor_filter() {
    use squark_types::fragment::{FragmentContent, FragmentKind};

    let mut f = Fixture::new(
        "body { margin: 0 } #dark { filter: invert(1) hue-rotate(180deg) }
         #scaled { width: 160px } canvas { vertical-align: top }",
    );
    let dark = f.element(BODY, "div", &[("id", "dark")]);
    let scaled = NodeId::from_raw(f.element(
        dark,
        "canvas",
        &[("id", "scaled"), ("width", " +600x"), ("height", "200")],
    ));
    f.text(scaled.raw(), "Fallback content");
    let natural = NodeId::from_raw(f.element(BODY, "canvas", &[("width", "-1")]));
    let pack = f.pack();
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    let locate = |engine: &StaticDocument<'_>, id, dims, expected: [f32; 2]| {
        let tree = engine.snapshot().0;
        let f = tree
            .nodes
            .values()
            .find(|f| f.owner == id && f.kind == FragmentKind::Replaced)
            .unwrap();
        assert!(
            matches!(f.content, FragmentContent::Canvas { id: owner, bitmap_size } if owner == id && bitmap_size == dims)
        );
        assert!(f.children.is_empty());
        assert!((f.border_box.size.width - expected[0]).abs() < 0.01);
        assert!((f.border_box.size.height - expected[1]).abs() < 0.01);
    };
    engine.update_layout(&input()).unwrap();
    locate(&engine, natural, [300, 150], [300.0, 150.0]);
    locate(&engine, scaled, [600, 200], [160.0, 160.0 / 3.0]);
    let container = engine
        .snapshot()
        .0
        .nodes
        .values()
        .find(|f| f.owner.raw() == dark && f.kind == FragmentKind::Box)
        .unwrap();
    assert!(
        matches!(&*container.style.properties.filters, [Filter::Invert(amount), Filter::HueRotate(_)] if *amount == 1.0)
    );

    engine
        .flush(DecodedBatch {
            records: &[Record {
                op: Opcode::CanvasSize,
                flags: 0,
                a: scaled.raw(),
                b: 200,
                c: 100,
            }],
            strings: &[],
            staged_bytes: &[],
        })
        .unwrap();
    engine.update_layout(&input()).unwrap();
    locate(&engine, scaled, [200, 100], [160.0, 80.0]);
    assert!(updates.try_iter().any(|update| matches!(
        update.update, ResourceUpdate::ResizeCanvas { id, size: [200, 100] } if id == scaled
    )));
}

#[test]
fn system_ui_shapes_with_the_fontconfig_system_ui_family() {
    use parley::{FontContext, fontique::GenericFamily};

    let mut system = FontContext::default();
    let Some(id) = system
        .collection
        .generic_families(GenericFamily::SystemUi)
        .next()
    else {
        return;
    };
    let family = system.collection.family_name(id).unwrap();
    let mut fixture = Fixture::new("");
    let context = NodeId::from_raw(fixture.element(BODY, "div", &[]));
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    let text = units("System UI Probe ABC MW iija11");
    let mut width = |css: &str| {
        engine
            .measure_text(MeasureText {
                font_environment: Some(context),
                font_css: css,
                text: &text,
                direction: Direction::Ltr,
                letter_spacing: 0.0,
                word_spacing: 0.0,
            })
            .unwrap()
            .width
    };
    assert_eq!(
        width("16px system-ui"),
        width(&format!("16px \"{family}\""))
    );
}

#[test]
fn rewrites_canvas_filter_operations_without_dropping_later_commands() {
    let filter = |text: &str| {
        let value = units(text);
        let mut payload = (value.len() as u32).to_le_bytes().to_vec();
        payload.extend(value.into_iter().flat_map(u16::to_le_bytes));
        command(27, &payload)
    };
    let input_stream = canvas_stream(&[
        command(1, &[]),
        filter("invert(93%) hue-rotate(180deg)"),
        filter("drop-shadow(5px -2px 3px currentColor)"),
        filter("invert(50%) url(#unknown)"),
        filter("none"),
        filter("malformed CSS"),
        command(27, &[1]),
        command(2, &[]),
    ]);

    let mut fixture = Fixture::new("canvas { color: rgba(7, 128, 220, 0.5) }");
    let canvas = fixture.element(BODY, "canvas", &[]);
    let pack = fixture.pack();
    let (send, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    assert!(engine.validate_filter(&units("url(#unknown)")));
    engine
        .flush(DecodedBatch {
            records: &[Record {
                op: Opcode::CanvasSubmit,
                flags: 0,
                a: canvas,
                b: 1,
                c: input_stream.len() as u32,
            }],
            strings: &[],
            staged_bytes: &[(1, &input_stream)],
        })
        .unwrap();
    let ResourceUpdate::SubmitCanvas { stream, .. } = updates.try_recv().unwrap().update else {
        panic!("expected filter stream");
    };
    let mut dark = 2u32.to_le_bytes().to_vec();
    dark.extend(6u32.to_le_bytes());
    dark.extend(0.93f32.to_le_bytes());
    dark.extend(5u32.to_le_bytes());
    dark.extend(180.0f32.to_le_bytes());
    let mut shadow = 1u32.to_le_bytes().to_vec();
    shadow.extend(3u32.to_le_bytes());
    for value in [5.0f32, -2.0, 3.0] {
        shadow.extend(value.to_le_bytes());
    }
    shadow.extend([7, 128, 220, 128]);

    assert_eq!(
        stream.as_ref(),
        canvas_stream(&[
            command(1, &[]),
            command(28, &dark),
            command(28, &shadow),
            command(28, &[0; 4]),
            command(28, &[0; 4]),
            command(2, &[]),
        ])
    );
}

#[test]
fn canvas_text_and_measure_text_start_unloaded_document_fonts() {
    let mut fixture = Fixture::new("");
    let canvas = fixture.element(BODY, "canvas", &[]);
    let pack = fixture.pack_with_assets(&[("/packed.woff2", 5, WOFF2)]);
    let (send, _updates) = resource_channel();
    let base = "https://example.test/";
    let mut engine = StaticDocument::from_pack(&pack, base.parse().unwrap(), send).unwrap();
    for (id, family, range) in [
        (1, "Canvas Draw Face", "U+0000-007F"),
        (2, "Canvas Measure Face", "U+0000-007F"),
        (3, "Unused Canvas Face", "U+0000-007F"),
    ] {
        define(
            &mut engine,
            id,
            family,
            json!({"source":{"css":"url(/packed.woff2)","baseUrl":base},
                   "unicode-range":range}),
            None,
        );
    }

    let font_u16 = units("20px 'Canvas Draw Face', serif");
    let text_u16 = units("Hello");
    let mut payload = Vec::new();
    payload.extend(10.0f64.to_le_bytes());
    payload.extend(20.0f64.to_le_bytes());
    payload.extend(f64::NAN.to_le_bytes());
    payload.extend(0u32.to_le_bytes()); // Start
    payload.extend(3u32.to_le_bytes()); // Alphabetic
    payload.extend(2u32.to_le_bytes()); // inherit
    payload.extend((font_u16.len() as u32).to_le_bytes());
    payload.extend((text_u16.len() as u32).to_le_bytes());
    payload.extend(font_u16.into_iter().flat_map(u16::to_le_bytes));
    payload.extend(text_u16.into_iter().flat_map(u16::to_le_bytes));
    let stream = canvas_stream(&[command(26, &payload)]);
    engine
        .flush(DecodedBatch {
            records: &[Record {
                op: Opcode::CanvasSubmit,
                flags: 0,
                a: canvas,
                b: 1,
                c: stream.len() as u32,
            }],
            strings: &[],
            staged_bytes: &[(1, &stream)],
        })
        .unwrap();
    assert_eq!(engine.take_font_load_starts(), [FaceId(1)]);

    let measured = units("Measure");
    engine
        .measure_text(MeasureText {
            font_environment: Some(NodeId::from_raw(canvas)),
            font_css: "18px 'Canvas Measure Face', sans-serif",
            text: &measured,
            direction: Direction::Ltr,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        })
        .unwrap();
    assert_eq!(engine.take_font_load_starts(), [FaceId(2)]);
    assert!(engine.take_font_load_starts().is_empty());
}

#[test]
fn resolves_font_relative_units_against_the_primary_web_font() {
    let sans: &[u8] = include_bytes!(
        "../../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
    );
    let mut fixture = Fixture::new(
        "@font-face { font-family: Sans; src: url(/sans.woff2) }
         html { font: 16px Sans, serif } body { margin: 0; font-size: 13px }
         div { height: 1px }
         #ch { width: 34ch } #ex { width: 10ex } #cap { width: 10cap }
         #ic { width: 10ic } #rch { width: 10rch }
         #late { font-family: 'Late Face', Sans; width: 10ch }",
    );
    let ids = ["ch", "ex", "cap", "ic", "rch", "late"]
        .map(|id| fixture.element(BODY, "div", &[("id", id)]));
    let pack = fixture.pack_with_assets(&[("/sans.woff2", 5, sans)]);
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), send).unwrap();
    engine.update_layout(&input()).unwrap();
    let widths = |engine: &StaticDocument<'_>| {
        ids.map(|id| super::find(engine.snapshot().0, id).border_box.size.width)
    };
    // Chromium 159 with Liberation Sans at 13px, the root at 16px. `ic`
    // falls back to 1em without a 水 glyph.
    let chromium = [245.8125, 68.671875, 89.4375, 130.0, 88.96875, 72.296875];
    for (width, chromium) in widths(&engine).into_iter().zip(chromium) {
        assert!((width - chromium).abs() < 0.02, "{width} vs {chromium}");
    }
    // A face that loads later becomes the primary font, and the styles
    // that resolved against the fallback compute again.
    define(
        &mut engine,
        1,
        "Late Face",
        json!({"source":{"bufferId":1}}),
        Some(WOFF2),
    );
    await_font(&mut engine, 1);
    engine.update_layout(&input()).unwrap();
    let late = widths(&engine)[5];
    assert!((late - 78.0).abs() < 0.02, "{late}");
}
