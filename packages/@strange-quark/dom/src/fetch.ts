// SPDX-License-Identifier: MIT

import { AbortController, AbortSignal } from "./abort.js";
import type { ParsedURL } from "./navigation.js";

export type HeaderPairs = [string, string][];
type HeadersInit = Headers | Iterable<Iterable<string>> | Record<string, string>;
export type BodyInit = string | ArrayBuffer | ArrayBufferView | NetworkBlob;
export type NetworkBlob = {
  readonly type: string;
  readonly size: number;
  bytes(): Promise<Uint8Array<ArrayBuffer>>;
  arrayBuffer(): Promise<ArrayBuffer>;
  text(): Promise<string>;
};
type BlobMetadata = [bytes: Uint8Array<ArrayBuffer>, mime: string, name?: string, modified?: number];
const blobData = new WeakMap<object, BlobMetadata>();
let nativeBlobMetadata: ((object: object) => BlobMetadata | undefined) | undefined;

function blobInfo(value: unknown): BlobMetadata | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const metadata = blobData.get(value) ?? nativeBlobMetadata?.(value);
  if (metadata !== undefined) blobData.set(value, metadata);
  return metadata;
}

export function blobBytes(value: unknown): Uint8Array<ArrayBuffer> | undefined {
  return blobInfo(value)?.[0];
}

export const token = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+(?![\s\S])/;

function headerName(value: unknown): string {
  const name = String(value);
  if (!token.test(name)) throw new TypeError("Invalid header name");
  return name.toLowerCase();
}

function headerValue(value: unknown): string {
  const text = String(value).replace(/^[\t ]+|[\t ]+$/g, "");
  if (/[\0\r\n\u0100-\uffff]/.test(text)) throw new TypeError("Invalid header value");
  return text;
}

export class Headers {
  #values = new Map<string, string[]>();
  #immutable = false;

  constructor(init?: HeadersInit) {
    if (init === undefined) return;
    if (typeof init !== "object" || init === null) throw new TypeError("Invalid headers");
    if (init instanceof Headers) {
      for (const [name, values] of init.#values) this.#values.set(name, [...values]);
    } else if (Symbol.iterator in init) {
      for (const pair of init as Iterable<Iterable<string>>) {
        const values = [...pair];
        if (values.length !== 2) throw new TypeError("A header must have two items");
        this.append(values[0], values[1]);
      }
    } else {
      for (const [name, value] of Object.entries(init)) this.append(name, value);
    }
  }

  static immutable(init?: HeadersInit): Headers {
    const headers = new Headers(init);
    headers.#immutable = true;
    return headers;
  }

  #check(): void {
    if (this.#immutable) throw new TypeError("Headers are immutable");
  }
  append(name: string, value: string): void {
    const key = headerName(name), text = headerValue(value);
    this.#check();
    const old = this.#values.get(key);
    if (old === undefined) this.#values.set(key, [text]);
    else old.push(text);
  }
  set(name: string, value: string): void {
    const key = headerName(name), text = headerValue(value);
    this.#check();
    this.#values.set(key, [text]);
  }
  delete(name: string): void {
    const key = headerName(name);
    this.#check();
    this.#values.delete(key);
  }
  has(name: string): boolean { return this.#values.has(headerName(name)); }
  get(name: string): string | null { return this.#values.get(headerName(name))?.join(", ") ?? null; }
  getSetCookie(): string[] { return [...(this.#values.get("set-cookie") ?? [])]; }

  *entries(): IterableIterator<[string, string]> {
    for (const key of [...this.#values.keys()].sort()) {
      const values = this.#values.get(key)!;
      if (key === "set-cookie") {
        for (const value of values) yield [key, value];
      } else {
        yield [key, values.join(", ")];
      }
    }
  }
  *keys(): IterableIterator<string> { for (const [name] of this) yield name; }
  *values(): IterableIterator<string> { for (const [, value] of this) yield value; }
  [Symbol.iterator](): IterableIterator<[string, string]> { return this.entries(); }
  forEach(callback: (value: string, key: string, headers: this) => void, thisArg?: unknown): void {
    for (const [name, value] of this) callback.call(thisArg, value, name, this);
  }
}

type RedirectMode = "follow" | "error" | "manual";
export type FetchHost = {
  parseURL(input: string, base?: string | null): ParsedURL | undefined;
  utf8Encode(input: string): Uint8Array<ArrayBuffer>;
  utf8Decode(input: Uint8Array): string;
  markBlob?(object: object, bytes: Uint8Array, mime: string, name?: string, modified?: number): void;
  blobMetadata?(object: object): BlobMetadata | undefined;
  fetchStart(id: number, url: string, method: string, headers: HeaderPairs, body: Uint8Array | undefined, redirect: RedirectMode): void;
  fetchCredit(id: number, bytes: number): void;
  fetchCancel(id: number): void;
};

type Read = IteratorResult<Uint8Array<ArrayBuffer>, undefined>;
type Pull = () => Promise<Read>;
const end = (): Read => ({ done: true, value: undefined });

function buffered(data: Uint8Array<ArrayBuffer>): Pull {
  let pending: Read = data.length === 0 ? end() : { done: false, value: data };
  return async () => {
    const chunk = pending;
    pending = end();
    return chunk;
  };
}

class BodyStream {
  locked = false;
  used = false;
  #pull: Pull;
  #cancel: (reason?: unknown) => void;
  #chain = Promise.resolve<unknown>(undefined);
  #done = false;

  constructor(pull: Pull, cancel: (reason?: unknown) => void) {
    this.#pull = pull;
    this.#cancel = cancel;
  }

  #read(): Promise<Read> {
    this.used = true;
    const read = this.#chain.then(async () => {
      const result = this.#done ? end() : await this.#pull();
      this.#done = Boolean(result.done);
      return result;
    });
    this.#chain = read.catch(() => {});
    return read;
  }
  getReader() {
    if (this.locked) throw new TypeError("Body stream is locked");
    this.locked = true;
    let active = true;
    return {
      read: () => active ? this.#read() : Promise.reject(new TypeError("Reader released")),
      cancel: async (reason?: unknown) => {
        if (!active) throw new TypeError("Reader released");
        this.#stop(reason);
      },
      releaseLock: () => {
        if (active) this.locked = false;
        active = false;
      },
    };
  }
  #stop(reason: unknown): void {
    this.used = this.#done = true;
    this.#cancel(reason);
  }
  async cancel(reason?: unknown): Promise<void> {
    if (this.locked) throw new TypeError("Body stream is locked");
    this.#stop(reason);
  }
  async *[Symbol.asyncIterator](): AsyncIterableIterator<Uint8Array<ArrayBuffer>> {
    const reader = this.getReader();
    let done = false;
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) {
          done = true;
          return;
        }
        yield chunk.value;
      }
    } finally {
      if (!done) await reader.cancel();
      reader.releaseLock();
    }
  }
  clone(): BodyStream {
    if (this.used || this.locked) throw new TypeError("Body stream was already used");
    const pull = this.#pull, cancel = this.#cancel;
    const queues: [Read[], Read[]] = [[], []];
    const cancelled = [false, false];
    let chain = Promise.resolve<unknown>(undefined);
    const read = (branch: 0 | 1) => {
      const other = branch === 0 ? 1 : 0;
      const pending = chain.then(async () => {
        const queued = queues[branch].shift();
        if (queued !== undefined) return queued;
        const item = await pull();
        if (!cancelled[other]) queues[other].push(item.done ? item : { done: false, value: item.value.slice() });
        return item;
      });
      chain = pending.catch(() => {});
      return pending;
    };
    const stop = (branch: number, reason: unknown) => {
      cancelled[branch] = true;
      queues[branch].length = 0;
      if (cancelled.every(Boolean)) cancel(reason);
    };
    this.#pull = () => read(0);
    this.#cancel = reason => stop(0, reason);
    return new BodyStream(() => read(1), reason => stop(1, reason));
  }
}

function encode(host: FetchHost, value: BodyInit | null | undefined): Uint8Array<ArrayBuffer> | null {
  if (value == null) return null;
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (ArrayBuffer.isView(value)) return new Uint8Array(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
  return blobBytes(value)?.slice() ?? host.utf8Encode(String(value));
}

function combine(chunks: Uint8Array[], length: number): Uint8Array<ArrayBuffer> {
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

type BlobOptions = { type?: string; endings?: "transparent" | "native" };

function createBlob(host: FetchHost) {
  nativeBlobMetadata = host.blobMetadata;
  class Blob {
    constructor(parts: Iterable<BodyInit> = [], options: BlobOptions = {}) {
      if (typeof parts === "string") throw new TypeError("Expected an iterable of Blob parts");
      const endings = options.endings ?? "transparent";
      if (endings !== "transparent" && endings !== "native") throw new TypeError("Invalid Blob endings");
      const bytes = Array.from(parts, part => {
        const input = typeof part === "string" && endings === "native" ? part.replace(/\r\n?/g, "\n") : part;
        return encode(host, input) ?? host.utf8Encode(String(input));
      });
      const buffer = combine(bytes, bytes.reduce((total, part) => total + part.length, 0));
      const type = String(options.type ?? "");
      const mime = /^[\x20-\x7e]*$(?![\s\S])/.test(type) ? type.toLowerCase() : "";
      blobData.set(this, [buffer, mime]);
      host.markBlob?.(this, buffer, mime);
    }
    get type(): string { return blobInfo(this)![1]; }
    get size(): number { return blobBytes(this)!.length; }
    slice(start?: number, end?: number, type?: string): Blob {
      return new Blob([blobBytes(this)!.subarray(start, end)], { type });
    }
    stream(): BodyStream { return new BodyStream(buffered(blobBytes(this)!.slice()), () => {}); }
    async bytes(): Promise<Uint8Array<ArrayBuffer>> { return blobBytes(this)!.slice(); }
    async arrayBuffer(): Promise<ArrayBuffer> { return (await this.bytes()).buffer; }
    async text(): Promise<string> { return host.utf8Decode(blobBytes(this)!).replace(/^\ufeff/, ""); }
    get [Symbol.toStringTag](): string { return "Blob"; }
  }

  class File extends Blob {
    constructor(parts: Iterable<BodyInit>, name: string, options: BlobOptions & { lastModified?: number } = {}) {
      if (arguments.length < 2 || parts === undefined) throw new TypeError("File requires parts and a name");
      super(parts, options);
      const fileName = String(name).toWellFormed();
      const modified = options.lastModified === undefined ? Date.now() : Number(options.lastModified);
      const time = Number.isFinite(modified) ? Math.trunc(modified) : 0;
      const [bytes, mime] = blobInfo(this)!;
      blobData.set(this, [bytes, mime, fileName, time]);
      host.markBlob?.(this, bytes, mime, fileName, time);
    }
    get name(): string { return blobInfo(this)![2]!; }
    get lastModified(): number { return blobInfo(this)![3]!; }
    get webkitRelativePath(): string { return ""; }
    override get [Symbol.toStringTag](): string { return "File"; }
  }
  return { Blob, File };
}

function bodyType(value: BodyInit | null | undefined): string {
  if (typeof value === "string") return "text/plain;charset=UTF-8";
  return blobBytes(value) === undefined ? "" : (value as NetworkBlob).type;
}

type RequestInit = {
  method?: string; headers?: HeadersInit; body?: BodyInit | null; signal?: AbortSignal | null;
  redirect?: RedirectMode; credentials?: "omit" | "same-origin" | "include";
};
type ResponseInit = { status?: number; statusText?: string; headers?: HeadersInit };
type ResponseData = {
  url: string; status: number; statusText: string; headers: HeaderPairs; redirected: boolean;
};
type Packet = [number, string, unknown];
export type Receiver = (kind: string, payload: unknown) => void;

export type FetchResponse = {
  readonly url: string;
  readonly status: number;
  readonly ok: boolean;
  readonly headers: Headers;
  readonly body: BodyStream | null;
  text(): Promise<string>;
};
type FetchRequest = { readonly url: string };
export type Fetch = (input: string | { toString(): string } | FetchRequest, init?: RequestInit) => Promise<FetchResponse>;
type FetchAPI = {
  Request: new (input: string | { toString(): string }, init?: RequestInit) => FetchRequest;
  Response: new (value?: BodyInit | null, init?: ResponseInit) => FetchResponse;
  Blob: new (parts?: Iterable<BodyInit>, options?: BlobOptions) => NetworkBlob;
  File: new (parts: Iterable<BodyInit>, name: string, options?: BlobOptions & { lastModified?: number }) => NetworkBlob;
  fetch: Fetch;
};

export function createFetch(host: FetchHost, subscribe: (fn: Receiver) => number, release: (id: number) => void): FetchAPI {
  const { Blob, File } = createBlob(host);
  abstract class Body {
    abstract readonly headers: Headers;
    #body: BodyStream | null;
    constructor(body: BodyStream | null) { this.#body = body; }
    get body(): BodyStream | null { return this.#body; }
    get bodyUsed(): boolean { return this.#body?.used ?? false; }
    async bytes(): Promise<Uint8Array<ArrayBuffer>> {
      if (this.#body === null) return new Uint8Array();
      if (this.bodyUsed || this.#body.locked) throw new TypeError("Body stream was already used");
      let length = 0;
      const chunks = [];
      for await (const chunk of this.#body) {
        chunks.push(chunk);
        length += chunk.length;
      }
      return combine(chunks, length);
    }
    async arrayBuffer(): Promise<ArrayBuffer> { return (await this.bytes()).buffer; }
    async text(): Promise<string> { return host.utf8Decode(await this.bytes()).replace(/^\ufeff/, ""); }
    async json(): Promise<unknown> { return JSON.parse(await this.text()); }
    async blob(): Promise<NetworkBlob> {
      return new Blob([await this.bytes()], { type: this.headers.get("content-type") ?? "" });
    }
  }

  const requests = new WeakMap<Request, Uint8Array<ArrayBuffer> | null>();
  const requestClone = Symbol("clone Request");
  class Request extends Body {
    readonly url: string;
    readonly method: string;
    readonly headers: Headers;
    readonly signal: AbortSignal;
    readonly redirect: RedirectMode;
    readonly credentials: NonNullable<RequestInit["credentials"]>;

    constructor(input: string | { toString(): string } | Request, init: RequestInit = {}, clone?: typeof requestClone) {
      const original = input instanceof Request ? input : undefined;
      const url = host.parseURL(original?.url ?? String(input));
      if (url === undefined || url.username || url.password) throw new TypeError("Invalid request URL");
      let method = String(init.method ?? original?.method ?? "GET");
      const upper = method.toUpperCase();
      if (!token.test(method) || ["CONNECT", "TRACE", "TRACK"].includes(upper)) throw new TypeError("Invalid request method");
      if (["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].includes(upper)) method = upper;
      const inherited = original !== undefined && init.body == null;
      if (inherited && (original.bodyUsed || original.body?.locked)) throw new TypeError("Request body was already used");
      const bytes = inherited ? requests.get(original)! : encode(host, init.body);
      if (bytes !== null && (method === "GET" || method === "HEAD")) throw new TypeError("GET and HEAD cannot have a body");
      const redirect = init.redirect ?? original?.redirect ?? "follow";
      if (!["follow", "manual", "error"].includes(redirect)) throw new TypeError("Invalid redirect mode");
      const credentials = init.credentials ?? original?.credentials ?? "same-origin";
      if (!["omit", "same-origin", "include"].includes(credentials)) throw new TypeError("Invalid credentials mode");
      const signal = init.signal === null ? undefined : init.signal ?? original?.signal;
      if (signal !== undefined && !(signal instanceof AbortSignal)) throw new TypeError("Invalid request signal");
      super(bytes === null ? null : new BodyStream(buffered(bytes.slice()), () => {}));
      requests.set(this, bytes);
      if (inherited && clone !== requestClone && original.body !== null) void original.body.cancel();
      this.url = url.href;
      this.method = method;
      this.headers = new Headers(init.headers ?? original?.headers);
      const contentType = bodyType(init.body);
      if (contentType && !this.headers.has("content-type")) this.headers.set("content-type", contentType);
      this.signal = signal === undefined ? new AbortController().signal : AbortSignal.any([signal]);
      this.redirect = redirect;
      this.credentials = credentials;
    }

    clone(): Request { return new Request(this, {}, requestClone); }
  }

  const internal = Symbol("network Response");
  type InternalResponse = { body: BodyStream | null; data: ResponseData; headers: Headers; type: Response["type"] };
  const responseState = new WeakMap<Response, InternalResponse>();
  class Response extends Body {
    constructor(value?: BodyInit | null, init?: ResponseInit);
    constructor(value: typeof internal, init: InternalResponse);
    constructor(value?: BodyInit | null | typeof internal, init: ResponseInit | InternalResponse = {}) {
      if (value === internal) {
        const state = init as InternalResponse;
        super(state.body);
        responseState.set(this, state);
      } else {
        const options = init as ResponseInit;
        const bytes = encode(host, value);
        const status = Math.trunc(Number(options.status ?? 200));
        if (status < 200 || status > 599 || !Number.isFinite(status)) throw new RangeError("Invalid response status");
        if (bytes !== null && [204, 205, 304].includes(status)) throw new TypeError("Response status cannot have a body");
        const statusText = String(options.statusText ?? "");
        if (!/^[\t\x20-\x7e\x80-\xff]*$/.test(statusText)) throw new TypeError("Invalid response status text");
        const headers = new Headers(options.headers);
        const contentType = bodyType(value);
        if (contentType && !headers.has("content-type")) headers.set("content-type", contentType);
        const body = bytes === null ? null : new BodyStream(buffered(bytes), () => {});
        super(body);
        responseState.set(this, {
          body, headers, type: "default",
          data: { url: "", status, statusText, redirected: false, headers: [] },
        });
      }
    }
    get url(): string { return responseState.get(this)!.data.url; }
    get status(): number { return responseState.get(this)!.data.status; }
    get statusText(): string { return responseState.get(this)!.data.statusText; }
    get redirected(): boolean { return responseState.get(this)!.data.redirected; }
    get type(): "default" | "basic" | "error" | "opaqueredirect" { return responseState.get(this)!.type; }
    get ok(): boolean { return this.status >= 200 && this.status <= 299; }
    get headers(): Headers { return responseState.get(this)!.headers; }

    clone(): Response {
      const state = responseState.get(this)!;
      return new Response(internal, {
        ...state, body: this.body?.clone() ?? null,
        headers: state.type === "default" ? new Headers(state.headers) : Headers.immutable(state.headers),
      });
    }
    static error(): Response {
      return new Response(internal, {
        body: null, headers: Headers.immutable(), type: "error",
        data: { url: "", status: 0, statusText: "", redirected: false, headers: [] },
      });
    }
    static json(value: unknown, init: ResponseInit = {}): Response {
      const body = JSON.stringify(value);
      if (body === undefined) throw new TypeError("Value cannot be serialized as JSON");
      const headers = new Headers(init.headers);
      if (!headers.has("content-type")) headers.set("content-type", "application/json");
      return new Response(body, { ...init, headers });
    }
    static redirect(url: string, status = 302): Response {
      const parsed = host.parseURL(String(url));
      if (parsed === undefined) throw new TypeError("Invalid redirect URL");
      status = Number(status);
      if (![301, 302, 303, 307, 308].includes(status)) throw new RangeError("Invalid redirect status");
      return new Response(internal, {
        body: null, headers: Headers.immutable({ location: parsed.href }), type: "default",
        data: { url: "", status, statusText: "", redirected: false, headers: [] },
      });
    }
  }

  async function fetch(input: string | { toString(): string } | Request, init?: RequestInit): Promise<Response> {
    const request = new Request(input, init);
    request.signal.throwIfAborted();
    return new Promise<Response>((resolve, reject) => {
      let complete = false, failed = false, failure: unknown;
      let reader: ReturnType<typeof Promise.withResolvers<Read>> | undefined;
      const signal = request.signal;
      function stop(): void {
        host.fetchCancel(id);
        release(id);
        signal.removeEventListener("abort", abort);
      }
      function fail(error: unknown): void {
        failed = true;
        failure = error;
        reader?.reject(error);
        reject(error);
        stop();
      }
      function abort(): void { fail(signal.reason); }
      function pull(): Promise<Read> {
        if (failed) return Promise.reject(failure);
        if (complete) return Promise.resolve(end());
        reader = Promise.withResolvers<Read>();
        host.fetchCredit(id, 64 * 1024);
        return reader.promise;
      }
      const id = subscribe((kind, payload) => {
        switch (kind) {
          case "headers": {
            const data = payload as ResponseData;
            const body = request.method === "HEAD" || [0, 204, 205, 304].includes(data.status)
              ? null : new BodyStream(pull, () => {
                complete = true;
                reader?.resolve(end());
                stop();
              });
            resolve(new Response(internal, {
              body, data, headers: Headers.immutable(data.headers),
              type: data.status === 0 ? "opaqueredirect" : "basic",
            }));
            break;
          }
          case "chunk":
            reader?.resolve({ done: false, value: payload as Uint8Array<ArrayBuffer> });
            reader = undefined;
            break;
          case "end":
            complete = true;
            reader?.resolve(end());
            stop();
            break;
          case "error":
            fail(new TypeError("Failed to fetch"));
            break;
        }
      });
      signal.addEventListener("abort", abort, { once: true });
      host.fetchStart(id, request.url, request.method, [...request.headers], requests.get(request) ?? undefined, request.redirect);
    });
  }

  return { Request, Response, Blob, File, fetch };
}

export type NetworkPacket = Packet;
