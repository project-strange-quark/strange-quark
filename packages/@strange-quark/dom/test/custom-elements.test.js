// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { installDocumentBindings, installDOM } from "@strange-quark/dom";

const HTML = "http://www.w3.org/1999/xhtml";

/** A window realm whose scripts define classes against its own globals. */
function realm() {
  const context = createContext({});
  const global = runInContext("globalThis", context);
  const document = installDOM(global);
  const errors = [];
  global.addEventListener("error", event => {
    errors.push(event.error);
    event.preventDefault();
  });
  return { global, document, errors, run: source => runInContext(source, context) };
}

/** Copies entries the realm logged into arrays of this realm for deepEqual. */
const plain = log => Array.from(log, entry => Array.isArray(entry) ? Array.from(entry) : entry);

const loggingClass = `(log, observed = []) => class extends HTMLElement {
  static get observedAttributes() { return observed; }
  constructor() { super(); log.push(["constructed", this.isConnected]); }
  connectedCallback() { log.push(["connected"]); }
  disconnectedCallback() { log.push(["disconnected"]); }
  adoptedCallback(from, to) { log.push(["adopted", from, to]); }
  attributeChangedCallback(...args) { log.push(["attribute", ...args]); }
}`;

test("customElements validates definitions like Chromium and rejects customized built-ins like Safari", () => {
  const { global, run } = realm();
  const { customElements } = global;
  assert.equal(Object.prototype.toString.call(customElements), "[object CustomElementRegistry]");
  assert.throws(() => new global.CustomElementRegistry(), TypeError);
  assert.throws(() => customElements.define("x-arrow", () => {}), TypeError);
  assert.throws(() => customElements.define("x-number", 1), TypeError);
  for (const name of ["", "abc", "a-Bc", "1-a", "-a", "font-face", "annotation-xml", "missing-glyph"]) {
    assert.throws(() => customElements.define(name, run("(class extends HTMLElement {})")), { name: "SyntaxError" }, name);
  }
  const First = run("(class extends HTMLElement {})");
  customElements.define("x-first", First);
  assert.throws(() => customElements.define("x-first", run("(class extends HTMLElement {})")), { name: "NotSupportedError" });
  assert.throws(() => customElements.define("x-again", First), { name: "NotSupportedError" });
  assert.throws(() => customElements.define("x-button", run("(class extends HTMLElement {})"), { extends: "button" }),
    { name: "NotSupportedError" });
  assert.equal(customElements.get("x-first"), First);
  assert.equal(customElements.get("x-button"), undefined);
  assert.equal(customElements.getName(First), "x-first");
  assert.equal(customElements.getName(run("(class {})")), null);
  customElements.define("x-unicode-é\u{1f600}", run("(class extends HTMLElement {})"));

  const reads = [];
  const prototype = new Proxy({}, { get(target, key) { reads.push(key); return undefined; } });
  const Plain = function () {};
  Plain.prototype = prototype;
  customElements.define("x-plain", Plain);
  assert.deepEqual(reads, ["connectedCallback", "disconnectedCallback", "adoptedCallback", "attributeChangedCallback"]);
  const NotCallable = run("(class extends HTMLElement {})");
  NotCallable.prototype.connectedCallback = 1;
  assert.throws(() => customElements.define("x-not-callable", NotCallable), TypeError);
  assert.equal(customElements.get("x-not-callable"), undefined);
});

test("whenDefined resolves with the constructor and rejects invalid names", async () => {
  const { global, run } = realm();
  const { customElements } = global;
  const pending = customElements.whenDefined("x-later");
  assert.equal(customElements.whenDefined("x-later"), pending);
  await assert.rejects(customElements.whenDefined("invalid"), { name: "SyntaxError" });
  const Later = run("(class extends HTMLElement {})");
  customElements.define("x-later", Later);
  assert.equal(await pending, Later);
  assert.equal(await customElements.whenDefined("x-later"), Later);
});

test("HTML element constructors are illegal unless a definition names NewTarget", () => {
  const { global, document, run } = realm();
  assert.throws(() => new global.HTMLElement(), { name: "TypeError", message: "Illegal constructor" });
  assert.throws(() => run("new (class extends HTMLElement {})()"), TypeError);
  const Element = run("(class extends HTMLElement {})");
  global.customElements.define("x-direct", Element);
  const element = new Element();
  assert(element instanceof Element);
  assert(element instanceof global.HTMLElement);
  assert.equal(element.localName, "x-direct");
  assert.equal(element.namespaceURI, HTML);
  assert.equal(element.ownerDocument, document);
  assert.equal(element.tagName, "X-DIRECT");
  assert.equal(element.isConnected, false);
  document.body.append(element);
  assert.equal(document.body.lastChild, element);
});

test("createElement runs the constructor synchronously and reports a failed construction", () => {
  const { global, document, errors, run } = realm();
  const log = [];
  global.customElements.define("x-sync", run(loggingClass)(log));
  const element = document.createElement("X-SYNC");
  assert.deepEqual(plain(log), [["constructed", false]]);
  assert.equal(element.constructor, global.customElements.get("x-sync"));
  assert.equal(document.createElementNS(HTML, "x-sync").constructor, global.customElements.get("x-sync"));
  assert.equal(document.createElementNS("urn:other", "x-sync").constructor, global.Element);

  global.customElements.define("x-attributes", run("(class extends HTMLElement { constructor() { super(); this.setAttribute('id', 'x'); } })"));
  const failed = document.createElement("x-attributes");
  assert(failed instanceof global.HTMLUnknownElement);
  assert.equal(failed.localName, "x-attributes");
  assert.equal(errors.at(-1).name, "NotSupportedError");

  global.customElements.define("x-object", run("(class extends HTMLElement { constructor() { return {}; } })"));
  assert(document.createElement("x-object") instanceof global.HTMLUnknownElement);
  assert(errors.at(-1) instanceof TypeError);

  global.customElements.define("x-throws", run("(class extends HTMLElement { constructor() { super(); throw new RangeError('boom'); } })"));
  assert(document.createElement("x-throws") instanceof global.HTMLUnknownElement);
  assert.equal(errors.at(-1).message, "boom");
  document.body.append(failed);
  assert(!(failed instanceof global.customElements.get("x-attributes")));
});

test("define upgrades connected candidates in tree order and later insertions upgrade the rest", () => {
  const { global, document, run } = realm();
  const log = [];
  const first = document.createElement("x-upgrade");
  first.setAttribute("a", "1");
  first.setAttribute("b", "2");
  const nested = document.createElement("x-upgrade");
  first.append(nested);
  const detached = document.createElement("x-upgrade");
  document.body.append(first);
  assert.equal(first.constructor, global.HTMLElement);

  const Upgrade = run(loggingClass)(log, ["a"]);
  global.customElements.define("x-upgrade", Upgrade);
  assert(first instanceof Upgrade && nested instanceof Upgrade);
  assert(!(detached instanceof Upgrade));
  assert.deepEqual(plain(log), [
    ["constructed", true], ["attribute", "a", null, "1", null], ["connected"],
    ["constructed", true], ["connected"],
  ]);
  log.length = 0;
  document.body.append(detached);
  assert(detached instanceof Upgrade);
  assert.deepEqual(plain(log), [["constructed", true], ["connected"]]);

  log.length = 0;
  const loose = document.createElement("div");
  loose.append(document.createElement("x-upgrade"));
  log.length = 0;
  const unknown = document.createElementNS(HTML, "x-upgrade");
  log.length = 0;
  global.customElements.upgrade(loose);
  assert(loose.firstChild instanceof Upgrade);
  assert(unknown instanceof Upgrade);
});

test("lifecycle callbacks run before the triggering DOM call returns", () => {
  const { global, document, run } = realm();
  const log = [];
  const Lifecycle = run(loggingClass)(log, ["title", "href"]);
  global.customElements.define("x-life", Lifecycle);
  const element = document.createElement("x-life");
  const host = document.createElement("div");
  log.length = 0;

  host.appendChild(element);
  assert.deepEqual(plain(log), []);
  document.body.appendChild(host);
  log.push(["returned"]);
  element.setAttribute("title", "one");
  element.title = "two";
  element.setAttribute("other", "ignored");
  element.removeAttribute("title");
  element.setAttributeNS("http://www.w3.org/1999/xlink", "xlink:href", "#x");
  element.toggleAttribute("title");
  document.body.removeChild(host);
  log.push(["removed"]);
  host.remove();
  assert.deepEqual(plain(log), [
    ["connected"], ["returned"],
    ["attribute", "title", null, "one", null],
    ["attribute", "title", "one", "two", null],
    ["attribute", "title", "two", null, null],
    ["attribute", "href", null, "#x", "http://www.w3.org/1999/xlink"],
    ["attribute", "title", null, "", null],
    ["disconnected"], ["removed"],
  ]);

  log.length = 0;
  element.style.color = "red";
  element.className = "c";
  element.classList.add("d");
  document.body.append(element);
  document.body.insertBefore(element, null);
  document.body.textContent = "";
  assert.deepEqual(plain(log), [["connected"], ["disconnected"], ["connected"], ["disconnected"]]);
});

test("adoptedCallback follows the element into another document before it connects", () => {
  const { global, document, run } = realm();
  const log = [];
  global.customElements.define("x-adopt", run(loggingClass)(log));
  const element = document.createElement("x-adopt");
  document.body.append(element);
  const other = new global.Document();
  const root = other.appendChild(other.createElement("root"));
  log.length = 0;
  other.adoptNode(element);
  assert.deepEqual(plain(log), [["disconnected"], ["adopted", document, other]]);
  log.length = 0;
  const fragment = other.createDocumentFragment();
  fragment.append(element);
  document.body.append(element);
  assert.deepEqual(plain(log), [["adopted", other, document], ["connected"]]);
  log.length = 0;
  root.append(element);
  assert.deepEqual(plain(log), [["disconnected"], ["adopted", document, other], ["connected"]]);
});

test("reactions queued by a callback run when that callback's own DOM call returns", () => {
  const { global, document, run } = realm();
  const log = [];
  const Outer = run(`(log) => class extends HTMLElement {
    connectedCallback() {
      log.push("outer connected");
      this.appendChild(document.createElement("x-inner"));
      log.push("outer appended");
    }
  }`)(log);
  const Inner = run(`(log) => class extends HTMLElement {
    connectedCallback() { log.push("inner connected"); }
  }`)(log);
  global.customElements.define("x-outer", Outer);
  global.customElements.define("x-inner", Inner);
  document.body.append(document.createElement("x-outer"));
  log.push("done");
  assert.deepEqual(plain(log), ["outer connected", "inner connected", "outer appended", "done"]);
});

test("Monaco's connection-observer registers once and notifies on connection changes", () => {
  const { global, document, run } = realm();
  run(`
    if (!customElements.get("connection-observer")) {
      customElements.define("connection-observer", class extends HTMLElement {
        connectedCallback() { this.callback?.(true); }
        disconnectedCallback() { this.callback?.(false); }
      });
    }
  `);
  const seen = [];
  const observer = document.createElement("connection-observer");
  observer.callback = connected => seen.push(connected);
  document.body.append(observer);
  observer.remove();
  assert.deepEqual(seen, [true, false]);
  assert.equal(global.customElements.get("connection-observer").name, "");
});

test("clones upgrade at the end of the call, template contents stay inert, and parsed markup upgrades on connection", () => {
  const { global, document, run } = realm();
  installDocumentBindings(global, document, {
    parseFragment: html => [...html.matchAll(/<([a-z-]+)>/g)].map(([, localName]) => ({
      kind: "element", namespace: HTML, localName, prefix: null, attrs: [], children: [],
    })),
    query() { return null; },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber() { return 0; },
    postTask(callback) { callback(); },
  });
  const log = [];
  const Clone = run(loggingClass)(log);
  global.customElements.define("x-clone", Clone);
  const source = document.createElement("x-clone");
  log.length = 0;
  const clone = source.cloneNode();
  assert(clone instanceof Clone);
  assert.deepEqual(plain(log), [["constructed", false]]);

  const template = document.createElement("template");
  template.content.append(document.createElement("x-clone"));
  log.length = 0;
  assert(!(template.content.cloneNode(true).firstChild instanceof Clone));
  assert(document.importNode(template.content, true).firstChild instanceof Clone);
  global.customElements.upgrade(template.content);
  assert.equal(template.content.firstChild.constructor, Clone);
  template.innerHTML = "<x-clone>";
  assert.equal(template.content.firstChild.constructor, global.HTMLElement);

  const detached = document.createElement("div");
  detached.innerHTML = "<x-clone>";
  assert.equal(detached.firstChild.constructor, global.HTMLElement);
  log.length = 0;
  document.body.innerHTML = "<x-clone>";
  assert(document.body.firstChild instanceof Clone);
  assert.deepEqual(plain(log), [["constructed", true], ["connected"]]);
});

test("upgrade failures leave the element failed and report the exception", () => {
  const { global, document, errors, run } = realm();
  const element = document.createElement("x-broken");
  document.body.append(element);
  global.customElements.define("x-broken", run("(class extends HTMLElement { constructor() { throw new Error('nope'); } })"));
  assert.equal(errors.at(-1).message, "nope");
  assert.equal(element.constructor, global.HTMLElement);
  element.remove();
  document.body.append(element);
  assert.equal(errors.length, 1);

  const twice = document.createElement("x-twice");
  document.body.append(twice);
  global.customElements.define("x-twice", run(`(class Twice extends HTMLElement {
    constructor() { Reflect.construct(HTMLElement, [], new.target); super(); }
  })`));
  assert(errors.at(-1) instanceof TypeError, "a second HTMLElement construction during an upgrade is rejected");
  assert.equal(errors.length, 2);
});
