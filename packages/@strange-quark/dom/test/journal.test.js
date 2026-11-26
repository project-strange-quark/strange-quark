// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  AtomKind, ElementState, FocusFlags, FontFaceAction, Journal,
  ListenerFlags, NO_STRING, NodeType, Opcode,
} from "@strange-quark/dom/journal";

function fixture(name) {
  return readFileSync(new URL(`./fixtures/${name}.bin`, import.meta.url));
}

test("encoder matches the Rust version 1 journal vectors", () => {
  const batches = [];
  const journal = new Journal((buffer, length) => {
    batches.push(Buffer.from(new Uint8Array(buffer, 0, length)));
    return new DataView(buffer).getUint32(0, true) / 16;
  });
  const tag = journal.intern(AtomKind.Tag, "canvas");
  const html = journal.intern(AtomKind.Namespace, "http://www.w3.org/1999/xhtml");
  const attr = journal.intern(AtomKind.Attribute, "canvas");
  const href = journal.intern(AtomKind.QualifiedAttribute, "http://www.w3.org/1999/xlink\0xlink:href");
  const css = journal.intern(AtomKind.CssProperty, "--x");

  journal.record(Opcode.CreateElement, 2, tag, html);
  journal.record(Opcode.CreateText, 3, "A\ud800💡", 0);
  journal.record(Opcode.CreateNode, 4, NodeType.Comment, "");
  journal.record(Opcode.CreateNode, 5, NodeType.DocumentType, "html");
  journal.record(Opcode.CreateNode, 6, NodeType.Document, NO_STRING);
  journal.record(Opcode.CreateNode, 7, NodeType.DocumentFragment, NO_STRING);
  journal.record(Opcode.Insert, 1, 5, 0);
  journal.record(Opcode.Insert, 1, 2, 0);
  journal.record(Opcode.Insert, 2, 3, 0);
  journal.record(Opcode.Insert, 2, 4, 3);
  journal.record(Opcode.SetAttr, 2, href, "#item");
  journal.record(Opcode.SetAttr, 2, attr, "a");
  journal.record(Opcode.RemoveAttr, 2, href, 0);
  journal.record(Opcode.SetText, 4, "comment", 0);
  journal.record(Opcode.SetStyle, 2, css, "");
  journal.record(Opcode.SetState, 2, ElementState.Invalid | ElementState.FocusVisible, 0xfff);
  journal.record(Opcode.SetValue, 2, "value", 0);
  journal.record(Opcode.SheetInsert, 2, 0, "canvas{color:red}");
  journal.record(Opcode.SheetDelete, 2, 0, 0);
  journal.record(Opcode.ScrollTo, 2, 0x3fc00000, 0xc0000000);
  journal.record(Opcode.Focus, 2, FocusFlags.Blur | FocusFlags.PreventScroll, 0);
  journal.record(Opcode.CanvasSize, 2, 320, 200);
  journal.record(Opcode.CanvasSubmit, 2, 9, 12);
  const font = '{"source":{"bufferId":11},"weight":"400"}';
  for (const action of [FontFaceAction.Define, FontFaceAction.Add, FontFaceAction.Remove]) {
    journal.record(Opcode.FontFace, 3, "Test", font, action);
  }
  const { NonPassiveWheel, NonPassiveTouchmove } = ListenerFlags;
  journal.record(Opcode.Listeners, 0, NonPassiveWheel | NonPassiveTouchmove, 0);
  journal.record(Opcode.Listeners, 2, NonPassiveTouchmove, 0);
  journal.record(Opcode.Remove, 2, 3, 0);
  journal.record(Opcode.Release, 3, 0, 0);
  assert.equal(journal.flush(), 35);

  assert.equal(journal.intern(AtomKind.Tag, "canvas"), tag);
  journal.record(Opcode.CreateElement, 0x01000003, tag, html);
  journal.record(Opcode.Insert, 2, 0x01000003, 0);
  assert.equal(journal.flush(), 2);
  assert.equal(journal.flush(), 0);

  assert.deepEqual(batches, [fixture("journal-v1"), fixture("journal-v1-next")]);
});

test("64 KiB rollover keeps local strings with their record and reuses the flush buffer", () => {
  const batches = [];
  let previousBuffer;
  const journal = new Journal((buffer, length) => {
    if (previousBuffer !== undefined) {
      assert.equal(buffer, previousBuffer);
    }
    previousBuffer = buffer;
    batches.push(Buffer.from(new Uint8Array(buffer, 0, length)));
    return new DataView(buffer).getUint32(0, true) / 16;
  });
  const tag = journal.intern(AtomKind.Tag, "div");
  journal.record(Opcode.CreateElement, 1, tag, 0);
  for (let index = 2; index < 4096; index++) {
    journal.record(Opcode.Listeners, 0, 0, 0);
  }
  assert.equal(batches.length, 1);
  journal.record(Opcode.SetValue, 1, "next\udfff", 0);
  assert.equal(journal.flush(), 1);
  assert.equal(batches.length, 2);
  assert.equal(batches[0].readUInt32LE(0), 65_536);
  assert.deepEqual(batches[1].subarray(40), Buffer.from("next\udfff", "utf16le"));
  assert.equal(batches[1].readUInt32LE(24), 0);
});

test("a failed acknowledgement preserves records and reentrant writes are rejected", () => {
  let attempts = 0;
  const journal = new Journal(() => {
    assert.throws(() => journal.record(Opcode.Listeners, 0, 0, 0), /while flushing/);
    assert.throws(() => journal.flush(), /reenter/);
    return attempts++;
  });
  journal.intern(AtomKind.Tag, "div");
  const batch = journal.snapshot();
  assert.throws(() => journal.flush(), /acknowledge/);
  assert.deepEqual(journal.snapshot(), batch);
  assert.equal(journal.flush(), 1);
  assert.deepEqual(Buffer.from(journal.snapshot()), Buffer.from([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
  ]));
});

test("the encoder validates operands and clears flags when it reuses the record header", () => {
  const headers = [];
  const journal = new Journal(buffer => {
    headers.push(new DataView(buffer).getUint32(16, true));
    return 1;
  });
  for (const value of [-1, 0x1_0000_0000, NaN, Infinity, 1.5]) {
    assert.throws(() => journal.record(Opcode.SetStyle, value, 1, "red", 1), /outside u32/);
  }
  for (const flags of [-1, 256, NaN, 1.5, "1", Symbol("flags")]) {
    assert.throws(() => journal.record(Opcode.SetStyle, 1, 1, "red", flags), /outside u8/);
  }
  assert.equal(journal.hasPendingRecords, false);
  journal.record(Opcode.SetStyle, 0x8000_0000, 1, "red", 1);
  assert.equal(journal.flush(), 1);
  journal.record(Opcode.ScrollTo, 1, 0xffff_ffff, 0x8000_0000);
  assert.equal(journal.flush(), 1);
  assert.deepEqual(headers, [0x0100 | Opcode.SetStyle, Opcode.ScrollTo]);
});
