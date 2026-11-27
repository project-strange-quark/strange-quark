// SPDX-License-Identifier: MIT

// Runs the real DOMPurify against squark-dom. The HTML parser bindings are
// parse5 here (squark-js gives the realm html5ever), and the expected output
// is what DOMPurify returns in the pinned Chromium.

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import createDOMPurify from "dompurify";
import { parse, parseFragment } from "parse5";
import { installDocumentBindings, installDOM } from "@strange-quark/dom";
// Inputs and the output DOMPurify 3.5.0 gives for each in Chromium 159.
import payloads from "./fixtures/dompurify-payloads.json" with { type: "json" };

const quirks = { "no-quirks": 0, "limited-quirks": 1, quirks: 2 };

function parsed(node) {
  switch (node.nodeName) {
    case "#text": return { kind: "text", value: node.value };
    case "#comment": return { kind: "comment", value: node.data };
    case "#documentType": return { kind: "doctype", name: node.name, publicId: node.publicId ?? "", systemId: node.systemId ?? "" };
    default: return {
      kind: "element", namespace: node.namespaceURI, localName: node.tagName, prefix: null,
      attrs: node.attrs.map(({ name, value, namespace, prefix }) => ({ namespace: namespace ?? null, localName: name, prefix: prefix ?? null, value })),
      children: (node.content ?? node).childNodes.map(parsed),
    };
  }
}

/** A window whose HTML parsing goes through parse5, with scripting disabled as DOMParser's is. */
function window() {
  const context = createContext({});
  const global = runInContext("globalThis", context);
  const document = installDOM(global, undefined, { url: "https://app.test/" });
  installDocumentBindings(global, document, {
    parseFragment: html => parseFragment(html).childNodes.map(parsed),
    parseHtml(html) {
      const tree = parse(html, { scriptingEnabled: false });
      return { quirksMode: quirks[tree.mode], nodes: tree.childNodes.map(parsed) };
    },
    query() { return null; },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber() { return 0; },
    postTask(callback) { callback(); },
  });
  return global;
}

test("DOMPurify is supported and sanitises XSS payloads exactly as it does in Chromium", () => {
  const global = window();
  const DOMPurify = createDOMPurify(global);
  assert.equal(DOMPurify.isSupported, true);
  for (const [input, expected] of payloads) assert.equal(DOMPurify.sanitize(input), expected, input);
});

test("DOMPurify's hooks, removal log and DOM results match Chromium's", () => {
  const global = window();
  const DOMPurify = createDOMPurify(global);
  DOMPurify.addHook("uponSanitizeAttribute", (_node, event) => { if (event.attrName === "data-drop") event.keepAttr = false; });
  const fragment = DOMPurify.sanitize("<p data-keep=1 data-drop=2 onclick=x>a<script>b</script></p>", { RETURN_DOM_FRAGMENT: true });
  assert.deepEqual(DOMPurify.removed.map(item => item.element?.localName ?? item.attribute?.name), ["onclick", "data-drop", "script"]);
  assert(DOMPurify.removed[0].attribute instanceof global.Attr);
  assert(fragment instanceof global.DocumentFragment);
  assert.notEqual(fragment.ownerDocument, global.document, "the fragment stays in DOMPurify's inert document");
  assert.equal(fragment.ownerDocument.defaultView, null);
  assert.equal(fragment.firstChild.outerHTML, "<p data-keep=\"1\">a</p>");
  const body = DOMPurify.sanitize("<em>hi</em><img src=x onerror=1>", { RETURN_DOM: true });
  assert.equal(body.outerHTML, "<body><em>hi</em><img src=\"x\"></body>");
  global.document.body.append(...body.childNodes);
  assert.equal(global.document.body.innerHTML, "<em>hi</em><img src=\"x\">");
  assert.equal(global.document.body.lastChild.ownerDocument, global.document);
});
