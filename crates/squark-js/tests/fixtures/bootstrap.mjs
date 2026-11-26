// SPDX-License-Identifier: MIT

export { bootstrapDocument } from "./index.js";
import { packedSheetOwner } from "./document.js";
import { Journal } from "./journal.js";

let batches;

export function setup() {
  batches = [];
  return new Journal((buffer, length) => {
    batches.push(buffer.slice(0, length));
    return new DataView(buffer).getUint32(0, true) / 16;
  });
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

export function check(document, journal, quirks) {
  assert(document.compatMode === (quirks === 2 ? "BackCompat" : "CSS1Compat"), "compatMode");
  assert(document.firstChild === document.doctype && document.doctype.name === "html", "doctype");
  const html = document.documentElement;
  assert(document.doctype.nextSibling === html, "root order");
  assert(html.ownerDocument === document && html.parentNode === document, "root identity");
  const head = html.firstChild;
  const style = head.firstChild;
  assert(style.textContent === ".app { color: red }", "style text");
  assert(packedSheetOwner(document, 40) === style, "inline stylesheet owner");
  assert(packedSheetOwner(document, 41) === style.nextSibling, "link stylesheet owner");
  assert(style.sheet === document.styleSheets[0] && style.sheet.ownerNode === style, "packed style sheet owner");
  const div = head.nextSibling.firstChild;
  assert(div.getAttribute("ID") === "app", "HTML attribute");
  assert(div.firstChild.nodeType === 8 && div.firstChild.nodeValue === "omitted from text", "comment");
  assert(div.textContent === "\ud800💡", "UTF-16 text");
  const svg = div.firstChild.nextSibling.nextSibling;
  assert(svg.namespaceURI === "http://www.w3.org/2000/svg", "SVG namespace");
  assert(svg.getAttribute("xlink:href") === "#target", "qualified attribute");
  assert(svg.getAttributeNS("http://www.w3.org/1999/xlink", "href") === "#target", "attribute namespace");
  const template = div.nextSibling;
  assert(template.localName === "template" && template.textContent === "", "inert template");
  assert(template.content.nodeType === 11 && template.content.parentNode === null, "template fragment");
  assert(template.content.firstChild.parentNode === template.content, "template fragment identity");
  assert(template.content.textContent === "inert contents", "template contents");
  assert(journal.flush() === 0 && batches.length === 0, "bootstrap must be silent");
  div.appendChild(document.createElement("DIV"));
  document.createElement("section");
  assert(journal.flush() === 4 && batches.length === 1, "subsequent journal");
  return batches[0];
}

function childElement(parent, name) {
  for (let child = parent.firstChild; child !== null; child = child.nextSibling) {
    if (child.localName === name) {
      return child;
    }
  }
  throw new Error(`Missing ${name} child`);
}

export function checkToolsFixture(document, journal, quirks, sheetId) {
  assert(document.compatMode === (quirks === 2 ? "BackCompat" : "CSS1Compat"), "packed compatMode");
  const html = document.documentElement;
  assert(html.localName === "html", "packed root");
  const body = childElement(html, "body");
  const template = document.getElementById("tpl");
  if (template !== null) {
    assert(template.firstChild === null && template.content.parentNode === null, "packed inert template");
    const inner = childElement(template.content, "div");
    assert(inner.className === "inner" && inner.textContent === "hello", "packed template contents");
  } else if (quirks === 0) {
    const head = childElement(html, "head");
    const style = childElement(head, "style");
    assert(packedSheetOwner(document, sheetId) === style, "packed sheet owner");
    assert(style.textContent === "body { margin: 0; color: #222; }", "packed style text");
    assert(childElement(head, "link").getAttribute("href") === "./styles/app.css", "packed link");
    assert(childElement(head, "script").textContent === "window.__boot = 1;", "packed script text");
    assert(globalThis.__boot === undefined, "no script evaluation during replay");
    const div = childElement(body, "div");
    assert(div.getAttribute("id") === "app" && div.textContent === "Hello 🌍", "packed HTML");
    const use = childElement(childElement(body, "svg"), "use");
    assert(use.getAttributeNS("http://www.w3.org/1999/xlink", "href") === "#icon", "packed namespace");
  }
  assert(journal.flush() === 0 && batches.length === 0, "packed bootstrap must be silent");
}
