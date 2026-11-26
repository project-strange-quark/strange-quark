// SPDX-License-Identifier: MIT

import { documentJournal, flushDocument, reflectedEvent } from "./document.js";
import type { Document } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";
import { FontFaceAction, Opcode } from "./journal.js";
import { bufferBytes } from "./web-crypto.js";

type Host = {
  stageBytes(bytes: Uint8Array): number;
  fontLoad(id: number): void;
  fontMatch(font: string, text: string): number[];
  postTask(callback: () => void): void;
};

const cssNames = {
  style: "style", weight: "weight", stretch: "stretch", unicodeRange: "unicode-range",
  variant: "variant", featureSettings: "font-feature-settings", variationSettings: "font-variation-settings",
  display: "font-display", ascentOverride: "ascent-override", descentOverride: "descent-override",
  lineGapOverride: "line-gap-override",
} as const;
const defaults: Record<keyof typeof cssNames, string> = {
  style: "normal", weight: "normal", stretch: "normal", unicodeRange: "U+0-10FFFF",
  variant: "normal", featureSettings: "normal", variationSettings: "normal", display: "auto",
  ascentOverride: "normal", descentOverride: "normal", lineGapOverride: "normal",
};
type Descriptor = keyof typeof cssNames;
type Status = "unloaded" | "loading" | "loaded" | "error";

export function installFonts(global: Record<PropertyKey, unknown>, document: Document, host: Host) {
  const journal = documentJournal(document);
  const known = new Map<number, FontFace>();
  const states = new WeakMap<FontFace, {
    id: number;
    family: string;
    source: string | Uint8Array<ArrayBuffer>;
    baseUrl: string;
    values: Record<Descriptor, string>;
    status: Status;
    promise?: ReturnType<typeof Promise.withResolvers<FontFace>>;
    automatic: boolean;
  }>();
  let next = 0;

  function define(face: FontFace): void {
    const state = states.get(face)!;
    const options = Object.fromEntries(Object.entries(cssNames).map(([name, css]) => [css, state.values[name as Descriptor]]));
    const source = typeof state.source === "string" ? { css: state.source, baseUrl: state.baseUrl }
      : { bufferId: host.stageBytes(state.source) };
    journal.record(Opcode.FontFace, state.id, state.family, JSON.stringify({ ...options, source }), FontFaceAction.Define);
  }

  function begin(face: FontFace): boolean {
    const state = states.get(face)!;
    if (state.status !== "unloaded") return false;
    state.status = "loading";
    fonts.start(face);
    return true;
  }

  class FontFace {
    declare style: string;
    declare weight: string;
    declare stretch: string;
    declare unicodeRange: string;
    declare variant: string;
    declare featureSettings: string;
    declare variationSettings: string;
    declare display: string;
    declare ascentOverride: string;
    declare descentOverride: string;
    declare lineGapOverride: string;

    constructor(family: string, source: string | ArrayBuffer | ArrayBufferView, descriptors: Partial<Record<Descriptor, string>> = {}) {
      if (arguments.length < 2) throw new TypeError("FontFace requires a family and source");
      const data = typeof source === "string" ? source : new Uint8Array(bufferBytes(source));
      const values = { ...defaults };
      for (const name of Object.keys(cssNames) as Descriptor[]) {
        const value = descriptors?.[name];
        if (value !== undefined) values[name] = String(value);
      }
      const id = ++next;
      states.set(this, { id, family: String(family), source: data, baseUrl: document.URL, values, status: "unloaded", automatic: false });
      known.set(id, this);
      define(this);
      if (typeof data !== "string") void this.load().catch(() => {});
    }

    get family(): string { return states.get(this)!.family; }
    set family(value: string) { states.get(this)!.family = String(value); define(this); }
    get status(): Status { return states.get(this)!.status; }
    get loaded(): Promise<FontFace> {
      const state = states.get(this)!;
      return (state.promise ??= Promise.withResolvers<FontFace>()).promise;
    }
    load(): Promise<FontFace> {
      const state = states.get(this)!;
      const promise = this.loaded;
      if (begin(this)) {
        try {
          flushDocument(document);
          host.fontLoad(state.id);
        } catch (error) {
          host.postTask(() => complete(state.id, (error as Error).message));
        }
      }
      return promise;
    }
  }
  for (const name of Object.keys(cssNames) as Descriptor[]) {
    Object.defineProperty(FontFace.prototype, name, {
      configurable: true, enumerable: true,
      get(this: FontFace) { return states.get(this)!.values[name]; },
      set(this: FontFace, value: string) { states.get(this)!.values[name] = String(value); define(this); },
    });
  }

  class FontFaceSetLoadEvent extends Event {
    readonly fontfaces: readonly FontFace[];
    constructor(type: string, init: { fontfaces?: Iterable<FontFace> } = {}) {
      super(type);
      this.fontfaces = Object.freeze([...(init.fontfaces ?? [])]);
    }
  }

  const secret = Symbol("FontFaceSet");
  class FontFaceSet extends EventTarget {
    #members = new Set<FontFace>();
    #loading = new Set<FontFace>();
    #loaded: FontFace[] = [];
    #failed: FontFace[] = [];
    #ready: Promise<FontFaceSet> = Promise.resolve(this);
    #finish?: (set: FontFaceSet) => void;

    constructor(token: typeof secret) {
      if (token !== secret) throw new TypeError("Illegal constructor");
      super();
    }
    get status(): "loaded" | "loading" { return this.#loading.size ? "loading" : "loaded"; }
    get ready(): Promise<FontFaceSet> { return this.#ready; }
    get size(): number { return this.#members.size; }
    add(face: FontFace): this {
      const state = states.get(face);
      if (state === undefined) throw new TypeError("Expected a FontFace");
      if (!this.#members.has(face)) {
        journal.record(Opcode.FontFace, state.id, face.family, "{}", FontFaceAction.Add);
        this.#members.add(face);
        if (face.status === "loading") this.start(face);
      }
      return this;
    }
    delete(face: FontFace): boolean {
      if (!this.#members.delete(face)) return false;
      journal.record(Opcode.FontFace, states.get(face)!.id, face.family, "{}", FontFaceAction.Remove);
      if (this.#loading.delete(face)) this.#settle();
      return true;
    }
    clear(): void { for (const face of this.#members) this.delete(face); }
    has(face: FontFace): boolean { return this.#members.has(face); }
    values(): SetIterator<FontFace> { return this.#members.values(); }
    keys(): SetIterator<FontFace> { return this.values(); }
    entries(): SetIterator<[FontFace, FontFace]> { return this.#members.entries(); }
    [Symbol.iterator](): SetIterator<FontFace> { return this.values(); }
    forEach(callback: (face: FontFace, key: FontFace, set: FontFaceSet) => void, thisArg?: unknown): void {
      this.#members.forEach(face => callback.call(thisArg, face, face, this));
    }
    #match(font: string, text: string): FontFace[] {
      flushDocument(document);
      return host.fontMatch(String(font), String(text)).map(id => known.get(id))
        .filter((face): face is FontFace => face !== undefined && this.has(face));
    }
    async load(font: string, text = " "): Promise<FontFace[]> {
      return Promise.all(this.#match(font, text).map(face => face.load()));
    }
    check(font: string, text = " "): boolean {
      return this.#match(font, text).every(face => face.status === "loaded");
    }
    start(face: FontFace): void {
      if (!this.#members.has(face) || this.#loading.has(face)) return;
      const first = this.#loading.size === 0;
      this.#loading.add(face);
      if (first) {
        const { promise, resolve } = Promise.withResolvers<FontFaceSet>();
        this.#ready = promise;
        this.#finish = resolve;
        host.postTask(() => dispatchTrustedEvent(this, new FontFaceSetLoadEvent("loading")));
      }
    }
    complete(face: FontFace): void {
      if (!this.#loading.delete(face)) return;
      (face.status === "loaded" ? this.#loaded : this.#failed).push(face);
      this.#settle();
    }
    #settle(): void {
      if (this.#loading.size !== 0) return;
      this.#finish?.(this);
      this.#finish = undefined;
      const loaded = this.#loaded, failed = this.#failed;
      this.#loaded = [];
      this.#failed = [];
      host.postTask(() => {
        dispatchTrustedEvent(this, new FontFaceSetLoadEvent("loadingdone", { fontfaces: loaded }));
        if (failed.length) dispatchTrustedEvent(this, new FontFaceSetLoadEvent("loadingerror", { fontfaces: failed }));
      });
    }
  }
  for (const event of ["loading", "loadingdone", "loadingerror"]) reflectedEvent(FontFaceSet.prototype, event);

  const fonts = new FontFaceSet(secret);
  Object.defineProperty(document, "fonts", { configurable: true, get: () => fonts });
  Object.assign(global, { FontFace, FontFaceSet, FontFaceSetLoadEvent });

  function complete(id: number, error: string | null): void {
    const face = known.get(id);
    if (face === undefined) return;
    const state = states.get(face)!;
    if (state.status !== "loading") return;
    if (error === null) {
      state.status = "loaded";
      state.promise!.resolve(face);
    } else {
      state.status = "error";
      state.promise!.reject(new DOMException(error, "NetworkError"));
    }
    fonts.complete(face);
  }
  return (id: number, error?: string | null): void => {
    const face = known.get(id);
    if (face === undefined) return;
    const state = states.get(face)!;
    if (error === undefined) {
      if (begin(face)) {
        state.automatic = true;
        // Rendering can start a load even when script never observes its promise.
        void face.loaded.catch(() => {});
      }
    } else if (state.automatic) {
      // Even a cached result settles after the queued loading event.
      host.postTask(() => complete(id, error));
    } else {
      complete(id, error);
    }
  };
}
