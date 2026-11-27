// SPDX-License-Identifier: MIT

import { connectedInputTarget, Document, HTMLScriptElement, installScriptLoader } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event } from "./events.js";
import { installTransport } from "./transport.js";
import type { TransportHost } from "./transport.js";

type Host = TransportHost & {
  runClassicScript(source: string, url: string): void;
  storageEntries(): [string, string][] | null | undefined;
  storageCommit(key: string | null, value: string | null): boolean;
  storageFlush(): boolean;
};

function createStorage(host: Host) {
  const token = Symbol("Storage");
  const state = new WeakMap<object, {
    entries: () => [string, string][];
    commit: (key: string | null, value: string | null) => void;
  }>();
  function memory() {
    const data = new Map<string, string>();
    return {
      entries: () => [...data],
      commit(key: string | null, value: string | null) {
        if (key === null) data.clear();
        else if (value === null) data.delete(key);
        else data.set(key, value);
      },
    };
  }
  const persistent = host.storageEntries() === null ? memory() : {
    entries(): [string, string][] {
      const values = host.storageEntries();
      if (values == null) throw new DOMException("Could not read localStorage", "SecurityError");
      return values;
    },
    commit(key: string | null, value: string | null) {
      if (!host.storageCommit(key, value)) throw new DOMException("Could not write localStorage", "QuotaExceededError");
    },
  };

  class Storage {
    constructor(secret: typeof token) {
      if (secret !== token) throw new TypeError("Illegal constructor");
    }
    get length(): number { return state.get(this)!.entries().length; }
    key(index: number): string | null { return state.get(this)!.entries()[Number(index) >>> 0]?.[0] ?? null; }
    getItem(key: string): string | null {
      key = String(key);
      return state.get(this)!.entries().find(([name]) => key === name)?.[1] ?? null;
    }
    setItem(key: string, value: string): void { state.get(this)!.commit(String(key), String(value)); }
    removeItem(key: string): void { state.get(this)!.commit(String(key), null); }
    clear(): void { state.get(this)!.commit(null, null); }
  }

  const handler: ProxyHandler<Storage> = {
    get(target, key, receiver) {
      if (typeof key !== "string" || Reflect.has(target, key)) return Reflect.get(target, key, receiver);
      return target.getItem(key) ?? undefined;
    },
    has(target, key) {
      return Reflect.has(target, key) || typeof key === "string" && target.getItem(key) !== null;
    },
    set(target, key, value, receiver) {
      if (typeof key !== "string") return Reflect.set(target, key, value, receiver);
      target.setItem(key, value);
      return true;
    },
    deleteProperty(target, key) {
      if (typeof key === "string") target.removeItem(key);
      else Reflect.deleteProperty(target, key);
      return true;
    },
    ownKeys(target) {
      return [...Reflect.ownKeys(target), ...state.get(target)!.entries().map(([key]) => key).filter(key => !Reflect.has(target, key))];
    },
    getOwnPropertyDescriptor(target, key) {
      const direct = Reflect.getOwnPropertyDescriptor(target, key);
      if (direct !== undefined || typeof key !== "string" || Reflect.has(target, key)) return direct;
      const value = target.getItem(key);
      if (value !== null) return { configurable: true, enumerable: true, writable: true, value };
    },
  };
  function storage(backend: ReturnType<typeof memory>): Storage {
    const target = new Storage(token);
    const proxy = new Proxy(target, handler);
    state.set(target, backend);
    state.set(proxy, backend);
    return proxy;
  }
  return { Storage, localStorage: storage(persistent), sessionStorage: storage(memory()) };
}

export function installNetworking(global: Record<PropertyKey, unknown>, document: Document, bindings: Host) {
  const { host, fetch, dispatch, fetchModule } = installTransport(global, bindings, () => document.URL);
  Object.assign(global, createStorage(host));

  async function runScript(script: HTMLScriptElement, src: string | null, module: boolean): Promise<void> {
    let sourceURL = document.URL;
    let code: string;
    try {
      if (module || script.getAttribute("integrity")) throw new TypeError("Unsupported script");
      if (src === null) code = script.textContent ?? "";
      else {
        if (src === "") throw new TypeError("Empty script URL");
        const response = await fetch(src);
        if (!response.ok) throw new TypeError("Script request failed");
        sourceURL = response.url;
        code = await response.text();
      }
    } catch {
      dispatchTrustedEvent(script, new Event("error"));
      return;
    }
    try { host.runClassicScript(code, sourceURL); }
    catch (error) { (global.reportError as (error: unknown) => void)(error); }
    if (src !== null) dispatchTrustedEvent(script, new Event("load"));
  }

  const executed = new WeakSet<HTMLScriptElement>();
  installScriptLoader(document, script => {
    const type = (script.getAttribute("type") ?? "").trim().toLowerCase();
    if (type && !["text/javascript", "application/javascript", "module"].includes(type)) return;
    if (executed.has(script)) return;
    const src = script.getAttribute("src");
    if (src === null && !script.textContent) return;
    executed.add(script);
    void runScript(script, src, type === "module");
  });
  return {
    dispatch,
    flushLocalStorage(): boolean { return host.storageFlush(); },
    fetchModule,
    async loadScript(id: number, src: string, module: boolean): Promise<void> {
      const script = connectedInputTarget(document, id);
      if (script instanceof HTMLScriptElement) {
        executed.add(script);
        await runScript(script, src, module);
      }
    },
  };
}
