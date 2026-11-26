// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { bootstrapDocument } from "@strange-quark/dom";
import { AtomKind, Journal, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";
import { packedSheetOwner } from "../dist/document.js";

function batch(records, strings) {
  const bytes = new ArrayBuffer(records.length * RECORD_BYTES);
  const view = new DataView(bytes);
  records.forEach(([opcode, a, b, c], index) => {
    const offset = index * RECORD_BYTES;
    view.setUint8(offset, opcode);
    view.setUint32(offset + 4, a, true);
    view.setUint32(offset + 8, b, true);
    view.setUint32(offset + 12, c, true);
  });
  return [bytes, strings];
}

test("bootstrap preserves the static tree and starts a silent journal with imported atoms", () => {
  const journal = new Journal(() => {
    throw new Error("Unexpected flush");
  });
  const batches = [
    batch([
      [Opcode.DefineAtom, 10, 0, AtomKind.Tag],
      [Opcode.DefineAtom, 11, 1, AtomKind.Namespace],
      [Opcode.CreateElement, 42, 10, 11],
      [Opcode.Insert, 3, 42, 0],
    ], ["style", "http://www.w3.org/1999/xhtml"]),
    batch([
      [Opcode.CreateText, 9, 0, 0],
      [Opcode.Insert, 42, 9, 0],
      [Opcode.SheetInsert, 42, 0, 0],
    ], ["body { color: red }"]),
  ];
  const document = bootstrapDocument(journal, 3, 2, batches);
  const style = document.documentElement;
  assert.equal(document.compatMode, "BackCompat");
  assert.equal(style.ownerDocument, document);
  assert.equal(style.parentNode, document);
  assert.equal(style.textContent, "body { color: red }");
  assert.equal(packedSheetOwner(document, 42), style);
  assert.equal(journal.flush(), 0);
  assert.throws(() => bootstrapDocument(journal, 3, 2, batches), /fresh journal/);

  style.appendChild(document.createElement("STYLE"));
  const bytes = new DataView(journal.snapshot());
  const word = (offset) => bytes.getUint32(offset, true);
  assert.equal(word(0), 32);
  assert.equal(word(4), 0);
  assert.deepEqual([word(20), word(24), word(28)], [43, 10, 11]);
  assert.deepEqual([word(36), word(40), word(44)], [42, 43, 0]);
});

test("a failed replay leaves the journal available for a fresh bootstrap", () => {
  const journal = new Journal();
  const invalid = batch([
    [Opcode.DefineAtom, 10, 0, AtomKind.Tag],
    [Opcode.SheetInsert, 42, 0, 0],
  ], ["div"]);
  assert.throws(() => bootstrapDocument(journal, 3, 0, [invalid]), /owning style or link/);
  assert.equal(journal.flush(), 0);

  const document = bootstrapDocument(journal, 3, 1, []);
  assert.equal(document.compatMode, "CSS1Compat");
  assert.equal(document.documentElement, null);
  document.createElement("div");
  const bytes = new DataView(journal.snapshot());
  assert.equal(bytes.getUint8(16), Opcode.DefineAtom);
  assert.equal(bytes.getUint32(20, true), 1);
  assert.equal(bytes.getUint32(52, true), 4);
});
