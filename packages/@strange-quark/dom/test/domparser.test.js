// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import {
  createDocument, Document, DOMParser, installDocumentBindings, installDOM, XMLDocument,
} from "@strange-quark/dom";
import { Journal, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";

function records(journal) {
  const batch = new DataView(journal.snapshot());
  const end = 16 + batch.getUint32(0, true);
  const result = [];
  for (let offset = 16; offset < end; offset += RECORD_BYTES) {
    result.push([batch.getUint8(offset), ...[4, 8, 12].map((step) => batch.getUint32(offset + step, true))]);
  }
  return result;
}

function setupRealm(overrides = {}) {
  const global = {};
  const journal = new Journal((buffer) => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const doc = createDocument(journal);
  installDOM(global, doc, { url: "https://example.test/app" });
  const root = doc.appendChild(doc.createElement("html"));
  root.append(doc.createElement("head"), doc.createElement("body"));
  journal.flush();

  const bindings = {
    parseFragment() { return []; },
    parseHtml() {
      return {
        quirksMode: 0,
        nodes: [
          { kind: "doctype", name: "html", publicId: "", systemId: "" },
          {
            kind: "element",
            namespace: "http://www.w3.org/1999/xhtml",
            localName: "html",
            prefix: null,
            attrs: [],
            children: [
              { kind: "element", namespace: "http://www.w3.org/1999/xhtml", localName: "head", prefix: null, attrs: [], children: [] },
              {
                kind: "element",
                namespace: "http://www.w3.org/1999/xhtml",
                localName: "body",
                prefix: null,
                attrs: [],
                children: [
                  {
                    kind: "element",
                    namespace: "http://www.w3.org/1999/xhtml",
                    localName: "div",
                    prefix: null,
                    attrs: [{ namespace: null, localName: "id", prefix: null, value: "parsed" }],
                    children: [{ kind: "text", value: "hello" }],
                  },
                ],
              },
            ],
          },
        ],
      };
    },
    parseXml(text) {
      if (text.includes("BAD")) {
        return { error: "Opening and ending tag mismatch: rect and svg", line: 1, column: 20 };
      }
      return [
        {
          kind: "element",
          namespace: "http://www.w3.org/2000/svg",
          localName: "svg",
          prefix: null,
          attrs: [
            { namespace: "http://www.w3.org/2000/xmlns/", localName: "xmlns", prefix: null, value: "http://www.w3.org/2000/svg" },
            { namespace: null, localName: "viewBox", prefix: null, value: "0 0 100 50" },
          ],
          children: [
            {
              kind: "element",
              namespace: "http://www.w3.org/2000/svg",
              localName: "rect",
              prefix: null,
              attrs: [{ namespace: null, localName: "width", prefix: null, value: "10" }],
              children: [],
            },
          ],
        },
      ];
    },
    query() { return null; },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber() { return 0; },
    postTask(fn) { fn(); },
    ...overrides,
  };
  installDocumentBindings(global, doc, bindings);
  return { global, doc, journal };
}

test("DOMParser validates MIME types and keeps parsed documents inert and unjournaled", () => {
  const { global, doc, journal } = setupRealm();
  const parser = new global.DOMParser();
  assert(parser instanceof DOMParser);

  assert.throws(() => parser.parseFromString("<div/>"), TypeError);
  assert.throws(() => parser.parseFromString("<div/>", "text/plain"), TypeError);
  assert.throws(() => parser.parseFromString("<div/>", "TEXT/HTML"), TypeError);

  const htmlDoc = parser.parseFromString("<!DOCTYPE html><div id='parsed'>hello</div>", "text/html");
  assert(htmlDoc instanceof Document);
  assert.equal(htmlDoc.defaultView, null);
  assert.equal(htmlDoc.contentType, "text/html");
  assert.equal(htmlDoc.compatMode, "CSS1Compat");
  assert.equal(htmlDoc.doctype?.name, "html");
  assert.equal(htmlDoc.URL, "https://example.test/app");

  for (const mime of ["text/xml", "application/xml", "application/xhtml+xml", "image/svg+xml"]) {
    const xmlDoc = parser.parseFromString("<svg/>", mime);
    assert(xmlDoc instanceof XMLDocument);
    assert(xmlDoc instanceof Document);
    assert.equal(xmlDoc.defaultView, null);
    assert.equal(xmlDoc.contentType, mime);
    assert.equal(xmlDoc.compatMode, "CSS1Compat");
  }

  const svgDoc = parser.parseFromString("<svg/>", "image/svg+xml");
  const svg = svgDoc.documentElement;
  assert.equal(svg.namespaceURI, "http://www.w3.org/2000/svg");
  assert.equal(svg.localName, "svg");
  assert.equal(svg.nodeName, "svg");
  assert.equal(svg.tagName, "svg");
  assert.equal(svg.getAttribute("viewBox"), "0 0 100 50");
  assert.equal(svg.getAttribute("viewbox"), null);
  assert.equal(svg.getAttribute("xmlns"), "http://www.w3.org/2000/svg");
  svg.setAttribute("width", "100");
  svg.setAttribute("height", "50");
  assert.equal(
    svg.outerHTML,
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50" width="100" height="50"><rect width="10"/></svg>',
  );

  const badDoc = parser.parseFromString("<svg><BAD></svg>", "image/svg+xml");
  const errorEl = badDoc.getElementsByTagName("parsererror")[0];
  assert(errorEl !== undefined);
  assert.match(errorEl.textContent, /line 1 at column 20: Opening and ending tag mismatch/);

  // Nothing from parsing or mutating the inert documents was written to the live journal.
  assert.equal(journal.hasPendingRecords, false);
  assert.equal(records(journal).length, 0);

  // Importing and adopting nodes into the live document journals them on transfer.
  const imported = doc.importNode(htmlDoc.getElementById("parsed"), true);
  assert.equal(imported.ownerDocument, doc);
  assert.equal(imported.textContent, "hello");
  assert.equal(journal.hasPendingRecords, true);
  const importOps = records(journal).map(([op]) => op);
  assert(importOps.includes(Opcode.CreateElement));
  assert(importOps.includes(Opcode.CreateText));
  assert(importOps.includes(Opcode.SetAttr));
  journal.flush();

  const parsedDiv = htmlDoc.getElementById("parsed");
  const adopted = doc.adoptNode(parsedDiv);
  assert.equal(adopted, parsedDiv);
  assert.equal(adopted.ownerDocument, doc);
  assert.equal(htmlDoc.getElementById("parsed"), null);
  doc.body.appendChild(adopted);
  const adoptOps = records(journal).map(([op]) => op);
  assert(adoptOps.includes(Opcode.CreateElement));
  assert(adoptOps.includes(Opcode.CreateText));
  assert(adoptOps.includes(Opcode.Insert));
});

test("document.execCommand and queryCommand helpers return false", () => {
  const { doc } = setupRealm();
  assert.equal(doc.execCommand.length, 1);
  assert.equal(doc.queryCommandSupported.length, 1);
  assert.equal(doc.queryCommandEnabled.length, 1);
  assert.throws(() => doc.execCommand(), TypeError);
  assert.throws(() => doc.queryCommandSupported(), TypeError);
  assert.throws(() => doc.queryCommandEnabled(), TypeError);

  for (const command of ["copy", "cut", "paste", "bold", "selectAll", "unknownCommand"]) {
    assert.equal(doc.execCommand(command), false);
    assert.equal(doc.queryCommandSupported(command), false);
    assert.equal(doc.queryCommandEnabled(command), false);
  }
});
