// SPDX-License-Identifier: MIT

import { Document, installLinkNavigation, openExternal, reflectedEvent, setDocumentURL } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";

export type ParsedURL = {
  href: string; protocol: string; origin: string; username: string; password: string;
  hostname: string; host: string; port: string; pathname: string; search: string; hash: string;
};

type MutableURLPart = Exclude<keyof ParsedURL, "href" | "origin">;
type URLHost = {
  parseURL(input: string, base?: string): ParsedURL | undefined;
  updateURL(href: string, part: MutableURLPart, value: string): ParsedURL;
};
type Host = URLHost & {
  cloneMessage(value: unknown): unknown;
  postTask(task: () => void): void;
  navigationWarning(message: string): void;
};

export class HashChangeEvent extends Event {
  readonly oldURL: string;
  readonly newURL: string;
  constructor(type: string, init: { oldURL?: string; newURL?: string } = {}) {
    super(type);
    this.oldURL = String(init.oldURL ?? "");
    this.newURL = String(init.newURL ?? "");
  }
}

export class PopStateEvent extends Event {
  readonly state: unknown;
  constructor(type: string, init: { state?: unknown } = {}) {
    super(type);
    this.state = init.state ?? null;
  }
}

function makeURL(host: URLHost) {
  class URL {
    #parts: ParsedURL;
    constructor(input: string, base?: string) {
      this.#parts = URL.#parse(String(input), base === undefined ? undefined : String(base));
    }
    static #parse(input: string, base?: string): ParsedURL {
      const parts = host.parseURL(input, base);
      if (parts === undefined) throw new TypeError("Invalid URL");
      return parts;
    }
    static canParse(input: string, base?: string): boolean {
      return host.parseURL(String(input), base === undefined ? undefined : String(base)) !== undefined;
    }
    static parse(input: string, base?: string): URL | null {
      return URL.canParse(input, base) ? new URL(input, base) : null;
    }
    get href(): string { return this.#parts.href; }
    set href(value: string) { this.#parts = URL.#parse(String(value)); }
    get protocol(): string { return this.#parts.protocol; }
    set protocol(value: string) { this.#set("protocol", value); }
    get origin(): string { return this.#parts.origin; }
    get username(): string { return this.#parts.username; }
    set username(value: string) { this.#set("username", value); }
    get password(): string { return this.#parts.password; }
    set password(value: string) { this.#set("password", value); }
    get hostname(): string { return this.#parts.hostname; }
    set hostname(value: string) { this.#set("hostname", value); }
    get host(): string { return this.#parts.host; }
    set host(value: string) { this.#set("host", value); }
    get port(): string { return this.#parts.port; }
    set port(value: string) { this.#set("port", value); }
    get pathname(): string { return this.#parts.pathname; }
    set pathname(value: string) { this.#set("pathname", value); }
    get search(): string { return this.#parts.search; }
    set search(value: string) { this.#set("search", value); }
    get hash(): string { return this.#parts.hash; }
    set hash(value: string) { this.#set("hash", value); }
    #set(part: MutableURLPart, value: string): void {
      this.#parts = host.updateURL(this.href, part, String(value));
    }
    #params = new URLSearchParams("", search => { this.search = search; }, () => this.search);
    get searchParams(): URLSearchParams { return this.#params; }
    toString(): string { return this.href; }
    toJSON(): string { return this.href; }
  }
  return URL;
}

function withSearch(url: { href: string; hash: string }, value: string): string {
  value = String(value);
  const search = value ? "?" + value.replace(/^\?/, "").replace(/#/g, "%23") : "";
  return url.href.replace(/[?#].*$/, "") + search + url.hash;
}

function withHash(href: string, value: string): string {
  value = String(value);
  return href.replace(/#.*$/, "") + (value ? "#" + value.replace(/^#/, "") : "");
}

type Entry = { url: ParsedURL; state: unknown };

export function installNavigation(global: Record<PropertyKey, unknown>, document: Document, host: Host): void {
  const start = host.parseURL(document.URL);
  if (start === undefined) throw new TypeError("Invalid document URL");
  const view = document.defaultView as EventTarget;
  const entries: Entry[] = [{ url: start, state: null }];
  let index = 0;
  const current = () => entries[index];
  const secret = Symbol("navigation");

  function resolve(value: string): ParsedURL {
    const base = current().url;
    const url = host.parseURL(value, base.href);
    if (url === undefined || url.origin !== base.origin || url.username !== base.username || url.password !== base.password) {
      throw new DOMException("Navigation requires a same-origin URL", "SecurityError");
    }
    return url;
  }
  function set(entry: Entry, replace: boolean): void {
    if (replace) entries[index] = entry;
    else {
      entries.splice(index + 1, entries.length, entry);
      index++;
    }
    setDocumentURL(document, entry.url.href);
  }
  function isHashChange(old: ParsedURL, url: ParsedURL): boolean {
    return old.hash !== url.hash && old.href.split("#", 1)[0] === url.href.split("#", 1)[0];
  }
  function hashChanged(old: ParsedURL, url: ParsedURL): void {
    if (isHashChange(old, url)) {
      dispatchTrustedEvent(view, new HashChangeEvent("hashchange", { oldURL: old.href, newURL: url.href }));
    }
  }
  function navigate(value: string, replace: boolean): void {
    const old = current().url;
    const url = resolve(value);
    if (old.href === url.href) return;
    set({ url, state: null }, replace);
    host.postTask(() => hashChanged(old, url));
    if (isHashChange(old, url)) {
      dispatchTrustedEvent(view, new PopStateEvent("popstate"));
    }
  }
  function clone(value: unknown): unknown {
    try { return host.cloneMessage(value); }
    catch (error) {
      if ((error as Error).name === "DataCloneError") throw new DOMException((error as Error).message, "DataCloneError");
      throw error;
    }
  }
  function reload(): void { host.navigationWarning("native document reload is unavailable"); }

  class Location {
    constructor(token: typeof secret) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get href(): string { return current().url.href; }
    set href(value: string) { this.assign(value); }
    get origin(): string { return current().url.origin; }
    get protocol(): string { return current().url.protocol; }
    get host(): string { return current().url.host; }
    get hostname(): string { return current().url.hostname; }
    get port(): string { return current().url.port; }
    get pathname(): string { return current().url.pathname; }
    get search(): string { return current().url.search; }
    set search(value: string) { this.assign(withSearch(this, value)); }
    get hash(): string { return current().url.hash; }
    set hash(value: string) { this.assign(withHash(this.href, value)); }
    assign(value: string): void { navigate(String(value), false); }
    replace(value: string): void { navigate(String(value), true); }
    reload(): void { reload(); }
    toString(): string { return this.href; }
  }

  function writeHistory(data: unknown, url: string | null | undefined, replace: boolean): void {
    const state = clone(data);
    set({ state, url: url == null || url === "" ? current().url : resolve(String(url)) }, replace);
  }

  class History {
    constructor(token: typeof secret) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get state(): unknown { return current().state; }
    get length(): number { return entries.length; }
    pushState(data: unknown, _unused: string, url?: string | null): void { writeHistory(data, url, false); }
    replaceState(data: unknown, _unused: string, url?: string | null): void { writeHistory(data, url, true); }
    back(): void { this.go(-1); }
    forward(): void { this.go(1); }
    go(delta = 0): void {
      delta = Number(delta) | 0;
      if (delta === 0) {
        reload();
        return;
      }
      host.postTask(() => {
        const next = index + delta;
        if (next < 0 || next >= entries.length) return;
        const old = current().url;
        index = next;
        const { url, state } = current();
        setDocumentURL(document, url.href);
        dispatchTrustedEvent(view, new PopStateEvent("popstate", { state }));
        hashChanged(old, url);
      });
    }
  }

  function follow(value: string, blank: boolean): void {
    const base = current().url;
    const url = host.parseURL(value, base.href);
    if (url === undefined) return;
    if (url.origin === base.origin && (url.protocol === "http:" || url.protocol === "https:") && !blank) {
      navigate(url.href, false);
    } else if (["http:", "https:", "mailto:", "tel:"].includes(url.protocol)) {
      openExternal(document, url.href);
    }
  }
  installLinkNavigation(document, follow);
  global.open = (value = "", target = "_blank") => {
    if (String(value) !== "") follow(String(value), String(target).toLowerCase() !== "_self");
    return null;
  };

  const location = new Location(secret);
  Object.assign(global, { Location, History, history: new History(secret), HashChangeEvent, PopStateEvent, URLSearchParams, URL: makeURL(host) });
  Object.defineProperty(global, "location", {
    configurable: true, enumerable: true, get: () => location, set: value => location.assign(value),
  });
  Object.defineProperty(document, "location", {
    configurable: true, get: () => location, set: value => location.assign(value),
  });
  for (const type of ["hashchange", "popstate"]) reflectedEvent(global, type);
}

function decodeParameter(input: string): string {
  return input.replace(/\+/g, " ").replace(/(?:%[\da-f]{2})+/gi, group => {
    try { return decodeURIComponent(group); }
    catch { return "\ufffd"; }
  });
}

function encodeParameter(input: string): string {
  return encodeURIComponent(input.toWellFormed()).replace(/[!'()~]/g, char => "%" + char.charCodeAt(0).toString(16).toUpperCase()).replace(/%20/g, "+");
}

class URLSearchParams {
  #pairs: [string, string][];
  #write?: (value: string) => void;
  #read?: () => string;
  constructor(input: string | Iterable<Iterable<string>> | Record<string, string> = "", write?: (value: string) => void, read?: () => string) {
    this.#write = write;
    this.#read = read;
    if (typeof input === "string") this.#pairs = URLSearchParams.#decode(input);
    else if (Symbol.iterator in input) {
      this.#pairs = Array.from(input as Iterable<Iterable<string>>, pair => {
        const names = Array.from(pair, String);
        if (names.length !== 2) throw new TypeError("Expected parameter pair");
        return [names[0], names[1]];
      });
    } else this.#pairs = Object.entries(input).map(([key, value]) => [key, String(value)]);
  }
  static #decode(input: string): [string, string][] {
    return input.replace(/^\?/, "").split("&").filter(Boolean).map(part => {
      const split = part.indexOf("=");
      return split === -1 ? [decodeParameter(part), ""]
        : [decodeParameter(part.slice(0, split)), decodeParameter(part.slice(split + 1))];
    });
  }
  #current(): [string, string][] { return this.#read === undefined ? this.#pairs : URLSearchParams.#decode(this.#read()); }
  #replace(pairs: [string, string][]): void {
    this.#pairs = pairs;
    this.#write?.(URLSearchParams.#encode(pairs));
  }
  static #encode(pairs: [string, string][]): string {
    return pairs.map(([key, value]) => encodeParameter(key) + "=" + encodeParameter(value)).join("&");
  }
  get size(): number { return this.#current().length; }
  append(name: string, value: string): void { this.#replace([...this.#current(), [String(name), String(value)]]); }
  delete(name: string, value?: string): void {
    const key = String(name), match = value === undefined ? undefined : String(value);
    this.#replace(this.#current().filter(([key2, value2]) => key2 !== key || match !== undefined && value2 !== match));
  }
  get(name: string): string | null { return this.getAll(name)[0] ?? null; }
  getAll(name: string): string[] {
    const key = String(name);
    return this.#current().filter(([key2]) => key2 === key).map(([, value]) => value);
  }
  has(name: string, value?: string): boolean {
    const values = this.getAll(name);
    return value === undefined ? values.length > 0 : values.includes(String(value));
  }
  set(name: string, value: string): void {
    const key = String(name), replacement: [string, string] = [key, String(value)];
    const pairs = this.#current();
    const index = pairs.findIndex(([key2]) => key2 === key);
    if (index === -1) pairs.push(replacement);
    else pairs.splice(index, 1, replacement);
    this.#replace(pairs.filter(([key2], i) => key2 !== key || i === (index === -1 ? pairs.length - 1 : index)));
  }
  sort(): void { this.#replace([...this.#current()].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)); }
  *entries(): IterableIterator<[string, string]> {
    for (let i = 0; i < this.size; i++) yield [...this.#current()[i]];
  }
  *keys(): IterableIterator<string> { for (const [key] of this) yield key; }
  *values(): IterableIterator<string> { for (const [, value] of this) yield value; }
  [Symbol.iterator](): IterableIterator<[string, string]> { return this.entries(); }
  forEach(callback: (value: string, key: string, params: this) => void, thisArg?: unknown): void {
    for (const [key, value] of this) callback.call(thisArg, value, key, this);
  }
  toString(): string { return URLSearchParams.#encode(this.#current()); }
}
