// SPDX-License-Identifier: MIT

import { connectedInputTarget, Document, Element, readScrollOffset, recordScroll } from "./document.js";
import { dispatchTrustedEvent, Event } from "./events.js";
import type { CSSStyleDeclaration } from "./style.js";

type Offset = { left: number; top: number };
type Owner = Document | Element;
type Sample = { target: number; x: number; y: number };
type Position = { left?: number; top?: number; behavior?: "auto" | "instant" | "smooth" };
type Alignment = "start" | "center" | "end" | "nearest";
type IntoView = {
  block?: Alignment; inline?: Alignment; behavior?: Position["behavior"]; container?: "all" | "nearest";
};
type Edges = { left: number; top: number; right: number; bottom: number };
type ScrollElement = Element & {
  clientWidth: number; clientHeight: number; clientLeft: number; clientTop: number;
  scrollWidth: number; scrollHeight: number;
  getClientRects(): ArrayLike<Edges>;
  getBoundingClientRect(): Edges;
};
type View = { innerWidth: number; innerHeight: number; getComputedStyle(element: Element): CSSStyleDeclaration };
type ScrollState = {
  offsets: WeakMap<Owner, Offset>;
  requested: WeakMap<Owner, Offset>;
  events: Set<Owner>;
};
const states = new WeakMap<Document, ScrollState>();
const zero = Object.freeze({ left: 0, top: 0 });

function documentOf(owner: Owner): Document {
  return owner instanceof Document ? owner : owner.ownerDocument;
}

function scrollOwner(element: Element): Owner {
  return element === element.ownerDocument.scrollingElement ? element.ownerDocument : element;
}

function offset(owner: Owner): Offset {
  const state = states.get(documentOf(owner));
  if (state === undefined) return zero;
  if (state.requested.has(owner)) {
    const next = readScrollOffset(owner);
    cache(state, owner, next);
  }
  return state.offsets.get(owner) ?? zero;
}

function cache(state: ScrollState, owner: Owner, next: Offset): void {
  const old = state.offsets.get(owner) ?? zero;
  state.requested.delete(owner);
  if (old.left === next.left && old.top === next.top) return;
  state.offsets.set(owner, next);
  state.events.add(owner);
}

function coordinate(value: unknown): number {
  const number = Number(value);
  if (!Number.isFinite(number) || number === 0) return 0;
  const limit = 3.4028234663852886e38;
  return Math.fround(Math.max(-limit, Math.min(limit, number)));
}

function validateBehavior(value: unknown): void {
  if (value !== undefined && value !== "auto" && value !== "instant" && value !== "smooth") {
    throw new TypeError("Invalid scroll behavior");
  }
}

function write(owner: Owner, options: Position): void {
  if (!owner.isConnected || options.left === undefined && options.top === undefined) return;
  const state = states.get(documentOf(owner));
  if (state === undefined) return;
  const basis = state.requested.get(owner) ?? offset(owner);
  const left = options.left === undefined ? basis.left : coordinate(options.left);
  const top = options.top === undefined ? basis.top : coordinate(options.top);
  if (left === basis.left && top === basis.top) return;
  recordScroll(owner, left, top);
  // Keep both requested axes until a read flushes and gets the clamped offset.
  state.requested.set(owner, { left, top });
}

function scrollTo(owner: Owner, first: Position | number | null | undefined, y: number | undefined, count: number): void {
  if (count === 1 && first != null && typeof first !== "object" && typeof first !== "function") {
    throw new TypeError("scrollTo requires both coordinates or an options object");
  }
  if (count >= 2) {
    write(owner, { left: Number(first), top: Number(y) });
    return;
  }
  const options = first as Position | null | undefined;
  validateBehavior(options?.behavior);
  if (options != null) write(owner, { left: options.left, top: options.top });
}

function alignment(value: unknown, fallback: Alignment): Alignment {
  if (value === undefined) return fallback;
  if (value === "start" || value === "center" || value === "end" || value === "nearest") return value;
  throw new TypeError("Invalid scrollIntoView alignment");
}

function delta(start: number, end: number, portStart: number, portEnd: number, align: Alignment): number {
  const leading = start - portStart;
  const trailing = end - portEnd;
  if (align === "start") return leading;
  if (align === "end") return trailing;
  if (align === "center") return (leading + trailing) / 2;
  if (leading < 0 && trailing > 0 || leading >= 0 && trailing <= 0) return 0;
  const fits = end - start <= portEnd - portStart;
  return (leading < 0) === fits ? leading : trailing;
}

function programmaticallyScrollable(overflow: string): boolean {
  return overflow === "auto" || overflow === "scroll" || overflow === "hidden";
}

function scrollIntoView(target: Element, arg: boolean | IntoView = true): void {
  const options: IntoView = typeof arg === "boolean" ? { block: arg ? "start" : "end" } : arg;
  const block = alignment(options?.block, "start");
  const inline = alignment(options?.inline, "nearest");
  validateBehavior(options?.behavior);
  if (options?.container !== undefined && options.container !== "all" && options.container !== "nearest") {
    throw new TypeError("Invalid scrollIntoView container");
  }
  if (!target.isConnected) return;

  const element = target as ScrollElement;
  const document = element.ownerDocument;
  const scroller = document.scrollingElement as ScrollElement | null;
  const view = document.defaultView as unknown as View | null;
  if (scroller === null || view === null || element.getClientRects().length === 0) return;
  const targetBox = element.getBoundingClientRect();
  const offsets: [Owner, Position][] = [];
  let movedX = 0, movedY = 0;

  for (let node = element.parentElement as ScrollElement | null; node !== null;
       node = node.parentElement as ScrollElement | null) {
    const owner = scrollOwner(node);
    const viewport = owner === document;
    const style = viewport ? null : view.getComputedStyle(node);
    const axisX = viewport || programmaticallyScrollable(style!.getPropertyValue("overflow-x"));
    const axisY = viewport || programmaticallyScrollable(style!.getPropertyValue("overflow-y"));
    if (!axisX && !axisY) continue;
    const width = viewport ? view.innerWidth : node.clientWidth;
    const height = viewport ? view.innerHeight : node.clientHeight;
    const bounds = viewport ? null : node.getBoundingClientRect();
    const left = bounds === null ? 0 : bounds.left + node.clientLeft;
    const top = bounds === null ? 0 : bounds.top + node.clientTop;
    const current = offset(owner);
    const clamp = (value: number, overflow: number) => Math.max(0, Math.min(value, Math.max(0, overflow)));
    const x = axisX ? clamp(current.left + delta(targetBox.left - movedX, targetBox.right - movedX, left, left + width, inline),
      node.scrollWidth - width) : current.left;
    const y = axisY ? clamp(current.top + delta(targetBox.top - movedY, targetBox.bottom - movedY, top, top + height, block),
      node.scrollHeight - height) : current.top;
    movedX += x - current.left;
    movedY += y - current.top;
    if (x !== current.left || y !== current.top) offsets.push([owner, { left: x, top: y }]);
    if (viewport || options?.container === "nearest") break;
  }
  for (const [owner, position] of offsets) write(owner, position);
}

export function installScroll(global: Record<PropertyKey, unknown>, document: Document) {
  const state: ScrollState = { offsets: new WeakMap(), requested: new WeakMap(), events: new Set() };
  states.set(document, state);
  Object.defineProperties(Element.prototype, {
    scrollLeft: {
      get(this: Element) { return offset(scrollOwner(this)).left; },
      set(this: Element, value: number) { write(scrollOwner(this), { left: value }); },
      configurable: true,
    },
    scrollTop: {
      get(this: Element) { return offset(scrollOwner(this)).top; },
      set(this: Element, value: number) { write(scrollOwner(this), { top: value }); },
      configurable: true,
    },
    scrollTo: {
      value: function(this: Element, first?: Position | number, y?: number) {
        scrollTo(scrollOwner(this), first, y, arguments.length);
      }, configurable: true, writable: true,
    },
    scrollIntoView: {
      value: function(this: Element, options?: boolean | IntoView) {
        scrollIntoView(this, options);
      }, configurable: true, writable: true,
    },
  });
  const scrollWindow = function(first?: Position | number, y?: number) {
    scrollTo(document, first, y, arguments.length);
  };
  Object.defineProperties(global, {
    scrollTo: { value: scrollWindow, configurable: true, writable: true },
    scrollX: { get: () => offset(document).left, configurable: true },
    scrollY: { get: () => offset(document).top, configurable: true },
    pageXOffset: { get: () => offset(document).left, configurable: true },
    pageYOffset: { get: () => offset(document).top, configurable: true },
  });
  return {
    sync(entries: Sample[]): void {
      for (const entry of entries) {
        const target = connectedInputTarget(document, entry.target);
        if (!(target instanceof Element) && target !== document) continue;
        const owner = target instanceof Element ? scrollOwner(target) : document;
        cache(state, owner, { left: entry.x, top: entry.y });
      }
    },
    dispatch(): void {
      const owners = [...state.events];
      state.events.clear();
      for (const owner of owners) {
        if (owner.isConnected) dispatchTrustedEvent(owner, new Event("scroll", { bubbles: owner === document }));
      }
    },
  };
}
