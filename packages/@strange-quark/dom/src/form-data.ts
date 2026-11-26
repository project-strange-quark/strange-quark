// SPDX-License-Identifier: MIT

import { formDataEntries, HTMLFormElement } from "./document.js";
import type { HTMLButtonElement, HTMLInputElement } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event } from "./events.js";
import { blobBytes } from "./fetch.js";
import type { BodyInit, NetworkBlob } from "./fetch.js";
import type { DOMFile } from "./file-list.js";

type Entry = [string, string | DOMFile];

export function installFormData(global: Record<PropertyKey, unknown>): void {
  const File = global.File as new (parts: BodyInit[], name: string, options?: { type?: string; lastModified?: number }) => DOMFile;
  function valueOf(value: string | NetworkBlob, filename?: string): string | DOMFile {
    if (blobBytes(value) === undefined) {
      if (filename !== undefined) throw new TypeError("A filename requires a Blob");
      return String(value).toWellFormed();
    }
    const blob = value as NetworkBlob;
    if (blob instanceof File && filename === undefined) return blob;
    const name = filename === undefined ? "blob" : String(filename);
    const lastModified = blob instanceof File ? blob.lastModified : undefined;
    return new File([blob], name, { type: blob.type, lastModified });
  }

  const constructing = new WeakSet<HTMLFormElement>();
  class FormData {
    #entries: Entry[];

    constructor(form?: HTMLFormElement, submitter?: HTMLInputElement | HTMLButtonElement | null) {
      if (form === undefined) {
        this.#entries = [];
        return;
      }
      if (!(form instanceof HTMLFormElement)) throw new TypeError("Expected an HTMLFormElement");
      if (constructing.has(form)) throw new DOMException("The form is constructing an entry list", "InvalidStateError");
      constructing.add(form);
      try {
        this.#entries = formDataEntries(form, submitter ?? null,
          () => new File([], "", { type: "application/octet-stream" }));
        dispatchTrustedEvent(form, new FormDataEvent("formdata", { bubbles: true, formData: this }));
      } finally {
        constructing.delete(form);
      }
    }
    append(name: string, value: string | NetworkBlob, filename?: string): void {
      this.#entries.push([String(name).toWellFormed(), valueOf(value, filename)]);
    }
    delete(name: string): void {
      const key = String(name).toWellFormed();
      this.#entries = this.#entries.filter(([name]) => name !== key);
    }
    get(name: string): string | DOMFile | null {
      const key = String(name).toWellFormed();
      return this.#entries.find(([name]) => name === key)?.[1] ?? null;
    }
    getAll(name: string): (string | DOMFile)[] {
      const key = String(name).toWellFormed();
      return this.#entries.filter(([name]) => name === key).map(([, value]) => value);
    }
    has(name: string): boolean { return this.get(name) !== null; }
    set(name: string, value: string | NetworkBlob, filename?: string): void {
      const key = String(name).toWellFormed();
      const index = this.#entries.findIndex(([name]) => name === key);
      const entry: Entry = [key, valueOf(value, filename)];
      if (index < 0) this.#entries.push(entry);
      else {
        this.#entries[index] = entry;
        this.#entries = this.#entries.filter(([name], i) => name !== key || i === index);
      }
    }
    *entries(): IterableIterator<Entry> {
      for (let i = 0; i < this.#entries.length; i++) yield [...this.#entries[i]];
    }
    *keys(): IterableIterator<string> { for (const [name] of this) yield name; }
    *values(): IterableIterator<string | DOMFile> { for (const [, value] of this) yield value; }
    [Symbol.iterator](): IterableIterator<Entry> { return this.entries(); }
    forEach(callback: (value: string | DOMFile, name: string, data: FormData) => void, receiver?: unknown): void {
      for (const [name, value] of this) callback.call(receiver, value, name, this);
    }
    get [Symbol.toStringTag](): string { return "FormData"; }
  }

  class FormDataEvent extends Event {
    readonly formData: FormData;
    constructor(type: string, init: ConstructorParameters<typeof Event>[1] & { formData: FormData }) {
      super(type, init);
      if (!(init?.formData instanceof FormData)) throw new TypeError("Expected a FormData");
      this.formData = init.formData;
    }
  }
  Object.assign(global, { FormData, FormDataEvent });
}
