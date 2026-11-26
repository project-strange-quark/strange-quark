// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { EventTarget, finishDocumentStartup, installDOM } from "@strange-quark/dom";

test("the realm global is an EventTarget and supports strict unqualified calls", () => {
  const context = createContext({ assert });
  const global = runInContext("globalThis", context);
  installDOM(global);
  assert(global instanceof EventTarget);
  runInContext(`
    "use strict";
    assert.equal(window, globalThis);
    assert.equal(window.parent, window);
    assert.equal(window.top, window);
    assert.equal(window.opener, null);
    assert.equal(document.readyState, "loading");
    assert.equal(document.getElementsByTagName("head")[0], document.head);
    let calls = 0;
    function listener(e) {
      assert.equal(this, window);
      assert.equal(e.currentTarget, window);
      calls++;
    }
    addEventListener("tick", listener);
    assert.equal(dispatchEvent(new Event("tick")), true);
    removeEventListener("tick", listener);
    assert.equal(dispatchEvent(new Event("tick")), true);
    assert.equal(calls, 1);
    const thrown = Error("test");
    addEventListener("error", event => {
      assert.equal(event.error, thrown);
      assert.equal(event.message, "test");
      assert.equal(event.isTrusted, true);
      event.preventDefault();
      calls++;
    }, { once: true });
    reportError(thrown);
    assert.equal(calls, 2);
    assert.throws(() => reportError());
  `, context);
});

test("document startup publishes readiness before the load event", () => {
  const window = {};
  const document = installDOM(window);
  const seen = [];
  document.addEventListener("readystatechange", () => seen.push(document.readyState));
  window.addEventListener("DOMContentLoaded", () => seen.push("dom loaded"));
  window.addEventListener("load", () => seen.push("load"));
  finishDocumentStartup(document);
  finishDocumentStartup(document);
  assert.equal(document.readyState, "complete");
  assert.deepEqual(seen, ["interactive", "dom loaded", "complete", "load"]);
});
