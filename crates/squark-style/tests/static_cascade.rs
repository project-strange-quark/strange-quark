// SPDX-License-Identifier: MIT
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use squark_assets::Pack;
use squark_journal::BatchDecoder;
use squark_mirror::Document;
use squark_style::{Cascade, screen};
use squark_types::NodeId;
use squark_types::journal::{Opcode, Record, STYLE_IMPORTANT};
use style::servo_arc::Arc;
use url::Url;

const HTML: u32 = 2;
const STYLE: u32 = 3;
const CSS: u32 = 4;
const LINK: u32 = 5;
const BODY: u32 = 6;
const CARD: u32 = 7;
const CHILD: u32 = 8;
const SIBLING: u32 = 9;
const UNRELATED: u32 = 10;
const CLASS: u32 = 9;

#[test]
fn cascades_packed_styles_in_tree_order_and_reacts_to_viewport_media() {
    let pack = fixture();
    let desktop = screen(800.0, 600.0, 1.0);
    let (mut doc, _, mut cascade) = load(&pack);
    cascade.update(&mut doc, desktop.clone()).unwrap();
    assert_eq!(
        width(&doc, CARD),
        280.0,
        "later link wins and media matches"
    );
    assert!(
        doc.computed_style(id(STYLE))
            .unwrap()
            .slow_clone_display()
            .is_none()
    );
    assert!(
        doc.computed_style(id(LINK))
            .unwrap()
            .slow_clone_display()
            .is_none()
    );
    let body = doc.computed_style(id(BODY)).unwrap();
    let style::values::generics::length::GenericMargin::LengthPercentage(margin) =
        &body.get_margin().margin_top
    else {
        panic!("expected UA body margin");
    };
    assert_eq!(margin.to_length().unwrap().px(), 8.0);
    cascade.update(&mut doc, screen(600.0, 800.0, 1.0)).unwrap();
    assert_eq!(width(&doc, CARD), 160.0, "link media no longer matches");

    // Removing a media rule alone is insufficient when a viewport unit changes.
    cascade
        .update(&mut doc, screen(1000.0, 600.0, 1.0))
        .unwrap();
    assert_eq!(width(&doc, CARD), 350.0);
}

#[test]
fn incremental_class_attribute_style_and_state_match_full_cascade() {
    let pack = fixture();
    let media = screen(800.0, 600.0, 1.0);
    let (mut incremental, mut decoder, mut cascade) = load(&pack);
    cascade.update(&mut incremental, media.clone()).unwrap();

    let (mut full, mut full_decoder, mut full_cascade) = load(&pack);
    let unrelated = incremental.computed_style(id(UNRELATED)).unwrap();
    let writes = batch_bytes(
        &[
            record(Opcode::SetAttr, CARD, CLASS, 0),
            record(Opcode::SetAttr, CHILD, 12, 1),
            record(Opcode::SetState, SIBLING, 1, 1),
            record(Opcode::SetStyle, CHILD, 13, 2),
        ],
        &["card open", "yes", "27px"],
    );
    incremental.apply_journal(&mut decoder, &writes).unwrap();
    full.apply_journal(&mut full_decoder, &writes).unwrap();
    cascade.update(&mut incremental, media.clone()).unwrap();
    full_cascade.update(&mut full, media).unwrap();
    for node in [CARD, CHILD, SIBLING, UNRELATED] {
        let inc = incremental.computed_style(id(node)).unwrap();
        let full = full.computed_style(id(node)).unwrap();
        assert_eq!(
            inc.get_position().width,
            full.get_position().width,
            "node {node}"
        );
        assert_eq!(
            inc.get_font().font_size,
            full.get_font().font_size,
            "node {node}"
        );
    }
    assert_eq!(width(&incremental, CARD), 370.0);
    assert_eq!(width(&incremental, CHILD), 93.0);
    assert_eq!(width(&incremental, SIBLING), 112.0);
    assert_eq!(
        incremental
            .computed_style(id(CHILD))
            .unwrap()
            .get_font()
            .font_size
            .used_size
            .0
            .px(),
        27.0
    );
    assert!(Arc::ptr_eq(
        &unrelated,
        &incremental.computed_style(id(UNRELATED)).unwrap()
    ));
}

#[test]
fn structural_mutations_match_a_fresh_cascade_without_visiting_unrelated_branches() {
    use std::time::Instant;

    let pack = fixture();
    let media = screen(800.0, 600.0, 1.0);
    let (mut incremental, mut decoder, mut cascade) = load(&pack);
    let (mut full, mut full_decoder, mut full_cascade) = load(&pack);
    let mut branches = Vec::new();
    for branch in 0..960 {
        let div = 100 + branch * 2;
        let span = div + 1;
        branches.extend([
            record(Opcode::CreateElement, div, 6, 1),
            record(Opcode::CreateElement, span, 7, 1),
            record(Opcode::Insert, BODY, div, CARD),
            record(Opcode::Insert, div, span, 0),
        ]);
    }
    let setup = batch_bytes(&branches, &[]);
    incremental.apply_journal(&mut decoder, &setup).unwrap();
    full.apply_journal(&mut full_decoder, &setup).unwrap();
    cascade.update(&mut incremental, media.clone()).unwrap();
    full_cascade.update(&mut full, media.clone()).unwrap();
    let unrelated = incremental.computed_style(id(101)).unwrap();
    let mut timing = Vec::new();

    for (edits, expected) in [
        (
            vec![
                record(Opcode::Remove, CARD, SIBLING, 0),
                record(Opcode::Insert, CARD, SIBLING, CHILD),
            ],
            [Some(72.0), Some(41.0), Some(33.0)],
        ),
        (
            vec![
                record(Opcode::Remove, CARD, CHILD, 0),
                record(Opcode::Insert, BODY, CHILD, UNRELATED),
                record(Opcode::Remove, CARD, SIBLING, 0),
            ],
            [None, None, Some(33.0)],
        ),
        (
            vec![record(Opcode::Remove, BODY, CHILD, 0)],
            [None, None, Some(57.0)],
        ),
        (
            vec![
                record(Opcode::Insert, CARD, CHILD, 0),
                record(Opcode::Insert, CARD, SIBLING, 0),
            ],
            [Some(11.0), Some(33.0), Some(33.0)],
        ),
    ] {
        let edits = batch_bytes(&edits, &[]);
        incremental.apply_journal(&mut decoder, &edits).unwrap();
        full.apply_journal(&mut full_decoder, &edits).unwrap();
        let start = Instant::now();
        cascade.update(&mut incremental, media.clone()).unwrap();
        let targeted_time = start.elapsed();
        let start = Instant::now();
        // A different viewport forces the baseline full cascade. None of the
        // fixture's declarations depend on the small width change.
        let viewport = 801.0 + timing.len() as f32;
        full_cascade
            .update(&mut full, screen(viewport, 600.0, 1.0))
            .unwrap();
        let full_time = start.elapsed();
        let nodes = [CHILD, SIBLING, UNRELATED];
        for (node, expected) in nodes.into_iter().zip(expected) {
            if let Some(expected) = expected {
                assert_eq!(width(&full, node), expected, "full style for {node}");
                assert_eq!(
                    width(&incremental, node),
                    expected,
                    "incremental style for {node}"
                );
            } else if node == CHILD && incremental.parent(id(CHILD)).is_some() {
                assert_eq!(
                    incremental
                        .computed_style(id(CHILD))
                        .unwrap()
                        .get_position()
                        .width,
                    full.computed_style(id(CHILD)).unwrap().get_position().width
                );
            }
        }
        assert!(incremental.restyled_elements() < 30);
        assert!(full.restyled_elements() > 1900);
        assert!(Arc::ptr_eq(
            &unrelated,
            &incremental.computed_style(id(101)).unwrap()
        ));
        timing.push((
            incremental.restyled_elements(),
            targeted_time,
            full.restyled_elements(),
            full_time,
        ));
    }
    for (targeted, elapsed, full, baseline) in timing {
        println!(
            "targeted {targeted} elements in {elapsed:?}; full {full} elements in {baseline:?}"
        );
    }
}

fn width(document: &Document, node: u32) -> f32 {
    use style::values::computed::Size;
    let style = document.computed_style(id(node)).unwrap();
    match &style.get_position().width {
        Size::LengthPercentage(width) => width.0.to_length().unwrap().px(),
        other => panic!("unexpected width {other:?} for node {node}"),
    }
}

fn load(pack: &Pack) -> (Document, BatchDecoder, Cascade<'_>) {
    Cascade::from_pack(
        pack,
        Url::parse("https://pack.test/index.html").unwrap(),
        screen(800.0, 600.0, 1.0),
    )
    .unwrap()
}

fn id(slot: u32) -> NodeId {
    NodeId::from_raw(slot)
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

fn string_table(strings: &[&str]) -> Vec<u8> {
    let mut table = vec![0u32];
    let mut units = Vec::new();
    for string in strings {
        units.extend(string.encode_utf16());
        table.push(units.len() as u32);
    }
    [strings.len() as u32, units.len() as u32]
        .into_iter()
        .chain(table)
        .flat_map(u32::to_le_bytes)
        .chain(units.into_iter().flat_map(u16::to_le_bytes))
        .collect()
}

fn batch_bytes(records: &[Record], strings: &[&str]) -> Vec<u8> {
    let strings = string_table(strings);
    let mut bytes = ((records.len() * 16) as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(&strings[..8]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend(records.iter().flat_map(Record::encode));
    bytes.extend_from_slice(&strings[8..]);
    bytes
}

fn fixture() -> Pack {
    use Opcode::{CreateElement, CreateText, DefineAtom, Insert, SetAttr};
    let names = [
        "http://www.w3.org/1999/xhtml",
        "html",
        "style",
        "link",
        "body",
        "div",
        "span",
        "b",
        "class",
        "rel",
        "media",
        "data-active",
        "font-size",
        "card",
        "stylesheet",
        "(min-width: 700px)",
        ".card { width: 160px; font-size: 21px }
         .card > span { width: 11px }
         .open > span[data-active=yes] { width: 93px }
         .card > span.important { width: 11px !important }
         .open > span + b:hover { width: 112px }
         body > b, .card > b { width: 33px }
         .card > b:nth-child(1) { width: 41px }
         .card:has(> b:first-child) > span { width: 72px }
         .card:empty + b { width: 57px }",
    ];
    let kinds = [4, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3];
    let atoms = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| record(DefineAtom, index as u32 + 1, index as u32, kind));
    let records: Vec<_> = atoms
        .chain([
            record(CreateElement, HTML, 2, 1),
            record(CreateElement, STYLE, 3, 1),
            record(CreateText, CSS, 16, 0),
            record(CreateElement, LINK, 4, 1),
            record(CreateElement, BODY, 5, 1),
            record(CreateElement, CARD, 6, 1),
            record(CreateElement, CHILD, 7, 1),
            record(CreateElement, SIBLING, 8, 1),
            record(CreateElement, UNRELATED, 8, 1),
            record(SetAttr, CARD, CLASS, 13),
            record(SetAttr, LINK, 10, 14),
            record(SetAttr, LINK, 11, 15),
            record(Insert, 1, HTML, 0),
            record(Insert, HTML, STYLE, 0),
            record(Insert, STYLE, CSS, 0),
            record(Insert, HTML, LINK, 0),
            record(Insert, HTML, BODY, 0),
            record(Insert, BODY, CARD, 0),
            record(Insert, CARD, CHILD, 0),
            record(Insert, CARD, SIBLING, 0),
            record(Insert, BODY, UNRELATED, 0),
        ])
        .collect();
    let batch = batch_bytes(&records, &names);
    let table = string_table(&["/assets/site.css", "/index.html"]);
    let scripts_pos = 80 + table.len();
    let batch_pos = scripts_pos + 16;
    let mut doc = b"SQDOC\0\0\0".to_vec();
    doc.extend(1u16.to_le_bytes());
    doc.extend(64u16.to_le_bytes());
    doc.extend([0u32, 1, 1].into_iter().flat_map(u32::to_le_bytes));
    doc.extend(
        [64u64, 80, table.len() as u64]
            .into_iter()
            .flat_map(u64::to_le_bytes),
    );
    doc.extend([1u32, 0].into_iter().flat_map(u32::to_le_bytes));
    doc.extend(
        [scripts_pos as u64, batch_pos as u64, batch.len() as u64]
            .into_iter()
            .flat_map(u64::to_le_bytes),
    );
    doc.extend(table);
    doc.extend([4u32, LINK, 0, 1].into_iter().flat_map(u32::to_le_bytes));
    doc.extend(batch);

    let css = b".card { width: 35vw } .card:has([data-active=yes]) { width: 370px }";
    let paths = ["/__squark/document.bin", "/assets/site.css"];
    let data = [doc.as_slice(), css.as_slice()];
    let data_start = 192;
    let mut pack = b"SQPK\x00\x01\x00\x00".to_vec();
    pack.extend(
        [2u32, 32, 96, paths.concat().len() as u32, data_start, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes),
    );
    let mut path_offset = 0;
    let mut data_offset = 0u32;
    for ((path, bytes), mime) in paths.into_iter().zip(data).zip([0u16, 3]) {
        let hash = path.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        pack.extend(hash.to_le_bytes());
        pack.extend((path_offset as u32).to_le_bytes());
        pack.extend((path.len() as u16).to_le_bytes());
        pack.extend(mime.to_le_bytes());
        pack.extend(
            [data_offset, bytes.len() as u32, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        path_offset += path.len();
        data_offset = (data_offset + bytes.len() as u32).next_multiple_of(8);
    }
    pack.extend(paths.concat().as_bytes());
    pack.resize(data_start as usize, 0);
    for bytes in data {
        pack.resize(pack.len().next_multiple_of(8), 0);
        pack.extend_from_slice(bytes);
    }

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&pack).unwrap();
    file.seek(SeekFrom::Start(5)).unwrap();
    let pack = Pack::open(&file).unwrap();
    assert_eq!(file.stream_position().unwrap(), 5);
    pack
}

#[test]
fn replays_the_tools_golden_pack_through_the_cascade() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pack/basic.appended.bin"
    );
    let pack = Pack::open(&File::open(path).unwrap()).unwrap();
    let (mut doc, _, mut cascade) = load(&pack);
    cascade.update(&mut doc, screen(800.0, 600.0, 1.0)).unwrap();

    let mut nodes = vec![doc.root()];
    let mut card = None;
    while let Some(node) = nodes.pop() {
        if doc
            .attribute(node, "id")
            .is_some_and(|attr| String::from_utf16_lossy(attr) == "app")
        {
            card = Some(node);
            break;
        }
        nodes.extend(doc.children(node).unwrap());
    }
    let card = card.unwrap();
    assert_eq!(width(&doc, card.raw()), 200.0);
    assert_eq!(
        doc.computed_style(card).unwrap().get_color().components.0,
        34.0 / 255.0
    );
}

#[test]
fn inline_set_style_priority_can_override_and_be_lowered() {
    let pack = fixture();
    let media = screen(800.0, 600.0, 1.0);
    let (mut doc, mut decoder, mut cascade) = load(&pack);
    cascade.update(&mut doc, media.clone()).unwrap();

    let setup = batch_bytes(
        &[
            record(Opcode::DefineAtom, 31, 0, 3),
            record(Opcode::SetAttr, CHILD, CLASS, 1),
        ],
        &["width", "important"],
    );
    doc.apply_journal(&mut decoder, &setup).unwrap();
    for (flags, expected) in [(STYLE_IMPORTANT, 45.0), (0, 11.0)] {
        let write = Record {
            flags,
            ..record(Opcode::SetStyle, CHILD, 31, 0)
        };
        doc.apply_journal(&mut decoder, &batch_bytes(&[write], &["45px"]))
            .unwrap();
        cascade.update(&mut doc, media.clone()).unwrap();
        assert_eq!(width(&doc, CHILD), expected);
    }
}
