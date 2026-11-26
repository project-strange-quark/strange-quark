// SPDX-License-Identifier: MIT

use squark_journal::{BatchDecoder, BatchWriter, JournalError, RecordError};
use squark_types::NodeId;
use squark_types::journal::{
    AtomKind, BATCH_HEADER_SIZE, ElementState, FOCUS_BLUR, FOCUS_PREVENT_SCROLL, FontFaceAction,
    ListenerFlags, MAX_RECORD_BYTES, NO_STRING, NodeType, Opcode::*, RECORD_SIZE, Record,
    SELECTION_BACKWARD, STYLE_IMPORTANT,
};

const GOLDEN: &[u8] =
    include_bytes!("../../../packages/@strange-quark/dom/test/fixtures/journal-v1.bin");
const NEXT: &[u8] =
    include_bytes!("../../../packages/@strange-quark/dom/test/fixtures/journal-v1-next.bin");
const ROOT: NodeId = NodeId::from_raw(1);

#[test]
fn validates_text_editing_records_in_utf16() {
    let mut writer = BatchWriter::new();
    let tag = writer
        .intern(AtomKind::Tag, &[105, 110, 112, 117, 116])
        .unwrap();
    let html: Vec<_> = "http://www.w3.org/1999/xhtml".encode_utf16().collect();
    let ns = writer.intern(AtomKind::Namespace, &html).unwrap();
    writer.record(CreateElement, 0, 2, tag, ns).unwrap();
    writer
        .record(SetSelection, SELECTION_BACKWARD, 2, 1, 3)
        .unwrap();
    writer.record(SetPreedit, 0, 2, "😀", 2).unwrap();
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    let bytes = writer.finish();
    let batch = decoder.validate(&bytes[0]).unwrap();
    let [selection, preedit] = batch
        .records()
        .skip(3)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert_eq!(
        (selection.op, selection.flags, selection.b, selection.c),
        (SetSelection, SELECTION_BACKWARD, 1, 3)
    );
    assert_eq!(
        (
            preedit.op,
            batch.strings().get(preedit.b).unwrap().units().len(),
            preedit.c
        ),
        (SetPreedit, 2, 2)
    );
    batch.commit();

    for (op, flags, label) in [
        (SetSelection, 0, "selection start exceeds end"),
        (SetPreedit, 0, "cursor exceeds preedit length"),
    ] {
        let mut writer = BatchWriter::new();
        let b: squark_journal::Operand<'_> = if op == SetSelection {
            4.into()
        } else {
            "😀".into()
        };
        writer.record(op, flags, 2, b, 3).unwrap();
        assert_eq!(
            decoder.validate(&writer.finish()[0]).unwrap_err(),
            JournalError::Record {
                index: 0,
                error: RecordError::Operand(label)
            }
        );
    }

    let mut writer = BatchWriter::new();
    writer.record(SetSelection, 2, 2, 0, 0).unwrap();
    assert_eq!(
        decoder.validate(&writer.finish()[0]).unwrap_err(),
        JournalError::Record {
            index: 0,
            error: RecordError::Flags(2)
        }
    );
}

#[test]
fn important_style_flag_matches_the_node_vector() {
    let golden =
        include_bytes!("../../../packages/@strange-quark/dom/test/fixtures/style-important.bin");
    let mut writer = BatchWriter::new();
    let tag = writer
        .intern(AtomKind::Tag, &[b'd' as u16, b'i' as u16, b'v' as u16])
        .unwrap();
    let html = "http://www.w3.org/1999/xhtml"
        .encode_utf16()
        .collect::<Vec<_>>();
    let namespace = writer.intern(AtomKind::Namespace, &html).unwrap();
    writer.record(CreateElement, 0, 1, tag, namespace).unwrap();
    let color = "color".encode_utf16().collect::<Vec<_>>();
    let property = writer.intern(AtomKind::CssProperty, &color).unwrap();
    writer
        .record(SetStyle, STYLE_IMPORTANT, 1, property, "red")
        .unwrap();
    assert_eq!(writer.finish(), [golden]);
    let mut decoder = BatchDecoder::new(NodeId::NONE).unwrap();
    let batch = decoder.validate(golden).unwrap();
    let style = batch.records().last().unwrap();
    assert_eq!(style.flags, STYLE_IMPORTANT);
    assert_eq!(
        batch.strings().get(style.c).unwrap().to_string_lossy(),
        "red"
    );
    drop(batch);
    let mut invalid = golden.to_vec();
    invalid[BATCH_HEADER_SIZE + RECORD_SIZE * 4 + 1] |= 2;
    assert!(matches!(
        decoder.validate(&invalid),
        Err(JournalError::Record {
            error: RecordError::Flags(3),
            ..
        })
    ));
}

#[test]
fn writer_and_decoder_match_the_node_vectors() {
    let mut writer = BatchWriter::new();
    let [tag, html, attr, href, css] = [
        (AtomKind::Tag, "canvas"),
        (AtomKind::Namespace, "http://www.w3.org/1999/xhtml"),
        (AtomKind::Attribute, "canvas"),
        (
            AtomKind::QualifiedAttribute,
            "http://www.w3.org/1999/xlink\0xlink:href",
        ),
        (AtomKind::CssProperty, "--x"),
    ]
    .map(|(kind, name)| {
        writer
            .intern(kind, &name.encode_utf16().collect::<Vec<_>>())
            .unwrap()
    });
    let text = [b'A' as u16, 0xd800, 0xd83d, 0xdca1];
    writer.record(CreateElement, 0, 2, tag, html).unwrap();
    writer.record(CreateText, 0, 3, &text[..], 0).unwrap();
    writer
        .record(CreateNode, 0, 4, NodeType::Comment as u32, "")
        .unwrap();
    writer
        .record(CreateNode, 0, 5, NodeType::DocumentType as u32, "html")
        .unwrap();
    writer
        .record(CreateNode, 0, 6, NodeType::Document as u32, NO_STRING)
        .unwrap();
    writer
        .record(
            CreateNode,
            0,
            7,
            NodeType::DocumentFragment as u32,
            NO_STRING,
        )
        .unwrap();
    writer.record(Insert, 0, 1, 5, 0).unwrap();
    writer.record(Insert, 0, 1, 2, 0).unwrap();
    writer.record(Insert, 0, 2, 3, 0).unwrap();
    writer.record(Insert, 0, 2, 4, 3).unwrap();
    writer.record(SetAttr, 0, 2, href, "#item").unwrap();
    writer.record(SetAttr, 0, 2, attr, "a").unwrap();
    writer.record(RemoveAttr, 0, 2, href, 0).unwrap();
    writer.record(SetText, 0, 4, "comment", 0).unwrap();
    writer.record(SetStyle, 0, 2, css, "").unwrap();
    writer
        .record(
            SetState,
            0,
            2,
            ElementState::INVALID.bits() | ElementState::FOCUS_VISIBLE.bits(),
            0xfff,
        )
        .unwrap();
    writer.record(SetValue, 0, 2, "value", 0).unwrap();
    writer
        .record(SheetInsert, 0, 2, 0, "canvas{color:red}")
        .unwrap();
    writer.record(SheetDelete, 0, 2, 0, 0).unwrap();
    writer
        .record(ScrollTo, 0, 2, 1.5f32.to_bits(), (-2.0f32).to_bits())
        .unwrap();
    writer
        .record(Focus, 0, 2, FOCUS_BLUR | FOCUS_PREVENT_SCROLL, 0)
        .unwrap();
    writer.record(CanvasSize, 0, 2, 320, 200).unwrap();
    writer.record(CanvasSubmit, 0, 2, 9, 12).unwrap();
    let descriptor = r#"{"source":{"bufferId":11},"weight":"400"}"#;
    for action in [
        FontFaceAction::Define,
        FontFaceAction::Add,
        FontFaceAction::Remove,
    ] {
        writer
            .record(FontFace, action as u8, 3, "Test", descriptor)
            .unwrap();
    }
    let wheel = ListenerFlags::NON_PASSIVE_WHEEL.bits();
    let touch = ListenerFlags::NON_PASSIVE_TOUCHMOVE.bits();
    writer.record(Listeners, 0, 0, wheel | touch, 0).unwrap();
    writer.record(Listeners, 0, 2, touch, 0).unwrap();
    writer.record(Remove, 0, 2, 3, 0).unwrap();
    writer.record(Release, 0, 3, 0, 0).unwrap();
    writer.end_batch();
    assert_eq!(
        writer
            .intern(AtomKind::Tag, &"canvas".encode_utf16().collect::<Vec<_>>())
            .unwrap(),
        tag
    );
    let reused = NodeId::new(3, 1).unwrap().raw();
    writer.record(CreateElement, 0, reused, tag, html).unwrap();
    writer.record(Insert, 0, 2, reused, 0).unwrap();
    assert_eq!(writer.finish(), [GOLDEN, NEXT]);

    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    // A pack's batch directory can point to unaligned bytes.
    let mut unaligned = vec![0xff];
    unaligned.extend_from_slice(GOLDEN);
    let batch = decoder.validate(&unaligned[1..]).unwrap();
    assert_eq!(batch.records().len(), 35);
    let create_text = batch
        .records()
        .find(|record| record.op == CreateText)
        .unwrap();
    let string = batch.strings().get(create_text.b).unwrap();
    assert!(string.units().eq(text));
    assert_eq!(string.to_string_lossy(), "A�💡");
    assert_eq!(
        batch
            .strings()
            .get(create_text.b + 1)
            .unwrap()
            .units()
            .len(),
        0
    );
    assert_eq!(batch.atom(href).unwrap().kind, AtomKind::QualifiedAttribute);
    assert_eq!(
        &*batch.atom(href).unwrap().text,
        &"http://www.w3.org/1999/xlink\0xlink:href"
            .encode_utf16()
            .collect::<Vec<_>>()
    );
    batch.commit();

    let batch = decoder.validate(NEXT).unwrap();
    assert_eq!(
        batch.records().next().unwrap(),
        Record {
            op: CreateElement,
            flags: 0,
            a: reused,
            b: tag,
            c: html
        }
    );
    assert_eq!(batch.records().len(), 2);
    assert!(batch.strings().is_empty());
    assert_eq!(batch.atom(tag).unwrap().kind, AtomKind::Tag);
    batch.commit();
}

#[test]
fn malformed_batches_publish_neither_atoms_nor_generations() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    let mut bad = GOLDEN.to_vec();
    // Corrupt the final RELEASE after a batch of otherwise valid mutations.
    bad[BATCH_HEADER_SIZE + 34 * RECORD_SIZE + 1] = 1;
    assert_eq!(
        decoder.validate(&bad).unwrap_err(),
        JournalError::Record {
            index: 34,
            error: RecordError::Flags(1)
        }
    );
    assert!(decoder.validate(NEXT).is_err());

    // The mirror may decline an otherwise valid batch before acknowledgment.
    drop(decoder.validate(GOLDEN).unwrap());
    assert!(decoder.validate(NEXT).is_err());

    decoder.validate(GOLDEN).unwrap().commit();
    decoder.validate(NEXT).unwrap().commit();
    assert!(decoder.validate(NEXT).is_err());
}

#[test]
fn rejects_truncation_trailing_bytes_and_invalid_boundaries() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    for len in 0..GOLDEN.len() {
        assert!(
            decoder.validate(&GOLDEN[..len]).is_err(),
            "accepted {len} of {} bytes",
            GOLDEN.len()
        );
    }
    let mut trailing = GOLDEN.to_vec();
    trailing.push(0);
    assert!(decoder.validate(&trailing).is_err());

    let offsets_start = BATCH_HEADER_SIZE + 35 * RECORD_SIZE;
    for (offset, word) in [
        (0, 65_552u32),
        (0, 17),
        (4, u32::MAX),
        (8, u32::MAX),
        (12, 2),
        (offsets_start, 1),
        (offsets_start + 8, 5),
        (offsets_start + 8, 261),
        (offsets_start + 20 * 4, 259),
    ] {
        let mut bytes = GOLDEN.to_vec();
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        assert!(matches!(
            decoder.validate(&bytes),
            Err(JournalError::Batch(_))
        ));
    }
}

#[test]
fn rejects_invalid_operands_atom_kinds_and_reserved_bits() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    for (index, offset, word) in [
        (0, 4, 0u32),        // atom zero
        (0, 12, 6),          // unknown atom kind
        (1, 4, 1),           // changes atom 1's kind and text
        (4, 4, 7),           // the later style record refers to undefined 5
        (5, 8, 2),           // namespace used as a tag
        (5, 12, 9),          // undefined namespace
        (6, 8, 20),          // out-of-range string index
        (6, 12, 1),          // nonzero unused operand
        (7, 8, 3),           // nodeType 3 uses CREATE_TEXT
        (9, 12, 0),          // absent string needs the sentinel
        (11, 4, 0x01000001), // invalid root generation
        (15, 8, 1),          // tag used as an attribute name
        (19, 8, 3),          // attribute name used as a CSS property
        (20, 8, 0x1000),     // reserved state bit
        (20, 12, 0x1000),    // reserved mask bit
        (22, 4, 0x01000002), // stale stylesheet owner
        (25, 8, 4),          // reserved focus bit
        (31, 8, 4),          // reserved listener bit
    ] {
        let mut bytes = GOLDEN.to_vec();
        let offset = BATCH_HEADER_SIZE + index * RECORD_SIZE + offset;
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        assert!(
            matches!(decoder.validate(&bytes), Err(JournalError::Record { .. })),
            "accepted mutation at record {index}, byte {offset}"
        );
    }

    for (index, offset, byte) in [(0, 0, 0xff), (0, 2, 1), (0, 3, 1), (28, 1, 3)] {
        let mut bytes = GOLDEN.to_vec();
        bytes[BATCH_HEADER_SIZE + index * RECORD_SIZE + offset] = byte;
        assert!(
            matches!(decoder.validate(&bytes), Err(JournalError::Record { index: error_index, .. }) if error_index == index)
        );
    }
}

#[test]
fn template_fragment_owner_must_be_a_live_html_template() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    let mut writer = BatchWriter::new();
    let template = writer
        .intern(
            AtomKind::Tag,
            &"template".encode_utf16().collect::<Vec<_>>(),
        )
        .unwrap();
    let html = writer
        .intern(
            AtomKind::Namespace,
            &"http://www.w3.org/1999/xhtml"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    writer.record(CreateElement, 0, 2, template, html).unwrap();
    writer.record(CreateElement, 0, 3, template, 0).unwrap();
    writer.record(CreateElement, 0, 4, template, html).unwrap();
    writer.record(Release, 0, 4, 0, 0).unwrap();
    decoder.validate(&writer.finish()[0]).unwrap().commit();
    for (owner, valid) in [
        (2, true),
        (NO_STRING, true),
        (ROOT.raw(), false),
        (0x01000002, false),
        (3, false),
        (4, false),
        (6, false),
    ] {
        let mut writer = BatchWriter::new();
        writer
            .record(CreateNode, 0, 5, NodeType::DocumentFragment as u32, owner)
            .unwrap();
        assert_eq!(decoder.validate(&writer.finish()[0]).is_ok(), valid);
    }
}

#[test]
fn atom_redefinition_requires_exact_kind_and_code_units() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    decoder.validate(GOLDEN).unwrap().commit();
    for (text, kind, valid) in [
        ("canvas", AtomKind::Tag, true),
        ("Canvas", AtomKind::Tag, false),
        ("canvas", AtomKind::Attribute, false),
    ] {
        let mut writer = BatchWriter::new();
        writer.record(DefineAtom, 0, 1, text, kind as u32).unwrap();
        assert_eq!(decoder.validate(&writer.finish()[0]).is_ok(), valid);
    }
    for malformed in ["namespace", "\0qname", "namespace\0", "ns\0more\0qname"] {
        let mut writer = BatchWriter::new();
        writer
            .record(
                DefineAtom,
                0,
                6,
                malformed,
                AtomKind::QualifiedAttribute as u32,
            )
            .unwrap();
        assert!(decoder.validate(&writer.finish()[0]).is_err());
    }
}

#[test]
fn generation_checks_reject_early_reuse_stale_references_and_wrap() {
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    let mut writer = BatchWriter::new();
    writer.record(CreateText, 0, 2, "text", 0).unwrap();
    writer.record(Release, 0, 2, 0, 0).unwrap();
    writer.record(CreateText, 0, 0x01000002, "next", 0).unwrap();
    assert!(decoder.validate(&writer.finish()[0]).is_err());

    for generation in 0..=u8::MAX {
        let id = NodeId::new(2, generation).unwrap();
        let mut writer = BatchWriter::new();
        writer.record(CreateText, 0, id.raw(), "text", 0).unwrap();
        writer.record(Release, 0, id.raw(), 0, 0).unwrap();
        decoder.validate(&writer.finish()[0]).unwrap().commit();
    }
    for (opcode, id) in [
        (CreateText, 2),
        (CreateText, 0x01000000),
        (SetText, 0xff000002),
    ] {
        let mut writer = BatchWriter::new();
        writer.record(opcode, 0, id, "text", 0).unwrap();
        assert!(decoder.validate(&writer.finish()[0]).is_err());
    }
}

#[test]
fn rollover_keeps_large_strings_with_their_records_and_reuses_atoms() {
    let mut writer = BatchWriter::new();
    let tag = writer.intern(AtomKind::Tag, &[b'p' as u16]).unwrap();
    for _ in 1..MAX_RECORD_BYTES / RECORD_SIZE {
        writer.record(Listeners, 0, 0, 0, 0).unwrap();
    }
    writer.record(CreateElement, 0, 2, tag, 0).unwrap();
    let text = vec![0xd800; MAX_RECORD_BYTES];
    writer.record(CreateText, 0, 3, &text[..], 0).unwrap();
    let batches = writer.finish();
    assert_eq!(batches.len(), 2);
    let mut decoder = BatchDecoder::new(ROOT).unwrap();
    let batch = decoder.validate(&batches[0]).unwrap();
    assert_eq!(batch.records().len(), MAX_RECORD_BYTES / RECORD_SIZE);
    batch.commit();
    let batch = decoder.validate(&batches[1]).unwrap();
    assert_eq!(batch.records().len(), 2);
    assert!(batch.strings().get(0).unwrap().units().eq(text));
    batch.commit();
}
