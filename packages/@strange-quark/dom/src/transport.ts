// SPDX-License-Identifier: MIT

// fetch, WebSocket and EventSource over the host's network packets. They need
// no document, so a worker realm installs them too.

import { AbortController } from "./abort.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";
import { blobBytes, createFetch, Headers, token } from "./fetch.js";
import type { BodyInit, Fetch, FetchHost, NetworkPacket, Receiver } from "./fetch.js";
import { reflectedEvent } from "./handlers.js";
import { MessageEvent } from "./timing.js";

export type TransportHost = FetchHost & {
  socketOpen(id: number, url: string, protocols: string[]): void;
  socketSend(id: number, data: string | Uint8Array): void;
  socketClose(id: number, code: number, reason: string): void;
};
type Host = TransportHost;

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

/**
 * Installs fetch, Headers, Request, Response, Blob, File, WebSocket, CloseEvent
 * and EventSource. Relative URLs resolve against `base()`.
 */
export function installTransport<T extends TransportHost>(global: Record<PropertyKey, unknown>, bindings: T, base: () => string) {
  const host: T = {
    ...bindings,
    parseURL: (input: string, url?: string | null) => bindings.parseURL(input, url === null ? undefined : url ?? base()),
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
  Object.assign(global, { fetch, Headers, Request, Response, Blob, File, WebSocket, CloseEvent, EventSource });
  return {
    host,
    fetch,
    dispatch([id, kind, payload]: NetworkPacket): void { listeners.get(id)?.(kind, payload); },
    async fetchModule(url: string): Promise<[string, string]> {
      const response = await fetch(url, { redirect: "error" });
      if (!response.ok) throw new TypeError(`Could not load module ${url} (HTTP ${response.status})`);
      return [response.headers.get("content-type") ?? "", await response.text()];
    },
  };
}
