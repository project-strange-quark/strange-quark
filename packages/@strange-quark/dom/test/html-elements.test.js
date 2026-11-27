// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { installDOM } from "@strange-quark/dom";

test("createElement returns the HTML interface for each tag and leaves unimplemented behaviour as HTMLElement", () => {
  const global = {};
  const document = installDOM(global);
  const interfaces = {
    div: "HTMLDivElement", span: "HTMLSpanElement", p: "HTMLParagraphElement", h1: "HTMLHeadingElement", h6: "HTMLHeadingElement",
    ul: "HTMLUListElement", ol: "HTMLOListElement", li: "HTMLLIElement", dl: "HTMLDListElement", pre: "HTMLPreElement",
    xmp: "HTMLPreElement", blockquote: "HTMLQuoteElement", q: "HTMLQuoteElement", ins: "HTMLModElement", table: "HTMLTableElement",
    caption: "HTMLTableCaptionElement", colgroup: "HTMLTableColElement", tbody: "HTMLTableSectionElement",
    thead: "HTMLTableSectionElement", tr: "HTMLTableRowElement", td: "HTMLTableCellElement", th: "HTMLTableCellElement",
    template: "HTMLTemplateElement", html: "HTMLHtmlElement", head: "HTMLHeadElement", body: "HTMLBodyElement",
    title: "HTMLTitleElement", meta: "HTMLMetaElement", base: "HTMLBaseElement", hr: "HTMLHRElement", time: "HTMLTimeElement",
    data: "HTMLDataElement", optgroup: "HTMLOptGroupElement", legend: "HTMLLegendElement", menu: "HTMLMenuElement",
    slot: "HTMLSlotElement",
  };
  for (const [tag, name] of Object.entries(interfaces)) {
    const element = document.createElement(tag);
    assert(element instanceof global[name], tag);
    assert(element instanceof global.HTMLElement, tag);
    assert.equal(Object.prototype.toString.call(element), `[object ${name}]`, tag);
    assert.throws(() => new global[name](), { name: "TypeError", message: "Illegal constructor" }, name);
  }
  for (const tag of ["dialog", "details", "picture", "fieldset", "audio", "video", "section", "x-y"]) {
    assert.equal(document.createElement(tag).constructor, global.HTMLElement, tag);
  }
  assert.equal(document.createElement("blink").constructor, global.HTMLUnknownElement);
  for (const name of ["HTMLDialogElement", "HTMLDetailsElement", "HTMLPictureElement", "HTMLMediaElement"]) {
    assert.equal(name in global, false, name);
  }
  assert.equal(document.createElementNS("urn:other", "div").constructor, global.Element);
  assert(document.body instanceof global.HTMLBodyElement, "the initial tree uses the same interfaces");
});

test("element interfaces reflect their attributes like Chromium", () => {
  const global = {};
  const document = installDOM(global, undefined, { url: "https://example.test/dir/page" });
  const ol = document.createElement("ol");
  assert.deepEqual([ol.start, ol.reversed, ol.type], [1, false, ""]);
  ol.setAttribute("start", "  -4x");
  ol.reversed = true;
  assert.deepEqual([ol.start, ol.getAttribute("reversed")], [-4, ""]);
  ol.start = 3.9;
  assert.equal(ol.getAttribute("start"), "3");
  const cell = document.createElement("td");
  assert.deepEqual([cell.colSpan, cell.rowSpan, cell.scope, cell.cellIndex], [1, 1, "", -1]);
  cell.setAttribute("colspan", "5000");
  cell.setAttribute("rowspan", "0");
  cell.setAttribute("scope", "ROW");
  assert.deepEqual([cell.colSpan, cell.rowSpan, cell.scope], [1000, 0, "row"]);
  cell.bgColor = null;
  assert.equal(cell.getAttribute("bgcolor"), "");
  const quote = document.createElement("blockquote");
  quote.setAttribute("cite", "../source");
  assert.equal(quote.cite, "https://example.test/source");
  const base = document.createElement("base");
  assert.equal(base.href, "https://example.test/dir/page");
  const title = document.createElement("title");
  title.text = "Hello";
  assert.equal(title.textContent, "Hello");
  const meta = document.createElement("meta");
  meta.httpEquiv = "refresh";
  assert.equal(meta.getAttribute("http-equiv"), "refresh");
  const form = document.body.appendChild(document.createElement("form"));
  const fieldset = form.appendChild(document.createElement("fieldset"));
  const legend = fieldset.appendChild(document.createElement("legend"));
  assert.equal(legend.form, form);
  assert.equal(document.createElement("legend").form, null);
});

test("table interfaces build and index rows, sections and cells", () => {
  const global = {};
  const document = installDOM(global);
  const table = document.createElement("table");
  const first = table.insertRow();
  assert.equal(table.tBodies.length, 1);
  assert.equal(first.parentNode, table.tBodies[0]);
  const head = table.createTHead();
  const headRow = head.insertRow();
  const foot = table.createTFoot();
  const footRow = foot.insertRow(0);
  const caption = table.createCaption();
  assert.equal(table.firstChild, caption);
  assert.equal(caption.nextSibling, head);
  assert.equal(table.lastChild, foot);
  assert.deepEqual([...table.rows], [headRow, first, footRow]);
  assert.deepEqual([headRow.rowIndex, first.rowIndex, footRow.rowIndex, first.sectionRowIndex], [0, 1, 2, 0]);
  const second = table.insertRow(2);
  assert.equal(second.parentNode, foot, "a row inserted before the footer row joins the footer");
  assert.deepEqual([...table.rows], [headRow, first, second, footRow]);
  const cell = second.insertCell();
  const header = second.insertBefore(document.createElement("th"), cell);
  assert.deepEqual([...second.cells], [header, cell]);
  assert.equal(cell.cellIndex, 1);
  second.deleteCell(-1);
  assert.deepEqual([...second.cells], [header]);
  assert.throws(() => second.deleteCell(5), { name: "IndexSizeError" });
  assert.throws(() => table.insertRow(9), { name: "IndexSizeError" });
  table.deleteRow(-1);
  assert.deepEqual([...table.rows], [headRow, first, second]);
  table.deleteTHead();
  table.deleteCaption();
  assert.equal(table.tHead, null);
  assert.equal(table.caption, null);
  assert.throws(() => { table.tHead = document.createElement("tbody"); }, { name: "HierarchyRequestError" });
  const body = table.createTBody();
  assert.equal(body, table.tBodies[1]);
});

test("custom elements must extend HTMLElement itself", () => {
  const context = createContext({});
  const global = runInContext("globalThis", context);
  installDOM(global);
  const Paragraph = runInContext("(class extends HTMLParagraphElement {})", context);
  global.customElements.define("x-paragraph", Paragraph);
  assert.throws(() => new Paragraph(), TypeError);
});
