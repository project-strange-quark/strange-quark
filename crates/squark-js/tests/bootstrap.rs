// SPDX-License-Identifier: MIT

#![allow(unsafe_code)]

mod support;
use support::{export, initialize_v8, load_dom_fixture};

use squark_journal::{BatchDecoder, BatchWriter};
use squark_js::bootstrap::{BootstrapError, bootstrap_document};
use squark_types::NodeId;
use squark_types::journal::{AtomKind, NO_STRING, NodeType, Opcode::*};
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DocumentHeader, QuirksMode,
};

const ROOT: NodeId = NodeId::from_raw(0x0700_0002);

fn fixture(quirks: u32) -> (Vec<u8>, [usize; 2]) {
    let mut writer = BatchWriter::new();
    let mut intern = |kind, text: &str| {
        writer
            .intern(kind, &text.encode_utf16().collect::<Vec<_>>())
            .unwrap()
    };
    let html = intern(AtomKind::Namespace, "http://www.w3.org/1999/xhtml");
    let svg = intern(AtomKind::Namespace, "http://www.w3.org/2000/svg");
    let [html_tag, head, style, body, div, svg_tag, link] =
        ["html", "head", "style", "body", "div", "svg", "link"]
            .map(|name| intern(AtomKind::Tag, name));
    let id = intern(AtomKind::Attribute, "id");
    let href = intern(
        AtomKind::QualifiedAttribute,
        "http://www.w3.org/1999/xlink\0xlink:href",
    );
    let template = intern(AtomKind::Tag, "template");

    writer
        .record(CreateNode, 0, 3, NodeType::DocumentType as u32, "html")
        .unwrap();
    writer.record(CreateElement, 0, 4, html_tag, html).unwrap();
    writer.record(CreateElement, 0, 6, head, html).unwrap();
    writer.record(CreateElement, 0, 40, style, html).unwrap();
    writer.record(CreateElement, 0, 41, link, html).unwrap();
    writer.record(CreateElement, 0, 55, body, html).unwrap();
    writer.record(CreateElement, 0, 56, div, html).unwrap();
    writer
        .record(CreateText, 0, 57, ".app { color: red }", 0)
        .unwrap();
    writer.record(Insert, 0, ROOT.raw(), 4, 0).unwrap();
    writer.record(Insert, 0, ROOT.raw(), 3, 4).unwrap();
    writer.record(Insert, 0, 4, 6, 0).unwrap();
    writer.record(Insert, 0, 4, 55, 0).unwrap();
    writer.record(Insert, 0, 6, 40, 0).unwrap();
    writer.record(Insert, 0, 6, 41, 0).unwrap();
    writer.record(Insert, 0, 40, 57, 0).unwrap();
    writer.record(Insert, 0, 55, 56, 0).unwrap();
    writer
        .record(SheetInsert, 0, 40, 0, ".app { color: red }")
        .unwrap();
    writer.end_batch();

    writer.record(SetAttr, 0, 56, id, "app").unwrap();
    writer.record(CreateElement, 0, 90, svg_tag, svg).unwrap();
    writer.record(SetAttr, 0, 90, href, "#target").unwrap();
    writer
        .record(CreateText, 0, 91, &[0xd800, 0xd83d, 0xdca1][..], 0)
        .unwrap();
    writer
        .record(
            CreateNode,
            0,
            92,
            NodeType::Comment as u32,
            "omitted from text",
        )
        .unwrap();
    writer
        .record(CreateNode, 0, 200, NodeType::Comment as u32, "detached")
        .unwrap();
    writer.record(Release, 0, 200, 0, 0).unwrap();
    writer.record(Insert, 0, 56, 90, 0).unwrap();
    writer.record(Insert, 0, 56, 91, 90).unwrap();
    writer.record(Insert, 0, 56, 92, 91).unwrap();
    writer.record(CreateElement, 0, 93, template, html).unwrap();
    writer
        .record(CreateNode, 0, 94, NodeType::DocumentFragment as u32, 93)
        .unwrap();
    writer
        .record(CreateText, 0, 95, "inert contents", 0)
        .unwrap();
    writer.record(Insert, 0, 94, 95, 0).unwrap();
    writer.record(Insert, 0, 55, 93, 0).unwrap();
    writer
        .record(
            CreateNode,
            0,
            96,
            NodeType::DocumentFragment as u32,
            NO_STRING,
        )
        .unwrap();
    writer
        .record(SheetInsert, 0, 41, 0, "@media screen { }")
        .unwrap();
    let batches = writer.finish();

    // The document header and directory follow `squark_types::pack`. There are
    // no executable scripts. The empty script string table has one offset.
    let directory = DOCUMENT_HEADER_SIZE;
    let string_table = directory + batches.len() * BATCH_DIR_ENTRY_SIZE;
    let first_batch = string_table + 12;
    let second_batch = first_batch + batches[0].len();
    let header = DocumentHeader {
        quirks_mode: QuirksMode::from_flags(quirks).unwrap(),
        document_node_id: ROOT,
        batch_count: batches.len() as u32,
        batch_dir_offset: directory as u64,
        script_strings_offset: string_table as u64,
        script_strings_len: 12,
        script_count: 0,
        script_table_offset: first_batch as u64,
    };
    let mut bytes = header.encode().to_vec();
    let offsets = [first_batch, second_batch];
    for (&offset, batch) in offsets.iter().zip(&batches) {
        bytes.extend(
            BatchDirEntry {
                offset: offset as u64,
                len: batch.len() as u64,
            }
            .encode(),
        );
    }
    bytes.extend([0; 12]);
    bytes.extend(batches.into_iter().flatten());
    (bytes, offsets)
}

#[test]
fn validates_the_whole_document_before_entering_javascript() {
    initialize_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let source = v8::String::new(
        scope,
        r#"
        (journal, id, mode, batches) => {
            if (id !== 0x07000002 || mode !== 1 || batches.length !== 2
                || !batches.every(([records]) => records instanceof ArrayBuffer)
                || !batches[1][1].some((text) => text.charCodeAt(0) === 0xd800)) {
                throw Error("wrong bootstrap values");
            }
            journal.called = true;
            return journal;
        }
    "#,
    )
    .unwrap();
    let replay = v8::Script::compile(scope, source, None)
        .unwrap()
        .run(scope)
        .unwrap();
    let replay = v8::Local::<v8::Function>::try_from(replay).unwrap();
    let journal = v8::Object::new(scope);
    let called = v8::String::new(scope, "called").unwrap();

    let (valid, [_, second_batch]) = fixture(1);
    for (offset, bytes) in [
        (8, 2u16.to_le_bytes().to_vec()),
        (12, 3u32.to_le_bytes().to_vec()),
        (16, 0u32.to_le_bytes().to_vec()),
        (24, u64::MAX.to_le_bytes().to_vec()),
        (32, 64u64.to_le_bytes().to_vec()),
        (64, (second_batch as u64).to_le_bytes().to_vec()),
    ] {
        let mut invalid = valid.clone();
        invalid[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(matches!(
            bootstrap_document(scope, &invalid, replay, journal),
            Err(BootstrapError::Document(_) | BootstrapError::Header(_))
        ));
    }
    let mut invalid = valid.clone();
    invalid[second_batch + 16] = 0xff;
    assert!(matches!(
        bootstrap_document(scope, &invalid, replay, journal),
        Err(BootstrapError::Journal { batch: 1, .. })
    ));
    assert!(journal.get(scope, called.into()).unwrap().is_undefined());
    assert_eq!(
        bootstrap_document(scope, &valid, replay, journal).unwrap(),
        journal
    );
    assert!(journal.get(scope, called.into()).unwrap().is_true());
}

#[test]
fn replays_batch_writer_and_squark_pack_output_into_the_unbundled_dom_in_v8() {
    initialize_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let namespace = load_dom_fixture(
        scope,
        "bootstrap.mjs",
        include_str!("fixtures/bootstrap.mjs"),
    );
    let replay = export(scope, namespace, "bootstrapDocument");
    let setup = export(scope, namespace, "setup");
    let check = export(scope, namespace, "check");
    let check_tools_fixture = export(scope, namespace, "checkToolsFixture");
    let receiver = v8::undefined(scope);

    for quirks in 0..=2 {
        v8::tc_scope!(let scope, scope);
        let journal =
            v8::Local::<v8::Object>::try_from(setup.call(scope, receiver.into(), &[]).unwrap())
                .unwrap();
        let (bytes, [first_batch, second_batch]) = fixture(quirks);
        let document = bootstrap_document(scope, &bytes, replay, journal).unwrap();
        let mode = v8::Integer::new_from_unsigned(scope, quirks);
        let Some(runtime_batch) = check.call(
            scope,
            receiver.into(),
            &[document.into(), journal.into(), mode.into()],
        ) else {
            let exception = scope.exception().unwrap();
            panic!("{}", exception.to_rust_string_lossy(scope));
        };
        let buffer = v8::Local::<v8::ArrayBuffer>::try_from(runtime_batch).unwrap();
        let runtime_bytes: Vec<_> = buffer
            .get_backing_store()
            .iter()
            .map(std::cell::Cell::get)
            .collect();

        let mut decoder = BatchDecoder::new(ROOT).unwrap();
        decoder
            .validate(&bytes[first_batch..second_batch])
            .unwrap()
            .commit();
        decoder.validate(&bytes[second_batch..]).unwrap().commit();
        let runtime = decoder.validate(&runtime_bytes).unwrap();
        let writes: Vec<_> = runtime
            .records()
            .map(|record| (record.op, record.a, record.b, record.c))
            .collect();
        assert_eq!(
            writes,
            [
                (CreateElement, 201, 7, 1),
                (Insert, 56, 201, 0),
                (DefineAtom, 13, 0, AtomKind::Tag as u32),
                (CreateElement, 202, 13, 1),
            ]
        );
        runtime.commit();
    }

    let tools_fixtures: [(&[u8], u32); 4] = [
        (
            include_bytes!("../../../tests/fixtures/pack/basic.document.bin"),
            0,
        ),
        (
            include_bytes!("../../../tests/fixtures/pack/limited-quirks.document.bin"),
            1,
        ),
        (
            include_bytes!("../../../tests/fixtures/pack/quirks.document.bin"),
            2,
        ),
        (
            include_bytes!("../../../tests/fixtures/pack/template.document.bin"),
            0,
        ),
    ];
    for (bytes, quirks) in tools_fixtures {
        v8::tc_scope!(let scope, scope);
        let journal =
            v8::Local::<v8::Object>::try_from(setup.call(scope, receiver.into(), &[]).unwrap())
                .unwrap();
        let document = bootstrap_document(scope, bytes, replay, journal).unwrap();
        let mode = v8::Integer::new_from_unsigned(scope, quirks);
        let sheet_id =
            v8::Integer::new_from_unsigned(scope, first_sheet_id(bytes).unwrap_or_default());
        let args = [
            document.into(),
            journal.into(),
            mode.into(),
            sheet_id.into(),
        ];
        if check_tools_fixture
            .call(scope, receiver.into(), &args)
            .is_none()
        {
            let exception = scope.exception().unwrap();
            panic!("{}", exception.to_rust_string_lossy(scope));
        }
    }
}

fn first_sheet_id(bytes: &[u8]) -> Option<u32> {
    let header = DocumentHeader::decode(bytes.first_chunk()?).unwrap();
    let directory = header.batch_dir_offset as usize;
    let directory_end = directory + header.batch_count as usize * BATCH_DIR_ENTRY_SIZE;
    let mut decoder = BatchDecoder::new(header.document_node_id).unwrap();
    for entry in bytes[directory..directory_end]
        .as_chunks::<BATCH_DIR_ENTRY_SIZE>()
        .0
    {
        let entry = BatchDirEntry::decode(entry);
        let start = entry.offset as usize;
        let end = start + entry.len as usize;
        let batch = decoder.validate(&bytes[start..end]).unwrap();
        if let Some(record) = batch.records().find(|record| record.op == SheetInsert) {
            return Some(record.a);
        }
        batch.commit();
    }
    None
}
