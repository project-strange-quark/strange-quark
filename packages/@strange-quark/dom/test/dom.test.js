// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument } from "@strange-quark/dom";
import { AtomKind, Journal } from "@strange-quark/dom/journal";

test("createElement and appendChild retain JS identity and emit a journal", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const parent = document.createElement("DIV");
  const child = document.createElement("span");

  assert.equal(parent.localName, "div");
  assert.equal(child.ownerDocument, document);
  assert.equal(child.parentNode, null);
  assert.equal(parent.appendChild(child), child);
  assert.equal(parent.firstChild, child);
  assert.equal(child.parentNode, parent);

  const names = "divhttp://www.w3.org/1999/xhtmlspan";
  const header = [96, 3, names.length, 1];
  const offsets = [0, 3, names.length - 4, names.length];

  // Explicit opcode and atom values check the wire contract independently.
  const records = [
    [0x10, 1, 0, 1],
    [0x10, 2, 1, 4],
    [0x01, 1, 1, 2],
    [0x10, 3, 2, 1],
    [0x01, 2, 3, 2],
    [0x03, 1, 2, 0],
  ];
  const expected = Buffer.alloc(16 + header[0] + offsets.length * 4);
  [...header, ...records.flat(), ...offsets].forEach((word, index) => {
    expected.writeUInt32LE(word, index * 4);
  });
  assert.deepEqual(
    Buffer.from(journal.snapshot()),
    Buffer.concat([expected, Buffer.from(names, "utf16le")]),
  );
});

test("atoms key by kind and exact UTF-16 code units", () => {
  const journal = new Journal();
  const text = "\uD800💡";
  assert.equal(journal.intern(AtomKind.Tag, text), 1);
  assert.equal(journal.intern(AtomKind.Tag, text), 1);
  assert.equal(journal.intern(AtomKind.Attribute, text), 2);

  const bytes = Buffer.from(journal.snapshot());
  assert.equal(bytes.readUInt32LE(0), 32);
  assert.equal(bytes.readUInt32LE(4), 2);
  assert.equal(bytes.readUInt32LE(8), 6);
  assert.deepEqual(bytes.subarray(48, 60), Buffer.from([0, 0, 0, 0, 3, 0, 0, 0, 6, 0, 0, 0]));
  assert.deepEqual(bytes.subarray(60), Buffer.from(text + text, "utf16le"));
});

test("appendChild moves a child and refuses cycles without recording a write", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const first = document.createElement("div");
  const second = document.createElement("div");
  const child = document.createElement("span");
  first.appendChild(child);
  second.appendChild(child);

  assert.equal(first.firstChild, null);
  assert.equal(second.firstChild, child);
  assert.equal(child.parentNode, second);
  const snapshot = journal.snapshot();
  const bytes = Buffer.from(snapshot);
  const moveOffset = 16 + bytes.readUInt32LE(0) - 32;
  const move = Array.from({ length: 8 }, (_, index) => bytes.readUInt32LE(moveOffset + index * 4));
  assert.deepEqual(move, [0x04, 1, 3, 0, 0x03, 2, 3, 0]);
  assert.throws(() => child.appendChild(second), /ancestor/);
  assert.deepEqual(journal.snapshot(), snapshot);
});
