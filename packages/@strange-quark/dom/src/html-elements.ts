// SPDX-License-Identifier: MIT

// HTML element interfaces whose members reflect content attributes or walk
// the element's own children. Elements whose interfaces need behaviour that
// Strange Quark lacks (dialog, details, media, picture, slot, fieldset
// validation, image maps, ...) stay HTMLElement, so instanceof checks and
// feature detection see that behaviour as missing.

import { defineElementInterface, Element, formFor, HTMLElement, HTMLFormElement, reflect, resolvedURL, Text } from "./document.js";
import type { Node } from "./document.js";
import { HTMLCollection } from "./collections.js";
import { DOMException } from "./events.js";

const HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";

function isHTML(node: Node | null, ...names: string[]): node is HTMLElement {
  return node instanceof Element && node.namespaceURI === HTML_NAMESPACE && names.includes(node.localName);
}

function children(element: Element, ...names: string[]): HTMLElement[] {
  return [...element.children].filter((child): child is HTMLElement => isHTML(child, ...names));
}

function legacyString(value: unknown): string { return value === null ? "" : String(value); }

/** [LegacyNullToEmptyString] DOMString attributes. */
function reflectNullable(prototype: object, fields: Record<string, string>): void {
  reflect(prototype, fields);
  for (const property of Object.keys(fields)) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, property)!;
    const setter = descriptor.set!;
    Object.defineProperty(prototype, property, {
      ...descriptor,
      set: Object.defineProperty(function (this: Element, value: unknown) { setter.call(this, legacyString(value)); }, "name",
        { value: `set ${property}` }),
    });
  }
}

function reflectURL(prototype: object, fields: Record<string, string>): void {
  reflect(prototype, fields);
  for (const [property, attribute] of Object.entries(fields)) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, property)!;
    Object.defineProperty(prototype, property, { ...descriptor, get(this: Element) { return resolvedURL(this, attribute); } });
  }
}

/** The HTML rules for parsing integers, or null when the value has none. */
function parseInteger(value: string | null): number | null {
  const match = value?.match(/^[\t\n\f\r ]*([+-]?\d+)/);
  return match === null || match === undefined ? null : Number(match[1]);
}

function reflectLong(prototype: object, property: string, attribute: string, fallback: number): void {
  Object.defineProperty(prototype, property, {
    configurable: true, enumerable: true,
    get(this: Element) {
      const value = parseInteger(this.getAttribute(attribute));
      return value === null || value < -0x8000_0000 || value > 0x7fff_ffff ? fallback : value;
    },
    set(this: Element, value: unknown) { this.setAttribute(attribute, String(Number(value) | 0)); },
  });
}

/** An unsigned long reflected and clamped to [min, max]. */
function reflectClamped(prototype: object, property: string, attribute: string, fallback: number, min: number, max: number): void {
  Object.defineProperty(prototype, property, {
    configurable: true, enumerable: true,
    get(this: Element) {
      const raw = this.getAttribute(attribute);
      const value = raw?.match(/^[\t\n\f\r ]*\+?(\d+)/)?.[1];
      return value === undefined ? fallback : Math.min(Math.max(Number(value), min), max);
    },
    set(this: Element, value: unknown) {
      const number = Number(value) >>> 0;
      this.setAttribute(attribute, String(number <= 0x7fff_ffff ? number : fallback));
    },
  });
}

function create(owner: Element, localName: string): HTMLElement {
  return owner.ownerDocument.createElementNS(HTML_NAMESPACE, localName) as HTMLElement;
}

function checkIndex(index: number, length: number, inclusive: boolean): number {
  index = Number(index) | 0;
  if (index < -1 || (inclusive ? index > length : index >= length)) {
    throw new DOMException(`The index ${index} is outside the collection`, "IndexSizeError");
  }
  return index;
}

const liveCollections = new WeakMap<Element, Map<string, HTMLCollection<HTMLElement>>>();

function live(element: Element, key: string, items: () => HTMLElement[]): HTMLCollection<HTMLElement> {
  let cache = liveCollections.get(element);
  if (cache === undefined) liveCollections.set(element, cache = new Map());
  let collection = cache.get(key);
  if (collection === undefined) cache.set(key, collection = new HTMLCollection(items));
  return collection;
}

export class HTMLHtmlElement extends HTMLElement {}
reflect(HTMLHtmlElement.prototype, { version: "version" });
defineElementInterface(HTMLHtmlElement, "html");

export class HTMLHeadElement extends HTMLElement {}
defineElementInterface(HTMLHeadElement, "head");

export class HTMLTitleElement extends HTMLElement {
  get text(): string { return [...this.childNodes].filter(child => child instanceof Text).map(child => (child as Text).data).join(""); }
  set text(value: string) { this.textContent = String(value); }
}
defineElementInterface(HTMLTitleElement, "title");

export class HTMLBaseElement extends HTMLElement {
  get href(): string {
    const raw = this.getAttribute("href") ?? "";
    const URL = (globalThis as { URL?: new (url: string, base: string) => { href: string } }).URL;
    try { return URL === undefined ? raw : new URL(raw, this.ownerDocument.URL).href; }
    catch { return raw; }
  }
  set href(value: string) { this.setAttribute("href", value); }
}
reflect(HTMLBaseElement.prototype, { target: "target" });
defineElementInterface(HTMLBaseElement, "base");

export class HTMLMetaElement extends HTMLElement {}
reflect(HTMLMetaElement.prototype, { name: "name", httpEquiv: "http-equiv", content: "content", media: "media", scheme: "scheme" });
defineElementInterface(HTMLMetaElement, "meta");

export class HTMLBodyElement extends HTMLElement {}
reflectNullable(HTMLBodyElement.prototype, { text: "text", link: "link", vLink: "vlink", aLink: "alink", bgColor: "bgcolor" });
reflect(HTMLBodyElement.prototype, { background: "background" });
defineElementInterface(HTMLBodyElement, "body");

export class HTMLHeadingElement extends HTMLElement {}
reflect(HTMLHeadingElement.prototype, { align: "align" });
defineElementInterface(HTMLHeadingElement, "h1", "h2", "h3", "h4", "h5", "h6");

export class HTMLParagraphElement extends HTMLElement {}
reflect(HTMLParagraphElement.prototype, { align: "align" });
defineElementInterface(HTMLParagraphElement, "p");

export class HTMLHRElement extends HTMLElement {}
reflect(HTMLHRElement.prototype, { align: "align", color: "color", size: "size", width: "width" });
reflect(HTMLHRElement.prototype, { noShade: "noshade" }, true);
defineElementInterface(HTMLHRElement, "hr");

export class HTMLPreElement extends HTMLElement {}
reflectLong(HTMLPreElement.prototype, "width", "width", 0);
defineElementInterface(HTMLPreElement, "pre", "listing", "xmp");

export class HTMLQuoteElement extends HTMLElement {}
reflectURL(HTMLQuoteElement.prototype, { cite: "cite" });
defineElementInterface(HTMLQuoteElement, "blockquote", "q");

export class HTMLOListElement extends HTMLElement {}
reflect(HTMLOListElement.prototype, { reversed: "reversed", compact: "compact" }, true);
reflectLong(HTMLOListElement.prototype, "start", "start", 1);
reflect(HTMLOListElement.prototype, { type: "type" });
defineElementInterface(HTMLOListElement, "ol");

export class HTMLUListElement extends HTMLElement {}
reflect(HTMLUListElement.prototype, { compact: "compact" }, true);
reflect(HTMLUListElement.prototype, { type: "type" });
defineElementInterface(HTMLUListElement, "ul");

export class HTMLMenuElement extends HTMLElement {}
reflect(HTMLMenuElement.prototype, { compact: "compact" }, true);
defineElementInterface(HTMLMenuElement, "menu");

export class HTMLLIElement extends HTMLElement {}
reflectLong(HTMLLIElement.prototype, "value", "value", 0);
reflect(HTMLLIElement.prototype, { type: "type" });
defineElementInterface(HTMLLIElement, "li");

export class HTMLDListElement extends HTMLElement {}
reflect(HTMLDListElement.prototype, { compact: "compact" }, true);
defineElementInterface(HTMLDListElement, "dl");

export class HTMLDivElement extends HTMLElement {}
reflect(HTMLDivElement.prototype, { align: "align" });
defineElementInterface(HTMLDivElement, "div");

export class HTMLSpanElement extends HTMLElement {}
defineElementInterface(HTMLSpanElement, "span");

export class HTMLDataElement extends HTMLElement {}
reflect(HTMLDataElement.prototype, { value: "value" });
defineElementInterface(HTMLDataElement, "data");

export class HTMLTimeElement extends HTMLElement {}
reflect(HTMLTimeElement.prototype, { dateTime: "datetime" });
defineElementInterface(HTMLTimeElement, "time");

export class HTMLModElement extends HTMLElement {}
reflectURL(HTMLModElement.prototype, { cite: "cite" });
reflect(HTMLModElement.prototype, { dateTime: "datetime" });
defineElementInterface(HTMLModElement, "ins", "del");

export class HTMLOptGroupElement extends HTMLElement {}
reflect(HTMLOptGroupElement.prototype, { disabled: "disabled" }, true);
reflect(HTMLOptGroupElement.prototype, { label: "label" });
defineElementInterface(HTMLOptGroupElement, "optgroup");

export class HTMLLegendElement extends HTMLElement {
  get form(): HTMLFormElement | null {
    const parent = this.parentElement;
    return isHTML(parent, "fieldset") ? formFor(parent) : null;
  }
}
reflect(HTMLLegendElement.prototype, { align: "align" });
defineElementInterface(HTMLLegendElement, "legend");

export class HTMLTableCaptionElement extends HTMLElement {}
reflect(HTMLTableCaptionElement.prototype, { align: "align" });
defineElementInterface(HTMLTableCaptionElement, "caption");

export class HTMLTableColElement extends HTMLElement {}
reflectClamped(HTMLTableColElement.prototype, "span", "span", 1, 1, 1000);
reflect(HTMLTableColElement.prototype, { align: "align", ch: "char", chOff: "charoff", vAlign: "valign", width: "width" });
defineElementInterface(HTMLTableColElement, "col", "colgroup");

function tableRows(table: HTMLTableElement): HTMLElement[] {
  const sections = children(table, "thead", "tbody", "tfoot", "tr");
  const rows = (section: HTMLElement) => children(section, "tr");
  return [
    ...sections.filter(section => section.localName === "thead").flatMap(rows),
    ...sections.flatMap(section => section.localName === "tr" ? [section] : section.localName === "tbody" ? rows(section) : []),
    ...sections.filter(section => section.localName === "tfoot").flatMap(rows),
  ];
}

function insertRow(parent: Element, rows: HTMLElement[], index: number): HTMLElement {
  const row = create(parent, "tr");
  const before = index === -1 || index === rows.length ? null : rows[index];
  (before?.parentNode ?? rows.at(-1)?.parentNode ?? parent).insertBefore(row, before);
  return row;
}

export class HTMLTableElement extends HTMLElement {
  get caption(): HTMLTableCaptionElement | null { return children(this, "caption")[0] as HTMLTableCaptionElement ?? null; }
  set caption(value: HTMLTableCaptionElement | null) {
    if (value !== null && !(value instanceof HTMLTableCaptionElement)) throw new TypeError("Expected an HTMLTableCaptionElement");
    this.deleteCaption();
    if (value !== null) this.insertBefore(value, this.firstChild);
  }
  createCaption(): HTMLTableCaptionElement {
    return this.caption ?? this.insertBefore(create(this, "caption"), this.firstChild) as HTMLTableCaptionElement;
  }
  deleteCaption(): void { this.caption?.remove(); }

  #section(name: string): HTMLTableSectionElement | null { return children(this, name)[0] as HTMLTableSectionElement ?? null; }
  #setSection(name: string, value: HTMLTableSectionElement | null, head: boolean): void {
    if (value !== null && !(value instanceof HTMLTableSectionElement)) throw new TypeError("Expected an HTMLTableSectionElement");
    if (value !== null && !isHTML(value, name)) throw new DOMException(`Expected a ${name} element`, "HierarchyRequestError");
    this.#section(name)?.remove();
    if (value !== null) this.insertBefore(value, head ? this.#afterCaptions() : null);
  }
  #afterCaptions(): Node | null {
    return [...this.childNodes].find(child => child instanceof Element && !isHTML(child, "caption", "colgroup")) ?? null;
  }

  get tHead(): HTMLTableSectionElement | null { return this.#section("thead"); }
  set tHead(value: HTMLTableSectionElement | null) { this.#setSection("thead", value, true); }
  createTHead(): HTMLTableSectionElement {
    return this.tHead ?? this.insertBefore(create(this, "thead"), this.#afterCaptions()) as HTMLTableSectionElement;
  }
  deleteTHead(): void { this.tHead?.remove(); }

  get tFoot(): HTMLTableSectionElement | null { return this.#section("tfoot"); }
  set tFoot(value: HTMLTableSectionElement | null) { this.#setSection("tfoot", value, false); }
  createTFoot(): HTMLTableSectionElement { return this.tFoot ?? this.appendChild(create(this, "tfoot")) as HTMLTableSectionElement; }
  deleteTFoot(): void { this.tFoot?.remove(); }

  get tBodies(): HTMLCollection<HTMLElement> { return live(this, "tBodies", () => children(this, "tbody")); }
  createTBody(): HTMLTableSectionElement {
    const last = children(this, "tbody").at(-1);
    return this.insertBefore(create(this, "tbody"), last === undefined ? null : last.nextSibling) as HTMLTableSectionElement;
  }

  get rows(): HTMLCollection<HTMLElement> { return live(this, "rows", () => tableRows(this)); }
  insertRow(index = -1): HTMLTableRowElement {
    const rows = tableRows(this);
    index = checkIndex(index, rows.length, true);
    if (rows.length === 0) {
      const body = children(this, "tbody").at(-1) ?? this.appendChild(create(this, "tbody"));
      return body.appendChild(create(this, "tr")) as HTMLTableRowElement;
    }
    return insertRow(this, rows, index) as HTMLTableRowElement;
  }
  deleteRow(index: number): void {
    if (arguments.length === 0) throw new TypeError("deleteRow requires an index");
    const rows = tableRows(this);
    index = checkIndex(index, rows.length, false);
    (index === -1 ? rows.at(-1) : rows[index])?.remove();
  }
}
reflect(HTMLTableElement.prototype, { align: "align", border: "border", frame: "frame", rules: "rules", summary: "summary", width: "width" });
reflectNullable(HTMLTableElement.prototype, { bgColor: "bgcolor", cellPadding: "cellpadding", cellSpacing: "cellspacing" });
defineElementInterface(HTMLTableElement, "table");

export class HTMLTableSectionElement extends HTMLElement {
  get rows(): HTMLCollection<HTMLElement> { return live(this, "rows", () => children(this, "tr")); }
  insertRow(index = -1): HTMLTableRowElement {
    const rows = children(this, "tr");
    index = checkIndex(index, rows.length, true);
    return this.insertBefore(create(this, "tr"), index === -1 || index === rows.length ? null : rows[index]) as HTMLTableRowElement;
  }
  deleteRow(index: number): void {
    if (arguments.length === 0) throw new TypeError("deleteRow requires an index");
    const rows = children(this, "tr");
    index = checkIndex(index, rows.length, false);
    (index === -1 ? rows.at(-1) : rows[index])?.remove();
  }
}
reflect(HTMLTableSectionElement.prototype, { align: "align", ch: "char", chOff: "charoff", vAlign: "valign" });
defineElementInterface(HTMLTableSectionElement, "thead", "tbody", "tfoot");

export class HTMLTableRowElement extends HTMLElement {
  get rowIndex(): number {
    const parent = this.parentElement;
    const table = isHTML(parent, "table") ? parent : isHTML(parent, "thead", "tbody", "tfoot") &&
      isHTML(parent.parentElement, "table") ? parent.parentElement : null;
    return table === null ? -1 : tableRows(table as HTMLTableElement).indexOf(this);
  }
  get sectionRowIndex(): number {
    const parent = this.parentElement;
    if (isHTML(parent, "table")) return tableRows(parent as HTMLTableElement).indexOf(this);
    return isHTML(parent, "thead", "tbody", "tfoot") ? children(parent, "tr").indexOf(this) : -1;
  }
  get cells(): HTMLCollection<HTMLElement> { return live(this, "cells", () => children(this, "td", "th")); }
  insertCell(index = -1): HTMLTableCellElement {
    const cells = children(this, "td", "th");
    index = checkIndex(index, cells.length, true);
    return this.insertBefore(create(this, "td"), index === -1 || index === cells.length ? null : cells[index]) as HTMLTableCellElement;
  }
  deleteCell(index: number): void {
    if (arguments.length === 0) throw new TypeError("deleteCell requires an index");
    const cells = children(this, "td", "th");
    index = checkIndex(index, cells.length, false);
    (index === -1 ? cells.at(-1) : cells[index])?.remove();
  }
}
reflect(HTMLTableRowElement.prototype, { align: "align", ch: "char", chOff: "charoff", vAlign: "valign" });
reflectNullable(HTMLTableRowElement.prototype, { bgColor: "bgcolor" });
defineElementInterface(HTMLTableRowElement, "tr");

const cellScopes = new Set(["row", "col", "rowgroup", "colgroup"]);

export class HTMLTableCellElement extends HTMLElement {
  get cellIndex(): number {
    const parent = this.parentElement;
    return isHTML(parent, "tr") ? children(parent, "td", "th").indexOf(this) : -1;
  }
  get scope(): string {
    const value = (this.getAttribute("scope") ?? "").toLowerCase();
    return cellScopes.has(value) ? value : "";
  }
  set scope(value: string) { this.setAttribute("scope", value); }
}
reflectClamped(HTMLTableCellElement.prototype, "colSpan", "colspan", 1, 1, 1000);
reflectClamped(HTMLTableCellElement.prototype, "rowSpan", "rowspan", 1, 0, 65534);
reflect(HTMLTableCellElement.prototype, {
  headers: "headers", abbr: "abbr", align: "align", axis: "axis", height: "height", width: "width",
  ch: "char", chOff: "charoff", vAlign: "valign",
});
reflect(HTMLTableCellElement.prototype, { noWrap: "nowrap" }, true);
reflectNullable(HTMLTableCellElement.prototype, { bgColor: "bgcolor" });
defineElementInterface(HTMLTableCellElement, "td", "th");
