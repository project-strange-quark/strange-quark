// SPDX-License-Identifier: MIT

import { reflectedEvent } from "./document.js";
import { dispatchTrustedEvent, DOMException, EventTarget, ProgressEvent } from "./events.js";
import { blobBytes } from "./fetch.js";
import type { NetworkBlob } from "./fetch.js";

type Host = {
  postTask(callback: () => void): void;
  blobRegister(data: Uint8Array, mime: string): string | undefined;
  blobRevoke(url: string): void;
  base64Encode(data: Uint8Array, url: boolean): string;
  base64Decode(text: string, url: boolean): Uint8Array | null;
  decodeText(data: Uint8Array, encoding: string): string;
  utf8Encode(text: string): Uint8Array<ArrayBuffer>;
};

export function installFiles(global: Record<PropertyKey, unknown>, host: Host): void {
  const URL = global.URL as { createObjectURL?: (blob: NetworkBlob) => string; revokeObjectURL?: (url: string) => void };
  URL.createObjectURL = blob => {
    const bytes = blobBytes(blob);
    if (bytes === undefined) throw new TypeError("Expected a Blob");
    const url = host.blobRegister(bytes, blob.type);
    if (url === undefined) throw new DOMException("Could not register a Blob URL", "OperationError");
    return url;
  };
  URL.revokeObjectURL = url => host.blobRevoke(String(url));

  class FileReader extends EventTarget {
    static readonly EMPTY = 0;
    static readonly LOADING = 1;
    static readonly DONE = 2;
    readonly EMPTY = 0;
    readonly LOADING = 1;
    readonly DONE = 2;
    #state = 0;
    #operation = 0;
    #result: string | ArrayBuffer | null = null;
    #error: DOMException | null = null;

    get readyState(): number { return this.#state; }
    get result(): string | ArrayBuffer | null { return this.#result; }
    get error(): DOMException | null { return this.#error; }

    #event(type: string, loaded?: number, total?: number): void {
      dispatchTrustedEvent(this, new ProgressEvent(type, { loaded, total, lengthComputable: total !== undefined }));
    }

    #read(blob: NetworkBlob, read: (bytes: Uint8Array<ArrayBuffer>) => string | ArrayBuffer): void {
      const bytes = blobBytes(blob);
      if (bytes === undefined) throw new TypeError("Expected a Blob");
      if (this.#state === 1) throw new DOMException("FileReader is already reading", "InvalidStateError");
      const operation = ++this.#operation;
      this.#state = 1;
      this.#result = this.#error = null;
      host.postTask(() => {
        if (this.#operation !== operation) return;
        this.#event("loadstart", 0, bytes.length);
        if (this.#operation !== operation) return;
        this.#event("progress", bytes.length, bytes.length);
        if (this.#operation !== operation) return;
        try {
          this.#result = read(bytes);
        } catch {
          this.#error = new DOMException("Could not read the Blob", "NotReadableError");
        }
        this.#state = 2;
        this.#event(this.#error === null ? "load" : "error", bytes.length, bytes.length);
        if (this.#operation === operation) this.#event("loadend", bytes.length, bytes.length);
      });
    }

    readAsArrayBuffer(blob: NetworkBlob): void { this.#read(blob, bytes => bytes.slice().buffer); }
    readAsText(blob: NetworkBlob, encoding?: string): void {
      const label = encoding === undefined ? /(?:^|;)\s*charset\s*=\s*"?([^;"\s]+)/i.exec(blob?.type)?.[1] ?? "" : String(encoding);
      this.#read(blob, bytes => host.decodeText(bytes, label));
    }
    readAsDataURL(blob: NetworkBlob): void {
      this.#read(blob, bytes => `data:${blob.type || "application/octet-stream"};base64,${host.base64Encode(bytes, false)}`);
    }
    abort(): void {
      this.#result = null;
      if (this.#state !== 1) return;
      this.#state = 2;
      const operation = ++this.#operation;
      this.#event("abort");
      if (this.#operation === operation) this.#event("loadend");
    }
  }
  for (const type of ["loadstart", "progress", "load", "abort", "error", "loadend"]) reflectedEvent(FileReader.prototype, type);

  function atob(text: string): string {
    const input = String(text).replace(/[\t\n\f\r ]/g, "");
    const bytes = /^(?:[A-Za-z0-9+/]*|(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=))$/.test(input)
      ? host.base64Decode(input, false) : null;
    if (bytes === null) throw new DOMException("Invalid Base64", "InvalidCharacterError");
    const chunks = [];
    for (let i = 0; i < bytes.length; i += 8192) chunks.push(String.fromCharCode(...bytes.subarray(i, i + 8192)));
    return chunks.join("");
  }
  function btoa(text: string): string {
    const input = String(text);
    const bytes = new Uint8Array(input.length);
    for (let i = 0; i < input.length; i++) {
      const value = input.charCodeAt(i);
      if (value > 255) throw new DOMException("Input must be Latin-1", "InvalidCharacterError");
      bytes[i] = value;
    }
    return host.base64Encode(bytes, false);
  }
  class TextEncoder {
    readonly encoding = "utf-8";
    encode(input = ""): Uint8Array<ArrayBuffer> {
      return host.utf8Encode(String(input));
    }
    encodeInto(source: string, destination: Uint8Array): { read: number; written: number } {
      if (!(destination instanceof Uint8Array)) throw new TypeError("Expected a Uint8Array");
      const text = String(source);
      let read = 0;
      let written = 0;
      while (read < text.length) {
        const code = text.codePointAt(read)!;
        const step = code > 0xffff ? 2 : 1;
        const encoded = host.utf8Encode(text.slice(read, read + step));
        if (written + encoded.length > destination.length) break;
        destination.set(encoded, written);
        written += encoded.length;
        read += step;
      }
      return { read, written };
    }
  }

  class TextDecoder {
    readonly encoding: string;
    readonly fatal: boolean;
    readonly ignoreBOM: boolean;
    constructor(label = "utf-8", options?: { fatal?: boolean; ignoreBOM?: boolean }) {
      const normalized = String(label).trim().toLowerCase();
      this.encoding = normalized === "" || normalized === "utf8" ? "utf-8" : normalized;
      this.fatal = Boolean(options?.fatal);
      this.ignoreBOM = Boolean(options?.ignoreBOM);
    }
    decode(input?: ArrayBufferView | ArrayBuffer): string {
      if (input === undefined) return "";
      const bytes = ArrayBuffer.isView(input)
        ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength)
        : input instanceof ArrayBuffer
          ? new Uint8Array(input)
          : (() => { throw new TypeError("Expected an ArrayBuffer or ArrayBufferView"); })();
      const decoded = host.decodeText(bytes, this.encoding);
      return this.ignoreBOM ? decoded : decoded.replace(/^\ufeff/, "");
    }
  }

  Object.assign(global, { FileReader, TextEncoder, TextDecoder, atob, btoa });
}
