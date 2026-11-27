// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { Document, installDocumentBindings, installDOM, XMLDocument } from "@strange-quark/dom";
import { Journal, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";

const HTML = "http://www.w3.org/1999/xhtml";
const SVG = "http://www.w3.org/2000/svg";

function opcodes(journal) {
  const batch = new DataView(journal.snapshot());
  const result = [];
  for (let offset = 16; offset < 16 + batch.getUint32(0, true); offset += RECORD_BYTES) result.push(batch.getUint8(offset));
  return result;
}

function realm() {
  const global = {};
  return { global, document: installDOM(global) };
}

test("document.implementation creates inert HTML and XML documents", () => {
  const { global, document } = realm();
  const { implementation } = document;
  assert.equal(document.implementation, implementation);
  assert(implementation instanceof global.DOMImplementation);
  assert.throws(() => new global.DOMImplementation(), TypeError);
  assert.equal(implementation.hasFeature(), true);

  const html = implementation.createHTMLDocument("Title & more");
  assert(html instanceof Document && !(html instanceof XMLDocument));
  assert.deepEqual([html.contentType, html.URL, html.compatMode, html.defaultView, html.title, html.doctype.name],
    ["text/html", "about:blank", "CSS1Compat", null, "Title & more", "html"]);
  assert.deepEqual([...html.documentElement.children].map(child => child.localName), ["head", "body"]);
  assert(html.body instanceof global.HTMLBodyElement);
  assert.equal(implementation.createHTMLDocument().head.firstChild, null, "no title element without a title");
  assert.notEqual(html.implementation, implementation);

  const doctype = implementation.createDocumentType("svg", "-//W3C//DTD SVG 1.1//EN", "svg11.dtd");
  assert.deepEqual([doctype.nodeType, doctype.name, doctype.publicId, doctype.systemId, doctype.ownerDocument],
    [10, "svg", "-//W3C//DTD SVG 1.1//EN", "svg11.dtd", document]);
  assert.throws(() => implementation.createDocumentType("a>b", "", ""), { name: "InvalidCharacterError" });
  const svg = implementation.createDocument(SVG, "svg:svg", doctype);
  assert(svg instanceof XMLDocument);
  assert.deepEqual([svg.contentType, svg.doctype, svg.documentElement.namespaceURI, svg.documentElement.prefix, svg.documentElement.tagName],
    ["image/svg+xml", doctype, SVG, "svg", "svg:svg"]);
  assert.equal(doctype.ownerDocument, svg);
  assert.equal(implementation.createDocument(HTML, "html").contentType, "application/xhtml+xml");
  const empty = implementation.createDocument(null, null);
  assert.deepEqual([empty.contentType, empty.documentElement, empty.childNodes.length], ["application/xml", null, 0]);
  assert.throws(() => implementation.createDocument(null, "a:b"), { name: "NamespaceError" });
  assert.throws(() => implementation.createDocument(null, "a b"), { name: "InvalidCharacterError" });
});

test("markup parses into inert documents without touching their journal and moves into the window document", () => {
  const { global, document } = realm();
  const contexts = [];
  installDocumentBindings(global, document, {
    parseFragment(html, contextId) {
      contexts.push(contextId);
      return [{ kind: "element", namespace: HTML, localName: "b", prefix: null, attrs: [{ namespace: null, localName: "id", prefix: null, value: html }], children: [] }];
    },
    query() { return null; },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber() { return 0; },
    postTask(callback) { callback(); },
  });
  const inert = document.implementation.createHTMLDocument();
  inert.body.innerHTML = "one";
  assert.equal(inert.body.innerHTML, "<b id=\"one\"></b>");
  assert.equal(contexts.length, 1);
  assert.notEqual(contexts[0], 0, "the engine parses against a stand-in context it mirrors");
  const template = inert.createElement("template");
  template.innerHTML = "two";
  assert.equal(template.content.firstChild.id, "two");
  assert.equal(template.content.ownerDocument, inert);
  const xml = document.implementation.createDocument(HTML, "html");
  xml.documentElement.innerHTML = "three";
  assert.equal(xml.documentElement.firstChild.ownerDocument, xml);

  const imported = document.importNode(inert.body.firstChild, true);
  assert.equal(imported.ownerDocument, document);
  const adopted = document.adoptNode(template);
  assert.equal(adopted.ownerDocument, document);
  assert.equal(adopted.content.firstChild.id, "two");
  document.body.append(imported, adopted);
  assert.equal(document.body.lastChild.content.firstChild.ownerDocument, document);
});

test("inert documents keep their nodes out of the window journal until adopted", () => {
  const journal = new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const document = installDOM({}, new Document(journal, 0, 0, undefined, true));
  journal.flush();
  const inert = document.implementation.createHTMLDocument("x");
  inert.body.append(inert.createElement("p"));
  assert.deepEqual(opcodes(journal), []);
  document.appendChild(document.adoptNode(inert.documentElement));
  assert(opcodes(journal).includes(Opcode.CreateElement));
});

test("NodeIterator walks in document order with whatToShow and a filter", () => {
  const { global, document } = realm();
  const root = document.createElement("div");
  const a = root.appendChild(document.createElement("a"));
  const text = a.appendChild(document.createTextNode("t"));
  const b = root.appendChild(document.createElement("b"));
  const comment = b.appendChild(document.createComment("c"));
  const c = root.appendChild(document.createElement("c"));
  const all = document.createNodeIterator(root);
  assert(all instanceof global.NodeIterator);
  assert.throws(() => new global.NodeIterator(), TypeError);
  assert.deepEqual([all.root, all.referenceNode, all.pointerBeforeReferenceNode, all.whatToShow, all.filter], [root, root, true, 0xffffffff, null]);
  const forward = [];
  for (let node = all.nextNode(); node !== null; node = all.nextNode()) forward.push(node);
  assert.deepEqual(forward, [root, a, text, b, comment, c]);
  assert.deepEqual([all.referenceNode, all.pointerBeforeReferenceNode], [c, false]);
  assert.equal(all.previousNode(), c);
  assert.equal(all.previousNode(), comment);
  all.detach();

  const seen = [];
  const elements = document.createNodeIterator(root, global.NodeFilter.SHOW_ELEMENT, node => {
    seen.push(node.localName);
    return node === b ? global.NodeFilter.FILTER_REJECT : global.NodeFilter.FILTER_ACCEPT;
  });
  assert.deepEqual([elements.nextNode(), elements.nextNode(), elements.nextNode()], [root, a, c], "REJECT skips only the node itself");
  assert.deepEqual(seen, ["div", "a", "b", "c"]);
  const object = { acceptNode(node) { assert.equal(this, object); return node.nodeType === 3 ? 1 : 3; } };
  assert.equal(document.createNodeIterator(root, 0xffffffff, object).nextNode(), text);
  assert.throws(() => document.createNodeIterator({}), TypeError);
});

test("NodeIterator filters cannot re-enter, rethrow their errors and see removals retarget the reference", () => {
  const { document } = realm();
  const root = document.createElement("div");
  const [a, b, c] = ["a", "b", "c"].map(name => root.appendChild(document.createElement(name)));
  const nested = document.createNodeIterator(root, 1, () => { nested.nextNode(); return 1; });
  assert.throws(() => nested.nextNode(), { name: "InvalidStateError" });
  const thrown = new Error("filter");
  let fail = true;
  const throwing = document.createNodeIterator(root, 1, () => { if (fail) throw thrown; return 1; });
  assert.throws(() => throwing.nextNode(), error => error === thrown);
  fail = false;
  assert.equal(throwing.nextNode(), root, "the filter runs again after an exception");

  const iterator = document.createNodeIterator(root);
  iterator.nextNode();
  iterator.nextNode();
  iterator.nextNode();
  assert.deepEqual([iterator.referenceNode, iterator.pointerBeforeReferenceNode], [b, false]);
  b.remove();
  assert.deepEqual([iterator.referenceNode, iterator.pointerBeforeReferenceNode], [a, false]);
  assert.equal(iterator.nextNode(), c);
  assert.equal(iterator.previousNode(), c);
  c.remove();
  assert.deepEqual([iterator.referenceNode, iterator.pointerBeforeReferenceNode], [a, false]);
  a.remove();
  assert.deepEqual([iterator.referenceNode, iterator.pointerBeforeReferenceNode], [root, false]);
  root.remove();
  assert.equal(iterator.referenceNode, root, "removing the root leaves the iterator alone");

  const other = document.implementation.createHTMLDocument();
  const elsewhere = document.createNodeIterator(document.body);
  elsewhere.nextNode();
  other.body.remove();
  assert.equal(elsewhere.referenceNode, document.body);
});
