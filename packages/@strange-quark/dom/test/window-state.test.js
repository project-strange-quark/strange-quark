// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument, dispatchWindowState, installDOM } from "@strange-quark/dom";
import { Journal, RECORD_BYTES } from "@strange-quark/dom/journal";

function setup() {
  const global = {};
  const journal = new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const document = createDocument(journal);
  installDOM(global, document, { url: "https://app.test/index.html" });
  const root = document.appendChild(document.createElement("html"));
  root.append(document.createElement("head"), document.createElement("body"));
  return { global, document };
}

test("losing and regaining window focus blurs and refocuses like Chromium", () => {
  const { global, document } = setup();
  const input = document.body.appendChild(document.createElement("input"));
  input.focus();
  const seen = [];
  for (const type of ["focus", "blur", "focusin", "focusout"]) {
    input.addEventListener(type, () => seen.push(`input:${type}`));
    global.addEventListener(type, event => { if (event.target === global) seen.push(`window:${type}`); });
  }
  assert.equal(document.hasFocus(), true);
  dispatchWindowState(document, false, true);
  assert.equal(document.hasFocus(), false);
  assert.equal(document.activeElement, input, "activeElement survives a window blur");
  dispatchWindowState(document, false, true);
  dispatchWindowState(document, true, true);
  assert.equal(document.hasFocus(), true);
  assert.deepEqual(seen, [
    "input:blur", "input:focusout", "window:blur",
    "window:focus", "input:focus", "input:focusin",
  ]);
});

test("visibility changes fire visibilitychange and update visibilityState", () => {
  const { document } = setup();
  const seen = [];
  document.addEventListener("visibilitychange", () => seen.push(document.visibilityState));
  assert.equal(document.visibilityState, "visible");
  dispatchWindowState(document, true, false);
  assert.equal(document.hidden, true);
  dispatchWindowState(document, true, false);
  dispatchWindowState(document, true, true);
  assert.deepEqual(seen, ["hidden", "visible"]);
});
