// SPDX-License-Identifier: MIT

import { Document, Element, nodeId } from "./document.js";
import { DOMException } from "./events.js";

type Rect = { x: number; y: number; width: number; height: number };
type Size = { inlineSize: number; blockSize: number };
type ResizeBox = "content-box" | "border-box" | "device-pixel-content-box";
type Length = { unit: "px" | "%"; value: number };
type IntersectionOptions = {
  root: number | null; rootMargin: Length[]; thresholds: number[];
};
type Host = {
  observeResize(id: number, target: number, box: ResizeBox): void;
  unobserveResize(id: number, target: number): void;
  disconnectResize(id: number): void;
  observeIntersection(id: number, target: number, options: IntersectionOptions): void;
  unobserveIntersection(id: number, target: number): void;
  disconnectIntersection(id: number): void;
  postTask(callback: () => void): void;
};

type ResizeWire = {
  observer: number; target: number; contentRect: Rect;
  contentBoxSize: Size[]; borderBoxSize: Size[]; devicePixelContentBoxSize: Size[];
};
type IntersectionWire = {
  observer: number; target: number; time: number; rootBounds: Rect | null;
  boundingClientRect: Rect; intersectionRect: Rect; isIntersecting: boolean; intersectionRatio: number;
};

export class DOMRectReadOnly {
  constructor(readonly x = 0, readonly y = 0, readonly width = 0, readonly height = 0) { Object.freeze(this); }
  get top(): number { return Math.min(this.y, this.y + this.height); }
  get right(): number { return Math.max(this.x, this.x + this.width); }
  get bottom(): number { return Math.max(this.y, this.y + this.height); }
  get left(): number { return Math.min(this.x, this.x + this.width); }
  toJSON() {
    const { x, y, width, height, top, right, bottom, left } = this;
    return { x, y, width, height, top, right, bottom, left };
  }
}

const token = Symbol("observer entry");
const rect = ({ x, y, width, height }: Rect) => new DOMRectReadOnly(x, y, width, height);

export class ResizeObserverSize {
  constructor(check?: symbol, readonly inlineSize = 0, readonly blockSize = 0) {
    if (check !== token) throw new TypeError("Illegal constructor");
    Object.freeze(this);
  }
}

function boxes(sizes: Size[]): readonly ResizeObserverSize[] {
  return Object.freeze(sizes.map(({ inlineSize, blockSize }) => new ResizeObserverSize(token, inlineSize, blockSize)));
}

export class ResizeObserverEntry {
  readonly contentRect: DOMRectReadOnly;
  readonly contentBoxSize: readonly ResizeObserverSize[];
  readonly borderBoxSize: readonly ResizeObserverSize[];
  readonly devicePixelContentBoxSize: readonly ResizeObserverSize[];

  constructor(check: symbol, readonly target: Element, input: ResizeWire) {
    if (check !== token) throw new TypeError("Illegal constructor");
    this.contentRect = rect(input.contentRect);
    this.contentBoxSize = boxes(input.contentBoxSize);
    this.borderBoxSize = boxes(input.borderBoxSize);
    this.devicePixelContentBoxSize = boxes(input.devicePixelContentBoxSize);
    Object.freeze(this);
  }
}

export class IntersectionObserverEntry {
  readonly time: number;
  readonly rootBounds: DOMRectReadOnly | null;
  readonly boundingClientRect: DOMRectReadOnly;
  readonly intersectionRect: DOMRectReadOnly;
  readonly isIntersecting: boolean;
  readonly intersectionRatio: number;

  constructor(check: symbol, readonly target: Element, input: IntersectionWire) {
    if (check !== token) throw new TypeError("Illegal constructor");
    this.time = input.time;
    this.rootBounds = input.rootBounds === null ? null : rect(input.rootBounds);
    this.boundingClientRect = rect(input.boundingClientRect);
    this.intersectionRect = rect(input.intersectionRect);
    this.isIntersecting = input.isIntersecting;
    this.intersectionRatio = input.intersectionRatio;
    Object.freeze(this);
  }
}

function margins(input: unknown): Length[] {
  const parts = String(input).trim().split(/\s+/);
  if (parts.length > 4) throw new DOMException("Expected one to four root margins", "SyntaxError");
  const values = parts.map(part => {
    const match = /^([+-]?(?:\d*\.\d+|\d+)(?:e[+-]?\d+)?)(px|%)$/i.exec(part);
    if (match === null || !Number.isFinite(Number(match[1]))) {
      throw new DOMException("Root margins must use px or %", "SyntaxError");
    }
    return { value: Number(match[1]), unit: match[2].toLowerCase() as Length["unit"] };
  });
  const [top, right = top, bottom = top, left = right] = values;
  return [top, right, bottom, left];
}

function thresholds(input: number | Iterable<number>): readonly number[] {
  const values = typeof input === "object" ? Array.from(input, Number) : [Number(input)];
  if (values.some(value => !Number.isFinite(value) || value < 0 || value > 1)) {
    throw new RangeError("IntersectionObserver thresholds must be between 0 and 1");
  }
  return Object.freeze(values.length === 0 ? [0] : values.sort((a, b) => a - b));
}

function element(target: unknown): asserts target is Element {
  if (!(target instanceof Element)) throw new TypeError("Expected an Element");
}

function report(global: Record<PropertyKey, unknown>, callback: () => void): void {
  try { callback(); }
  catch (error) { (global.reportError as ((error: unknown) => void) | undefined)?.(error); }
}

export function installObservers(global: Record<PropertyKey, unknown>, host: Host) {
  const resize = new Map<number, ResizeObserver>();
  const intersection = new Map<number, IntersectionObserver>();
  const queued = new Set<IntersectionObserver>();
  let deliverResizeGroup: (observer: ResizeObserver, entries: ResizeWire[]) => void;
  let enqueueIntersection: (observer: IntersectionObserver, entry: IntersectionWire) => void;
  let deliverIntersection: (observer: IntersectionObserver) => void;
  let taskQueued = false;
  let nextId = 1;
  function id(): number {
    if (nextId > 0xffff_ffff) throw new RangeError("Observer ids exhausted");
    return nextId++;
  }

  class ResizeObserver {
    #id = id();
    #targets = new Map<number, { element: Element; box: ResizeBox }>();
    #callback: (entries: ResizeObserverEntry[], observer: ResizeObserver) => void;
    constructor(callback: (entries: ResizeObserverEntry[], observer: ResizeObserver) => void) {
      if (typeof callback !== "function") throw new TypeError("Expected a ResizeObserver callback");
      this.#callback = callback;
    }
    observe(target: Element, options?: { box?: ResizeBox }): void {
      element(target);
      const box = options?.box === undefined ? "content-box" : String(options.box) as ResizeBox;
      if (box !== "content-box" && box !== "border-box" && box !== "device-pixel-content-box") throw new TypeError("Invalid box option");
      const key = nodeId(target);
      if (this.#targets.get(key)?.box === box) return;
      host.observeResize(this.#id, key, box);
      this.#targets.set(key, { element: target, box });
      resize.set(this.#id, this);
    }
    unobserve(target: Element): void {
      element(target);
      const key = nodeId(target);
      if (!this.#targets.has(key)) return;
      host.unobserveResize(this.#id, key);
      this.#targets.delete(key);
      if (this.#targets.size === 0) resize.delete(this.#id);
    }
    disconnect(): void {
      if (this.#targets.size === 0) return;
      host.disconnectResize(this.#id);
      this.#targets.clear();
      resize.delete(this.#id);
    }
    static {
      deliverResizeGroup = (observer, entries) => {
        const result: ResizeObserverEntry[] = [];
        for (const entry of entries) {
          const target = observer.#targets.get(entry.target)?.element;
          if (target !== undefined) result.push(new ResizeObserverEntry(token, target, entry));
        }
        if (result.length !== 0) report(global, () => observer.#callback(result, observer));
      };
    }
  }

  function deliverResize(entries: ResizeWire[]): void {
    const groups = new Map<number, ResizeWire[]>();
    for (const entry of entries) {
      let group = groups.get(entry.observer);
      if (group === undefined) groups.set(entry.observer, group = []);
      group.push(entry);
    }
    for (const [key, group] of [...groups].sort(([a], [b]) => a - b)) {
      const observer = resize.get(key);
      if (observer !== undefined) deliverResizeGroup(observer, group);
    }
  }

  class IntersectionObserver {
    #id = id();
    #root: Element | Document | null;
    #margins: Length[];
    #thresholds: readonly number[];
    #targets = new Map<number, Element>();
    #queued: IntersectionObserverEntry[] = [];
    #callback: (entries: IntersectionObserverEntry[], observer: IntersectionObserver) => void;
    constructor(callback: (entries: IntersectionObserverEntry[], observer: IntersectionObserver) => void, options?: {
      root?: Element | Document | null; rootMargin?: string; threshold?: number | Iterable<number>;
    }) {
      if (typeof callback !== "function") throw new TypeError("Expected an IntersectionObserver callback");
      const root = options?.root ?? null;
      if (root !== null && !(root instanceof Document) && !(root instanceof Element)) throw new TypeError("Invalid observer root");
      this.#root = root;
      this.#margins = margins(options?.rootMargin ?? "0px");
      this.#thresholds = thresholds(options?.threshold ?? 0);
      this.#callback = callback;
    }
    get root(): Element | Document | null { return this.#root; }
    get rootMargin(): string { return this.#margins.map(length => length.value + length.unit).join(" "); }
    get thresholds(): readonly number[] { return this.#thresholds; }
    observe(target: Element): void {
      element(target);
      const key = nodeId(target);
      if (this.#targets.has(key)) return;
      host.observeIntersection(this.#id, key, {
        root: this.#root === null ? null : nodeId(this.#root),
        rootMargin: this.#margins, thresholds: [...this.#thresholds],
      });
      this.#targets.set(key, target);
      intersection.set(this.#id, this);
    }
    unobserve(target: Element): void {
      element(target);
      const key = nodeId(target);
      if (!this.#targets.has(key)) return;
      host.unobserveIntersection(this.#id, key);
      this.#targets.delete(key);
      if (this.#targets.size === 0) intersection.delete(this.#id);
    }
    disconnect(): void {
      if (this.#targets.size === 0) return;
      host.disconnectIntersection(this.#id);
      this.#targets.clear();
      intersection.delete(this.#id);
    }
    takeRecords(): IntersectionObserverEntry[] {
      const entries = this.#queued;
      this.#queued = [];
      queued.delete(this);
      return entries;
    }
    static {
      enqueueIntersection = (observer, entry) => {
        const target = observer.#targets.get(entry.target);
        if (target === undefined) return;
        observer.#queued.push(new IntersectionObserverEntry(token, target, entry));
        queued.add(observer);
      };
      deliverIntersection = observer => {
        const entries = observer.takeRecords();
        if (entries.length !== 0) report(global, () => observer.#callback(entries, observer));
      };
    }
  }

  function queueIntersection(entries: IntersectionWire[]): void {
    for (const entry of entries) {
      const observer = intersection.get(entry.observer);
      if (observer !== undefined) enqueueIntersection(observer, entry);
    }
    if (taskQueued || queued.size === 0) return;
    taskQueued = true;
    host.postTask(() => {
      taskQueued = false;
      for (const observer of [...queued]) deliverIntersection(observer);
    });
  }

  Object.assign(global, {
    ResizeObserver, ResizeObserverEntry, ResizeObserverSize,
    IntersectionObserver, IntersectionObserverEntry, DOMRectReadOnly,
  });
  return { deliverResize, queueIntersection };

}
