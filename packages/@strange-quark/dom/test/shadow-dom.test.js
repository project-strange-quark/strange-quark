// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { bootstrapDocument, installDOM, installInput } from "@strange-quark/dom";
import { Journal, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";
import { nodeId } from "../dist/document.js";

function records(journal) {
  const batch = new DataView(journal.snapshot());
  const end = 16 + batch.getUint32(0, true);
  const result = [];
  for (let offset = 16; offset < end; offset += RECORD_BYTES) {
    result.push([batch.getUint8(offset), ...[4, 8, 12].map(step => batch.getUint32(offset + step, true))]);
  }
  return result;
}

const microtask = () => new Promise(resolve => queueMicrotask(resolve));

test("attachShadow accepts the valid hosts and init members and rejects the rest", () => {
  const global = {};
  const document = installDOM(global);
  for (const name of ["div", "span", "article", "section", "h3", "body", "x-widget"]) {
    const root = document.createElement(name).attachShadow({ mode: "open" });
    assert(root instanceof global.ShadowRoot && root instanceof global.DocumentFragment, name);
  }
  for (const name of ["button", "img", "input", "template", "slot"]) {
    assert.throws(() => document.createElement(name).attachShadow({ mode: "open" }), { name: "NotSupportedError" }, name);
  }
  assert.throws(() => document.createElementNS("urn:other", "div").attachShadow({ mode: "open" }), { name: "NotSupportedError" });
  const host = document.createElement("div");
  assert.throws(() => host.attachShadow(), TypeError);
  assert.throws(() => host.attachShadow({}), TypeError);
  assert.throws(() => host.attachShadow({ mode: "half" }), TypeError);
  assert.throws(() => host.attachShadow({ mode: "open", slotAssignment: "auto" }), TypeError);
  const read = [];
  const init = new Proxy({ mode: "closed", delegatesFocus: 1, clonable: "", serializable: true, slotAssignment: "manual" }, {
    get(target, key) { read.push(key); return target[key]; },
  });
  const root = host.attachShadow(init);
  assert.deepEqual(read, ["clonable", "delegatesFocus", "mode", "serializable", "slotAssignment"]);
  assert.deepEqual([root.host, root.mode, root.delegatesFocus, root.slotAssignment, root.clonable, root.serializable],
    [host, "closed", true, "manual", false, true]);
  assert.equal(host.shadowRoot, null, "a closed root is hidden");
  assert.throws(() => host.attachShadow({ mode: "closed" }), { name: "NotSupportedError" });
  assert.equal(Object.prototype.toString.call(root), "[object ShadowRoot]");
  assert.throws(() => new global.ShadowRoot(), TypeError);

  class Plain extends global.HTMLElement { static disabledFeatures = ["shadow"]; }
  global.customElements.define("no-shadow", Plain);
  assert.throws(() => new Plain().attachShadow({ mode: "open" }), { name: "NotSupportedError" });
});

test("shadow trees have their own root, and the composed root crosses into the document", () => {
  const document = installDOM({});
  const host = document.body.appendChild(document.createElement("div"));
  const root = host.attachShadow({ mode: "open" });
  const inner = root.appendChild(document.createElement("p"));
  const text = inner.appendChild(document.createTextNode("x"));
  assert.equal(host.shadowRoot, root);
  assert.equal(text.getRootNode(), root);
  assert.equal(text.getRootNode({ composed: true }), document);
  assert.equal(inner.parentNode, root);
  assert.equal(root.parentNode, null);
  assert(inner.isConnected && root.isConnected);
  assert.equal(document.contains(inner), false, "contains does not cross the boundary");
  assert.equal(document.getElementById.call(document, "x"), null);
  inner.id = "x";
  assert.equal(document.getElementById("x"), null);
  assert.equal(root.getElementById("x"), inner);
  const walker = document.createTreeWalker(document.body);
  const seen = [];
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) seen.push(node);
  assert.deepEqual(seen, [host], "tree walkers stay in the light tree");
  host.remove();
  assert.equal(inner.isConnected, false);
  assert.throws(() => document.body.appendChild(document.createElement("div")).appendChild(host).shadowRoot.appendChild(host),
    { name: "HierarchyRequestError" }, "a host cannot move into its own shadow tree");
  assert.throws(() => root.cloneNode(), { name: "NotSupportedError" });
  assert.throws(() => document.importNode(root), { name: "NotSupportedError" });
  assert.throws(() => document.adoptNode(root), { name: "HierarchyRequestError" });
});

test("named slots assign their host's children, fall back, and follow slot and name changes", async () => {
  const document = installDOM({});
  const host = document.body.appendChild(document.createElement("div"));
  const header = host.appendChild(document.createElement("h1"));
  header.slot = "title";
  const text = host.appendChild(document.createTextNode("body text"));
  const span = host.appendChild(document.createElement("span"));
  const root = host.attachShadow({ mode: "open" });
  assert.equal(header.assignedSlot, null);
  const title = root.appendChild(document.createElement("slot"));
  title.name = "title";
  const rest = root.appendChild(document.createElement("slot"));
  const fallback = rest.appendChild(document.createElement("em"));
  assert.deepEqual(title.assignedNodes(), [header]);
  assert.deepEqual(rest.assignedNodes(), [text, span]);
  assert.deepEqual(rest.assignedElements(), [span]);
  assert.equal(header.assignedSlot, title);
  assert.equal(text.assignedSlot, rest);
  assert.equal(fallback.assignedSlot, null);

  span.slot = "title";
  assert.deepEqual(title.assignedNodes(), [header, span], "assigned nodes keep the host's order");
  assert.deepEqual(rest.assignedNodes(), [text]);
  title.name = "other";
  assert.deepEqual(title.assignedNodes(), []);
  assert.deepEqual(title.assignedNodes({ flatten: true }), [], "a slot without fallback flattens to nothing");
  text.remove();
  assert.deepEqual(rest.assignedNodes(), []);
  assert.deepEqual(rest.assignedNodes({ flatten: true }), [fallback], "an empty slot flattens to its fallback");

  // A nested host's slot passes its own assigned nodes on.
  const inner = root.appendChild(document.createElement("div"));
  const forwarded = inner.appendChild(document.createElement("slot"));
  forwarded.name = "title";
  title.name = "title";
  const innerRoot = inner.attachShadow({ mode: "open" });
  const innerSlot = innerRoot.appendChild(document.createElement("slot"));
  assert.deepEqual(innerSlot.assignedNodes(), [forwarded]);
  forwarded.name = "unused";
  host.append(Object.assign(document.createElement("b"), { slot: "unused" }));
  assert.deepEqual(innerSlot.assignedNodes({ flatten: true }).map(node => node.localName), ["b"]);

  const closed = document.createElement("div");
  const hidden = closed.appendChild(document.createElement("i"));
  closed.attachShadow({ mode: "closed" }).appendChild(document.createElement("slot"));
  assert.equal(hidden.assignedSlot, null, "assignedSlot hides a closed tree's slot");
});

test("manual slots assign only the nodes assign() lists, in its order", () => {
  const document = installDOM({});
  const host = document.createElement("div");
  const [a, b, c] = ["a", "b", "c"].map(name => host.appendChild(document.createElement(name)));
  const root = host.attachShadow({ mode: "open", slotAssignment: "manual" });
  const first = root.appendChild(document.createElement("slot"));
  const second = root.appendChild(document.createElement("slot"));
  assert.deepEqual(first.assignedNodes(), [], "named matching is off");
  first.assign(c, a, c);
  assert.deepEqual(first.assignedNodes(), [c, a]);
  second.assign(a);
  assert.deepEqual(first.assignedNodes(), [c], "assigning moves a node from its previous slot");
  assert.deepEqual(second.assignedNodes(), [a]);
  const elsewhere = document.createElement("div");
  first.assign(elsewhere, b);
  assert.deepEqual(first.assignedNodes(), [b], "only the host's children are assigned");
  assert.throws(() => first.assign(document.createComment("x")), TypeError);
});

test("slotchange fires at the slot after mutation observer callbacks", async () => {
  const global = {};
  const document = installDOM(global);
  const host = document.body.appendChild(document.createElement("div"));
  const root = host.attachShadow({ mode: "open" });
  const slot = root.appendChild(document.createElement("slot"));
  const order = [];
  new global.MutationObserver(() => order.push("observer")).observe(host, { childList: true });
  slot.addEventListener("slotchange", event => order.push(["slot", event.bubbles, event.composed, event.target === slot]));
  root.addEventListener("slotchange", () => order.push("root"));
  host.append(document.createElement("p"), "text");
  assert.deepEqual(order, []);
  await microtask();
  assert.deepEqual(order, ["observer", ["slot", true, false, true], "root"]);
  order.length = 0;
  const empty = root.appendChild(document.createElement("slot"));
  empty.name = "empty";
  await microtask();
  empty.addEventListener("slotchange", () => order.push("fallback"));
  empty.append("fallback");
  await microtask();
  assert.deepEqual(order, ["fallback", "root"], "a change to an empty slot's fallback signals it");
});

test("events retarget at shadow boundaries, and composedPath hides closed trees", () => {
  const global = {};
  const document = installDOM(global);
  const host = document.body.appendChild(document.createElement("div"));
  const root = host.attachShadow({ mode: "closed" });
  const button = root.appendChild(document.createElement("span"));
  const seen = [];
  const log = name => event => seen.push([name, event.target, event.eventPhase, event.composedPath().length]);
  document.addEventListener("click", log("document"));
  host.addEventListener("click", log("host"));
  root.addEventListener("click", log("root"));
  button.addEventListener("click", log("button"));
  const event = new global.MouseEvent("click", { bubbles: true, composed: true });
  button.dispatchEvent(event);
  assert.deepEqual(seen, [
    ["button", button, 2, 7], ["root", button, 3, 7], ["host", host, 2, 5], ["document", host, 3, 5],
  ]);
  assert.equal(event.target, host, "the target stays retargeted after dispatch");
  assert.deepEqual(event.composedPath(), []);

  seen.length = 0;
  const local = new global.Event("click", { bubbles: true });
  button.dispatchEvent(local);
  assert.deepEqual(seen.map(([name]) => name), ["button", "root"], "a non-composed event stops at its shadow root");
  assert.equal(local.target, null, "a target left in a shadow tree is cleared after dispatch");

  const other = root.appendChild(document.createElement("b"));
  const related = [];
  host.addEventListener("mouseover", event => related.push(["host", event.target, event.relatedTarget]));
  other.addEventListener("mouseover", event => related.push(["other", event.target, event.relatedTarget]));
  document.addEventListener("mouseover", event => related.push(["document", event.target, event.relatedTarget]));
  other.dispatchEvent(new global.MouseEvent("mouseover", { bubbles: true, composed: true, relatedTarget: button }));
  assert.deepEqual(related, [["other", other, button]], "a move inside one shadow tree is invisible outside it");
  related.length = 0;
  other.dispatchEvent(new global.MouseEvent("mouseover", { bubbles: true, composed: true, relatedTarget: document.body }));
  assert.deepEqual(related, [["other", other, document.body], ["host", host, document.body], ["document", host, document.body]]);
});

test("slotted nodes dispatch through their slot", () => {
  const global = {};
  const document = installDOM(global);
  const host = document.body.appendChild(document.createElement("div"));
  const light = host.appendChild(document.createElement("p"));
  const root = host.attachShadow({ mode: "open" });
  const wrapper = root.appendChild(document.createElement("section"));
  const slot = wrapper.appendChild(document.createElement("slot"));
  const event = new global.Event("ping", { bubbles: true });
  let path;
  document.addEventListener("ping", event => { path = event.composedPath(); });
  light.dispatchEvent(event);
  assert.deepEqual(path, [light, slot, wrapper, root, host, document.body, document.documentElement, document, global]);

  const closedHost = document.body.appendChild(document.createElement("div"));
  const closedLight = closedHost.appendChild(document.createElement("p"));
  const closedRoot = closedHost.attachShadow({ mode: "closed" });
  const closedSlot = closedRoot.appendChild(document.createElement("slot"));
  let inside;
  closedSlot.addEventListener("ping", event => { inside = event.composedPath(); });
  closedLight.dispatchEvent(new global.Event("ping", { bubbles: true }));
  assert.deepEqual(path, [closedLight, closedHost, document.body, document.documentElement, document, global],
    "a closed tree's slot stays out of the path its outside sees");
  assert.deepEqual(inside, [closedLight, closedSlot, closedRoot, closedHost, document.body, document.documentElement, document, global]);
});

test("focus in a shadow tree shows as its host and delegatesFocus forwards host focus", () => {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const body = document.appendChild(document.createElement("html")).appendChild(document.createElement("body"));
  const host = body.appendChild(document.createElement("div"));
  const root = host.attachShadow({ mode: "open", delegatesFocus: true });
  root.appendChild(document.createElement("span")).append("label");
  const input = root.appendChild(document.createElement("input"));
  const events = [];
  for (const type of ["focus", "focusin"]) host.addEventListener(type, event => events.push([type, event.target]));
  host.focus();
  assert.equal(document.activeElement, host);
  assert.equal(root.activeElement, input);
  assert.deepEqual(events, [["focus", host], ["focusin", host]]);
  const outside = body.appendChild(document.createElement("button"));
  outside.focus();
  assert.equal(root.activeElement, null);
  host.focus();
  assert.equal(root.activeElement, input);
  host.blur();
  assert.equal(document.activeElement, body, "blurring a host blurs the focus inside it");
  input.focus();
  host.remove();
  assert.equal(document.activeElement, body, "removing the host drops the focus inside it");
});

test("sequential focus navigation visits a shadow tree after its host, and slotted nodes at their slot", () => {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const body = document.appendChild(document.createElement("html")).appendChild(document.createElement("body"));
  const dispatch = installInput(document, { inputResult() {} });
  const button = (parent, label) => {
    const element = parent.appendChild(document.createElement("button"));
    element.id = label;
    return element;
  };
  button(body, "before");
  const host = body.appendChild(document.createElement("div"));
  const slotted = button(host, "slotted");
  const root = host.attachShadow({ mode: "open" });
  button(root, "inner-first");
  root.appendChild(document.createElement("slot"));
  const last = button(root, "inner-last");
  last.tabIndex = 1;
  button(body, "after");
  const order = [];
  for (let step = 0; step < 5; step++) {
    dispatch({ id: 1, type: "keydown", target: 0, related: 0, time: 1, cancelable: true, fields: {
      key: "Tab", code: "Tab", location: 0, repeat: false, isComposing: false, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
    } });
    const shadow = root.activeElement;
    order.push((shadow ?? document.activeElement).id);
  }
  assert.equal(slotted.assignedSlot !== null, true);
  assert.deepEqual(order, ["before", "inner-last", "inner-first", "slotted", "after"]);
});

test("custom elements in shadow trees connect with their host and upgrade when defined", () => {
  const global = {};
  const document = installDOM(global);
  const host = document.createElement("div");
  const root = host.attachShadow({ mode: "open" });
  const pending = root.appendChild(document.createElement("x-late"));
  const calls = [];
  class Widget extends global.HTMLElement {
    connectedCallback() { calls.push(["connected", this.localName]); }
    disconnectedCallback() { calls.push(["disconnected", this.localName]); }
  }
  global.customElements.define("x-widget", Widget);
  root.appendChild(document.createElement("x-widget"));
  assert.deepEqual(calls, []);
  document.body.appendChild(host);
  assert.deepEqual(calls, [["connected", "x-widget"]]);
  global.customElements.define("x-late", class extends Widget {});
  assert.deepEqual(calls.at(-1), ["connected", "x-late"], "define upgrades connected elements in shadow trees");
  assert(pending instanceof Widget);
  host.remove();
  assert.deepEqual(calls.slice(-2), [["disconnected", "x-late"], ["disconnected", "x-widget"]]);
});

test("clonable shadow roots clone with their host, and getHTML serializes serializable ones", () => {
  const document = installDOM({});
  const host = document.createElement("div");
  host.setAttribute("title", "a > b");
  const root = host.attachShadow({ mode: "open", clonable: true, serializable: true, delegatesFocus: true });
  root.append(document.createElement("slot"), "inner");
  host.append(document.createElement("br"), "light");
  const clone = host.cloneNode(false);
  assert.deepEqual([clone.shadowRoot.clonable, clone.shadowRoot.delegatesFocus, clone.shadowRoot.childNodes.length, clone.childNodes.length],
    [true, true, 2, 0]);
  const plain = document.createElement("div");
  plain.attachShadow({ mode: "open" });
  assert.equal(plain.cloneNode(true).shadowRoot, null);
  const wrapper = document.createElement("section");
  wrapper.append(host);
  assert.equal(wrapper.getHTML(), '<div title="a > b"><br>light</div>');
  assert.equal(wrapper.getHTML({ serializableShadowRoots: true }),
    '<div title="a > b"><template shadowrootmode="open" shadowrootdelegatesfocus="" shadowrootserializable="" ' +
    'shadowrootclonable=""><slot></slot>inner</template><br>light</div>');
  assert.equal(plain.getHTML({ shadowRoots: [plain.shadowRoot] }), '<template shadowrootmode="open"></template>');
  assert.equal(root.getHTML(), "<slot></slot>inner");
});

test("mutation observers see a shadow tree only when they observe inside it", async () => {
  const global = {};
  const document = installDOM(global);
  const host = document.body.appendChild(document.createElement("div"));
  const root = host.attachShadow({ mode: "open" });
  const outside = [];
  const inside = [];
  new global.MutationObserver(records => outside.push(...records)).observe(document, { childList: true, subtree: true });
  new global.MutationObserver(records => inside.push(...records)).observe(root, { childList: true, subtree: true });
  root.appendChild(document.createElement("p")).append("x");
  await microtask();
  assert.equal(outside.length, 0);
  assert.deepEqual(inside.map(record => record.target), [root, root.firstChild]);
});

test("the journal carries shadow roots and slot assignment", () => {
  const journal = new Journal();
  const document = bootstrapDocument(journal, 1, 0, []);
  const body = document.appendChild(document.createElement("html")).appendChild(document.createElement("body"));
  const host = body.appendChild(document.createElement("div"));
  const light = host.appendChild(document.createElement("p"));
  const root = host.attachShadow({ mode: "open" });
  const slot = root.appendChild(document.createElement("slot"));
  const written = records(journal);
  const find = opcode => written.filter(([op]) => op === opcode).map(([, a, b, c]) => [a, b, c]);
  assert.deepEqual(find(Opcode.AttachShadow), [[nodeId(root), nodeId(host), 0]]);
  assert.deepEqual(find(Opcode.AssignSlot), [[nodeId(slot), 0, 0], [nodeId(slot), nodeId(light), 0]]);
});
