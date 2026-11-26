// SPDX-License-Identifier: MIT

import { DOMException, Event } from "./events.js";
import { fileList, updateFileList } from "./file-list.js";
import type { DOMFile } from "./file-list.js";
import { blobBytes } from "./fetch.js";
import type { NetworkBlob } from "./fetch.js";

type TransferEntry = { type: string; value: string | DOMFile };
const transferToken = Symbol("DataTransfer item constructor");

function format(type: string): string {
  type = String(type).toLowerCase();
  if (type === "text") return "text/plain";
  if (type === "url") return "text/uri-list";
  return type;
}

export class DataTransferItem {
  #entry: TransferEntry;
  constructor(token: symbol, entry: TransferEntry) {
    if (token !== transferToken) throw new TypeError("Illegal constructor");
    this.#entry = entry;
  }
  get kind(): "string" | "file" { return typeof this.#entry.value === "string" ? "string" : "file"; }
  get type(): string { return this.#entry.type; }
  getAsFile(): DOMFile | null {
    const value = this.#entry.value;
    return typeof value === "string" ? null : value;
  }
  getAsString(callback: ((text: string) => void) | null): void {
    const value = this.#entry.value;
    if (typeof value === "string" && typeof callback === "function") {
      scheduleTransfers.get(this)?.(() => callback(value));
    }
  }
}

type Store = { entries: TransferEntry[]; readonly: boolean; schedule: (callback: () => void) => void };
const listStores = new WeakMap<DataTransferItemList, Store>();
const scheduleTransfers = new WeakMap<DataTransferItem, Store["schedule"]>();
const updateItems = Symbol("update items");

export class DataTransferItemList {
  readonly [index: number]: DataTransferItem;
  #items: DataTransferItem[] = [];
  #files = fileList();
  constructor(token: symbol, store: Store) {
    if (token !== transferToken) throw new TypeError("Illegal constructor");
    listStores.set(this, store);
  }
  get length(): number { return this.#items.length; }
  [Symbol.iterator](): ArrayIterator<DataTransferItem> { return this.#items.values(); }
  add(data: string, type: string): DataTransferItem | null;
  add(data: DOMFile): DataTransferItem | null;
  add(data: string | DOMFile, type?: string): DataTransferItem | null {
    const store = listStores.get(this)!;
    if (store.readonly) return null;
    if (type !== undefined) {
      const mime = String(type).toLowerCase();
      if (store.entries.some(entry => entry.type === mime && typeof entry.value === "string")) {
        throw new DOMException("Duplicate DataTransfer type", "NotSupportedError");
      }
      store.entries.push({ type: mime, value: String(data) });
    } else {
      if (typeof data !== "object" || blobBytes(data) === undefined || typeof data.name !== "string") {
        throw new TypeError("Expected a File");
      }
      store.entries.push({ type: data.type, value: data });
    }
    this[updateItems]();
    return this.#items[this.#items.length - 1];
  }
  remove(index: number): void {
    const store = listStores.get(this)!;
    if (store.readonly) throw new DOMException("DataTransfer is read-only", "InvalidStateError");
    store.entries.splice(Number(index) >>> 0, 1);
    this[updateItems]();
  }
  clear(): void {
    const store = listStores.get(this)!;
    if (store.readonly) return;
    store.entries.length = 0;
    this[updateItems]();
  }
  [updateItems]() {
    for (let index = 0; index < this.#items.length; index++) Reflect.deleteProperty(this, String(index));
    const store = listStores.get(this)!;
    this.#items = store.entries.map((entry, index) => {
      const item = new DataTransferItem(transferToken, entry);
      scheduleTransfers.set(item, store.schedule);
      Object.defineProperty(this, index, { configurable: true, enumerable: true, value: item });
      return item;
    });
    updateFileList(this.#files, store.entries.flatMap(({ value }) => typeof value === "string" ? [] : [value]));
    return this.#files;
  }
}

const transferStores = new WeakMap<DataTransfer, Store>();

export class DataTransfer {
  #items: DataTransferItemList;
  #files: ReturnType<typeof fileList>;
  constructor() {
    const store: Store = { entries: [], readonly: false, schedule: callback => void Promise.resolve().then(callback) };
    transferStores.set(this, store);
    this.#items = new DataTransferItemList(transferToken, store);
    this.#files = this.#items[updateItems]();
  }
  get items(): DataTransferItemList { return this.#items; }
  get files(): ReturnType<typeof fileList> { return this.#files; }
  get types(): readonly string[] {
    const entries = transferStores.get(this)!.entries;
    const types = entries.flatMap(({ type, value }) => typeof value === "string" ? [type] : []);
    if (this.files.length !== 0) types.push("Files");
    return Object.freeze(types);
  }
  getData(type: string): string {
    const original = String(type).toLowerCase();
    const entry = transferStores.get(this)!.entries.find(entry => typeof entry.value === "string" && entry.type === format(original));
    const value = (entry?.value as string | undefined) ?? "";
    if (original === "url") return value.split(/\r?\n/).find(line => line && !line.startsWith("#")) ?? "";
    return value;
  }
  setData(type: string, value: string): void {
    const store = transferStores.get(this)!;
    if (store.readonly) return;
    type = format(type);
    const index = store.entries.findIndex(entry => typeof entry.value === "string" && entry.type === type);
    const entry = { type, value: String(value) };
    if (index < 0) store.entries.push(entry);
    else store.entries[index] = entry;
    this.#items[updateItems]();
  }
  clearData(type?: string): void {
    const store = transferStores.get(this)!;
    if (store.readonly) return;
    const mime = type === undefined ? undefined : format(type);
    store.entries = store.entries.filter(entry => typeof entry.value !== "string" || mime !== undefined && entry.type !== mime);
    this.#items[updateItems]();
  }
}

export class ClipboardEvent extends Event {
  readonly clipboardData: DataTransfer | null;
  constructor(type: string, init: ConstructorParameters<typeof Event>[1] & { clipboardData?: DataTransfer | null } = {}) {
    super(type, init);
    this.clipboardData = init.clipboardData ?? null;
  }
}

const supported = new Set(["text/plain", "text/html", "image/png"]);
export class ClipboardItem {
  #items: Map<string, Promise<string | NetworkBlob>>;
  readonly presentationStyle: string;
  constructor(items: Record<string, string | NetworkBlob | PromiseLike<string | NetworkBlob>>,
              options: { presentationStyle?: string } = {}) {
    this.#items = new Map(Object.entries(items).map(([type, value]) => [type.toLowerCase(), Promise.resolve(value)]));
    if (this.#items.size === 0) throw new TypeError("ClipboardItem requires at least one representation");
    this.presentationStyle = options.presentationStyle ?? "unspecified";
    if (!["unspecified", "inline", "attachment"].includes(this.presentationStyle)) throw new TypeError("Invalid presentationStyle");
  }
  get types(): readonly string[] { return Object.freeze([...this.#items.keys()]); }
  static supports(type: string): boolean { return supported.has(String(type).toLowerCase()); }
  async getType(type: string): Promise<NetworkBlob> {
    type = String(type).toLowerCase();
    const value = this.#items.get(type);
    if (value === undefined) throw new DOMException("Clipboard type is absent", "NotFoundError");
    const data = await value;
    if (typeof data === "string") return new (globalThis as unknown as { Blob: new (parts: string[], options: { type: string }) => NetworkBlob }).Blob([data], { type });
    if (blobBytes(data) === undefined) throw new TypeError("Clipboard data must be a string or Blob");
    return data;
  }
}

export function pasteTransfer(entries: TransferEntry[], schedule: Store["schedule"]): DataTransfer {
  const data = new DataTransfer();
  Object.assign(transferStores.get(data)!, { entries, readonly: true, schedule });
  data.items[updateItems]();
  return data;
}

export function clipboardEntries(data: DataTransfer): readonly TransferEntry[] {
  return transferStores.get(data)!.entries;
}
