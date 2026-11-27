// SPDX-License-Identifier: MIT

import { Document, installLinkNavigation, openExternal, reflectedEvent, setDocumentURL } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";
import { makeURL, URLSearchParams } from "./url.js";
import type { ParsedURL, URLHost } from "./url.js";

export type { ParsedURL };

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
