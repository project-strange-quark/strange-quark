// SPDX-License-Identifier: MIT

import { AbortController } from "./abort.js";
import { connectedInputTarget, Document, HTMLScriptElement, installScriptLoader, reflectedEvent } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";
import { blobBytes, createFetch, Headers, token } from "./fetch.js";
import type { BodyInit, Fetch, FetchHost, NetworkPacket, Receiver } from "./fetch.js";
import { MessageEvent } from "./timing.js";

type Host = FetchHost & {
  socketOpen(id: number, url: string, protocols: string[]): void;
  socketSend(id: number, data: string | Uint8Array): void;
  socketClose(id: number, code: number, reason: string): void;
  runClassicScript(source: string, url: string): void;
  storageEntries(): [string, string][] | null | undefined;
  storageCommit(key: string | null, value: string | null): boolean;
  storageFlush(): boolean;
};

type Handler<T extends Event = Event> = ((event: T) => unknown) | null;

export class CloseEvent extends Event {
  readonly code: number;
  readonly reason: string;
  readonly wasClean: boolean;
  constructor(type: string, init: { code?: number; reason?: string; wasClean?: boolean } = {}) {
    super(type);
    this.code = Number(init.code ?? 0) & 0xffff;
    this.reason = String(init.reason ?? "");
    this.wasClean = Boolean(init.wasClean);
  }
}

const WS_STATES = { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 } as const;
const SSE_STATES = { CONNECTING: 0, OPEN: 1, CLOSED: 2 } as const;

function createWebSocket(host: Host, Blob: ReturnType<typeof createFetch>["Blob"],
    subscribe: (fn: Receiver) => number, release: (id: number) => void) {
  const states = WS_STATES;
  class WebSocket extends EventTarget {
    static readonly CONNECTING = states.CONNECTING;
    static readonly OPEN = states.OPEN;
    static readonly CLOSING = states.CLOSING;
    static readonly CLOSED = states.CLOSED;
    readonly CONNECTING = states.CONNECTING;
    readonly OPEN = states.OPEN;
    readonly CLOSING = states.CLOSING;
    readonly CLOSED = states.CLOSED;
    readonly extensions = "";
    readonly url: string;
    declare onopen: Handler;
    declare onmessage: Handler<MessageEvent>;
    declare onerror: Handler;
    declare onclose: Handler<CloseEvent>;
    #state: number = states.CONNECTING;
    #protocol = "";
    #buffered = 0;
    #binaryType: "arraybuffer" | "blob" = "blob";
    #id: number;

    constructor(input: string, protocols: string | Iterable<string> = []) {
      super();
      const raw = host.parseURL(String(input));
      if (raw === undefined) throw new DOMException("Invalid WebSocket URL", "SyntaxError");
      const url = host.parseURL(raw.href.replace(/^http/, "ws"), null);
      if (url === undefined || !["ws:", "wss:"].includes(url.protocol) ||
          url.href.includes("#") || url.username || url.password) {
        throw new DOMException("Invalid WebSocket URL", "SyntaxError");
      }
      const names = typeof protocols === "string" ? [protocols] : Array.from(protocols, String);
      if (names.some(name => !token.test(name)) || new Set(names).size !== names.length) {
        throw new DOMException("Invalid WebSocket subprotocols", "SyntaxError");
      }
      this.url = url.href;
      this.#id = subscribe((kind, payload) => {
        switch (kind) {
          case "open":
            if (this.#state !== states.CONNECTING) return;
            this.#state = states.OPEN;
            this.#protocol = String(payload);
            dispatchTrustedEvent(this, new Event("open"));
            break;
          case "message":
            if (this.#state !== states.OPEN) return;
            if (payload instanceof Uint8Array) {
              payload = this.#binaryType === "arraybuffer" ? payload.buffer : new Blob([payload]);
            }
            dispatchTrustedEvent(this, new MessageEvent("message", { data: payload, origin: url.origin }));
            break;
          case "sent":
            this.#buffered -= Number(payload);
            break;
          case "error":
            this.#state = states.CLOSED;
            dispatchTrustedEvent(this, new Event("error"));
            break;
          case "close":
            this.#state = states.CLOSED;
            release(this.#id);
            dispatchTrustedEvent(this, new CloseEvent("close", payload as CloseEvent));
            break;
        }
      });
      host.socketOpen(this.#id, url.href, names);
    }

    get readyState(): number { return this.#state; }
    get protocol(): string { return this.#protocol; }
    get bufferedAmount(): number { return this.#buffered; }
    get binaryType(): "blob" | "arraybuffer" { return this.#binaryType; }
    set binaryType(value: string) {
      if (value === "blob" || value === "arraybuffer") this.#binaryType = value;
    }

    send(data: BodyInit): void {
      if (this.#state === states.CONNECTING) throw new DOMException("WebSocket is connecting", "InvalidStateError");
      const value = blobBytes(data) ?? (data instanceof ArrayBuffer ? new Uint8Array(data) : ArrayBuffer.isView(data)
        ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : String(data));
      this.#buffered += typeof value === "string" ? host.utf8Encode(value).length : value.byteLength;
      if (this.#state === states.OPEN) host.socketSend(this.#id, value);
    }

    close(code?: number, reason = ""): void {
      const status = code === undefined ? 1005 : Number(code) & 0xffff;
      if (code !== undefined && status !== 1000 && (status < 3000 || status > 4999)) {
        throw new DOMException("Invalid WebSocket close code", "InvalidAccessError");
      }
      reason = String(reason);
      if (host.utf8Encode(reason).length > 123) throw new DOMException("Invalid WebSocket close reason", "SyntaxError");
      if (this.#state < states.CLOSING) {
        this.#state = states.CLOSING;
        host.socketClose(this.#id, status, reason);
      }
    }
  }
  for (const type of ["open", "message", "error", "close"]) reflectedEvent(WebSocket.prototype, type);
  return WebSocket;
}

class EventStream {
  #line: number[] = [];
  #cr = false;
  #first = true;
  #data: string[] = [];
  #type = "";
  id = "";
  retry = 3000;
  #host: FetchHost;
  #message: (type: string, data: string, id: string) => void;

  constructor(host: FetchHost, message: (type: string, data: string, id: string) => void) {
    this.#host = host;
    this.#message = message;
  }
  push(bytes: Uint8Array): void {
    for (const byte of bytes) {
      if (this.#cr) {
        this.#cr = false;
        if (byte === 10) continue;
      }
      if (byte === 10 || byte === 13) {
        this.#cr = byte === 13;
        this.#process(this.#host.utf8Decode(Uint8Array.from(this.#line)));
        this.#line = [];
      } else {
        this.#line.push(byte);
      }
    }
  }
  #process(text: string): void {
    if (this.#first) {
      this.#first = false;
      text = text.replace(/^\ufeff/, "");
    }
    if (text === "") {
      if (this.#data.length !== 0) this.#message(this.#type || "message", this.#data.join("\n"), this.id);
      this.#data = [];
      this.#type = "";
      return;
    }
    const colon = text.indexOf(":");
    const field = colon === -1 ? text : text.slice(0, colon);
    const value = colon === -1 ? "" : text.slice(colon + 1).replace(/^ /, "");
    switch (field) {
      case "data": this.#data.push(value); break;
      case "event": this.#type = value; break;
      case "id": if (!value.includes("\0")) this.id = value; break;
      case "retry": if (/^\d+$/.test(value)) this.retry = Math.min(Number(value), 0x7fffffff); break;
    }
  }
}

type Timers = {
  setTimeout(callback: () => void, delay: number): number;
  clearTimeout(id: number): void;
};

function createEventSource(host: Host, fetch: Fetch, timers: Timers) {
  const states = SSE_STATES;
  class EventSource extends EventTarget {
    static readonly CONNECTING = states.CONNECTING;
    static readonly OPEN = states.OPEN;
    static readonly CLOSED = states.CLOSED;
    readonly CONNECTING = states.CONNECTING;
    readonly OPEN = states.OPEN;
    readonly CLOSED = states.CLOSED;
    readonly url: string;
    readonly withCredentials: boolean;
    declare onopen: Handler;
    declare onmessage: Handler<MessageEvent>;
    declare onerror: Handler;
    #state: number = states.CONNECTING;
    #controller: AbortController | undefined;
    #timer: number | undefined;
    #retry = 3000;
    #lastId = "";

    constructor(url: string, init: { withCredentials?: boolean } = {}) {
      super();
      const parsed = host.parseURL(String(url));
      if (parsed === undefined) throw new DOMException("Invalid EventSource URL", "SyntaxError");
      this.url = parsed.href;
      this.withCredentials = Boolean(init.withCredentials);
      void this.#connect();
    }
    get readyState(): number { return this.#state; }
    close(): void {
      this.#state = states.CLOSED;
      this.#controller?.abort();
      if (this.#timer !== undefined) timers.clearTimeout(this.#timer);
    }
    #closed(): boolean { return this.#state === states.CLOSED; }
    #fail(retry: boolean): void {
      if (this.#closed()) return;
      if (retry) {
        this.#state = states.CONNECTING;
        this.#timer = timers.setTimeout(() => void this.#connect(), this.#retry);
      } else {
        this.close();
      }
      dispatchTrustedEvent(this, new Event("error"));
    }
    async #connect(): Promise<void> {
      if (this.#closed()) return;
      this.#timer = undefined;
      const controller = new AbortController();
      this.#controller = controller;
      const headers = new Headers({ accept: "text/event-stream" });
      if (this.#lastId) {
        const utf8 = host.utf8Encode(this.#lastId);
        headers.set("last-event-id", Array.from(utf8, byte => String.fromCharCode(byte)).join(""));
      }
      let opened = false;
      try {
        const response = await fetch(this.url, {
          headers, signal: controller.signal, credentials: this.withCredentials ? "include" : "same-origin",
        });
        const mime = response.headers.get("content-type");
        if (response.status !== 200 || mime?.split(";", 1)[0].trim().toLowerCase() !== "text/event-stream" || response.body === null) {
          this.#fail(false);
          return;
        }
        if (this.#closed()) return;
        const origin = host.parseURL(response.url)?.origin ?? "";
        const parser = new EventStream(host, (type, data, lastEventId) => {
          if (!this.#closed()) dispatchTrustedEvent(this, new MessageEvent(type, { data, origin, lastEventId }));
        });
        parser.id = this.#lastId;
        parser.retry = this.#retry;
        this.#state = states.OPEN;
        opened = true;
        dispatchTrustedEvent(this, new Event("open"));
        for await (const chunk of response.body) {
          if (this.#closed()) break;
          parser.push(chunk);
          this.#lastId = parser.id;
          this.#retry = parser.retry;
        }
        this.#fail(true);
      } catch {
        this.#fail(opened);
      }
    }
  }
  for (const type of ["open", "message", "error"]) reflectedEvent(EventSource.prototype, type);
  return EventSource;
}

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
  const host = {
    ...bindings,
    parseURL: (input: string, base?: string | null) => bindings.parseURL(input, base === null ? undefined : base ?? document.URL),
  };
  let nextId = 0;
  const listeners = new Map<number, Receiver>();
  function subscribe(fn: Receiver): number {
    const id = ++nextId;
    if (id > 0xffffffff) throw new RangeError("Network task ID space exhausted");
    listeners.set(id, fn);
    return id;
  }
  const release = (id: number) => { listeners.delete(id); };
  const { Request, Response, Blob, File, fetch } = createFetch(host, subscribe, release);
  const WebSocket = createWebSocket(host, Blob, subscribe, release);
  const EventSource = createEventSource(host, fetch, global as unknown as Timers);
  Object.assign(global, { fetch, Headers, Request, Response, Blob, File, WebSocket, CloseEvent, EventSource, ...createStorage(host) });

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
    dispatch([id, kind, payload]: NetworkPacket): void { listeners.get(id)?.(kind, payload); },
    flushLocalStorage(): boolean { return host.storageFlush(); },
    async fetchModule(url: string): Promise<[string, string]> {
      const response = await fetch(url, { redirect: "error" });
      if (!response.ok) throw new TypeError(`Could not load module ${url} (HTTP ${response.status})`);
      return [response.headers.get("content-type") ?? "", await response.text()];
    },
    async loadScript(id: number, src: string, module: boolean): Promise<void> {
      const script = connectedInputTarget(document, id);
      if (script instanceof HTMLScriptElement) {
        executed.add(script);
        await runScript(script, src, module);
      }
    },
  };
}
