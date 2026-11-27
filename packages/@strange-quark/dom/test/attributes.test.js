// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument, installDOM, MutationObserver } from "@strange-quark/dom";
import { Journal, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";

const XLINK = "http://www.w3.org/1999/xlink";

test("attributes is a live NamedNodeMap of Attr nodes that keep their identity", () => {
  const global = {};
  const document = installDOM(global);
  const element = document.createElement("div");
  element.setAttribute("id", "a");
  element.setAttributeNS(XLINK, "xlink:href", "#x");
  const { attributes } = element;
  assert(attributes instanceof global.NamedNodeMap);
  assert.equal(element.attributes, attributes);
  assert.equal(attributes.length, 2);
  const id = attributes[0];
  assert(id instanceof global.Attr && id instanceof global.Node);
  assert.deepEqual([id.nodeType, id.name, id.localName, id.value, id.nodeName, id.nodeValue, id.textContent, id.specified],
    [2, "id", "id", "a", "id", "a", "a", true]);
  assert.equal(id.ownerElement, element);
  assert.equal(id.ownerDocument, document);
  assert.equal(attributes.id, id);
  assert.equal(attributes.getNamedItem("ID"), id);
  assert.equal(element.getAttributeNode("id"), id);
  assert.equal(attributes.item(1), element.getAttributeNodeNS(XLINK, "href"));
  assert.deepEqual([attributes[1].prefix, attributes[1].namespaceURI, attributes[1].name], ["xlink", XLINK, "xlink:href"]);
  assert.deepEqual([...attributes].map(attr => attr.name), ["id", "xlink:href"]);
  assert.deepEqual(Object.keys(attributes), ["0", "1"]);
  assert(Object.getOwnPropertyNames(attributes).includes("xlink:href"));
  assert.equal(attributes[2], undefined);

  element.setAttribute("id", "b");
  assert.equal(element.getAttributeNode("id"), id, "a value change keeps the node");
  assert.equal(id.value, "b");
  id.value = "c";
  assert.equal(element.id, "c");
  element.removeAttribute("id");
  assert.equal(id.ownerElement, null);
  assert.equal(id.value, "c", "a removed attribute's node keeps its last value");
  assert.equal(attributes.length, 1);
  element.setAttribute("id", "d");
  assert.notEqual(element.getAttributeNode("id"), id);
});

test("setAttributeNode, removeAttributeNode and NamedNodeMap edits follow the DOM standard", () => {
  const global = {};
  const document = installDOM(global);
  const element = document.createElement("div");
  const other = document.createElement("div");
  const records = [];
  new MutationObserver(list => records.push(...list)).observe(element, { attributes: true, attributeOldValue: true });

  const title = document.createAttribute("TITLE");
  assert.deepEqual([title.name, title.value, title.ownerElement], ["title", "", null]);
  title.value = "one";
  assert.equal(element.setAttributeNode(title), null);
  assert.equal(element.getAttribute("title"), "one");
  assert.equal(title.ownerElement, element);
  assert.equal(element.setAttributeNode(title), title);
  assert.throws(() => other.setAttributeNode(title), { name: "InUseAttributeError", code: 10 });
  const replacement = document.createAttribute("title");
  replacement.value = "two";
  assert.equal(element.attributes.setNamedItem(replacement), title);
  assert.deepEqual([title.ownerElement, title.value, element.getAttribute("title")], [null, "one", "two"]);
  assert.throws(() => element.removeAttributeNode(title), { name: "NotFoundError" });
  assert.equal(element.removeAttributeNode(replacement), replacement);
  assert.equal(element.hasAttribute("title"), false);
  assert.throws(() => element.attributes.removeNamedItem("title"), { name: "NotFoundError" });
  element.setAttribute("lang", "en");
  assert.equal(element.attributes.removeNamedItem("lang").value, "en");

  const href = document.createAttributeNS(XLINK, "xlink:href");
  assert.deepEqual([href.prefix, href.localName, href.namespaceURI], ["xlink", "href", XLINK]);
  assert.throws(() => document.createAttributeNS(null, "a:b"), { name: "NamespaceError" });
  assert.throws(() => document.createAttribute("a b"), { name: "InvalidCharacterError" });
  element.setAttributeNodeNS(href);
  assert.equal(element.attributes.removeNamedItemNS(XLINK, "href"), href);
  assert.throws(() => element.appendChild(document.createAttribute("x")), { name: "HierarchyRequestError" });
  assert.equal(document.createAttribute("x").cloneNode().name, "x");
  const moved = document.createAttribute("y");
  element.setAttributeNode(moved);
  const otherDocument = new global.Document();
  assert.equal(otherDocument.adoptNode(moved), moved);
  assert.deepEqual([moved.ownerElement, moved.ownerDocument, element.hasAttribute("y")], [null, otherDocument, false]);

  return Promise.resolve().then(() => {
    assert.deepEqual(records.map(record => [record.attributeName, record.oldValue]), [
      ["title", null], ["title", "one"], ["title", "two"], ["lang", null], ["lang", "en"], ["href", null], ["href", ""],
      ["y", null], ["y", ""],
    ]);
  });
});

test("replacing an attribute by an Attr with another prefix keeps the engine's attribute set in step", () => {
  const journal = new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const document = createDocument(journal);
  const element = document.createElement("div");
  element.setAttributeNS(XLINK, "a:href", "1");
  journal.flush();
  const attr = document.createAttributeNS(XLINK, "b:href");
  attr.value = "2";
  element.setAttributeNode(attr);
  const batch = new DataView(journal.snapshot());
  const opcodes = [];
  for (let offset = 16; offset < 16 + batch.getUint32(0, true); offset += RECORD_BYTES) opcodes.push(batch.getUint8(offset));
  assert.deepEqual(opcodes.filter(opcode => opcode === Opcode.RemoveAttr || opcode === Opcode.SetAttr), [Opcode.RemoveAttr, Opcode.SetAttr]);
  assert.equal(element.attributes.length, 1);
  assert.equal(element.attributes[0].name, "b:href");
});
