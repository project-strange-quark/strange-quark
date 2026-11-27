// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument, installDocumentBindings, installDOM } from "@strange-quark/dom";
import { Journal, RECORD_BYTES } from "@strange-quark/dom/journal";

// Bundlers such as Vite preload a lazy chunk's CSS with a <link rel=stylesheet>
// and wait for its load or error event before importing the chunk. The engine
// double answers SheetLoaded (query kind 13) from a queue, in query order.
function setup(answers) {
  const global = {};
  const journal = new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const doc = createDocument(journal);
  installDOM(global, doc, { url: "https://app.test/index.html" });
  const root = doc.appendChild(doc.createElement("html"));
  root.append(doc.createElement("head"), doc.createElement("body"));
  journal.flush();
  const tasks = [];
  const queries = [];
  installDocumentBindings(global, doc, {
    query(kind, id) {
      queries.push(kind);
      assert.equal(typeof id, "number");
      return answers.shift();
    },
    postTask(callback) {
      tasks.push(callback);
    },
  });
  const run = () => {
    while (tasks.length) tasks.shift()();
  };
  return { doc, run, queries };
}

function link(doc, rel, href, seen) {
  const element = doc.createElement("link");
  element.rel = rel;
  element.href = href;
  for (const type of ["load", "error"]) element.addEventListener(type, () => seen.push([element.getAttribute("href"), type]));
  return element;
}

test("a stylesheet link inserted after load fires load or error from a task", () => {
  const seen = [];
  const { doc, run, queries } = setup([true, false]);
  doc.head.append(
    link(doc, "stylesheet", "/assets/chunk.css", seen),
    link(doc, "stylesheet", "/assets/missing.css", seen),
    link(doc, "icon", "/favicon.svg", seen),
    link(doc, "alternate stylesheet", "/alt.css", seen),
  );
  assert.deepEqual(seen, [], "events wait for a task");
  run();
  assert.deepEqual(seen, [["/assets/chunk.css", "load"], ["/assets/missing.css", "error"]]);
  assert.deepEqual(queries, [13, 13], "only stylesheet links ask the engine");
  assert.equal(doc.getElementsByTagName("link")[0].sheet.href, "https://app.test/assets/chunk.css");
});

test("only the latest href reports, and a removed link reports nothing", () => {
  const seen = [];
  const { doc, run, queries } = setup([true]);
  const element = link(doc, "stylesheet", "/a.css", seen);
  doc.head.appendChild(element);
  element.href = "/b.css";
  const removed = link(doc, "stylesheet", "/c.css", seen);
  doc.head.appendChild(removed);
  removed.remove();
  run();
  assert.deepEqual(seen, [["/b.css", "load"]]);
  assert.deepEqual(queries, [13]);
});
