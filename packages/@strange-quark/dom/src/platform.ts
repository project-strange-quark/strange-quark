// SPDX-License-Identifier: MIT

import {
  ClipboardEvent, ClipboardItem, DataTransfer, DataTransferItem, DataTransferItemList,
  clipboardEntries, pasteTransfer,
} from "./clipboard.js";
import {
  clipboardSelection, editClipboardSelection, focusedElement, installPlatformActions,
} from "./document.js";
import type { Document, Element } from "./document.js";
import { dispatchTrustedEvent, DOMException, EventTarget } from "./events.js";
import { FileList } from "./file-list.js";
import { installFullscreen } from "./fullscreen.js";
import { registerWindowRequests } from "./native.js";
import { DragEvent } from "./drag.js";
import { navigatorIdentity } from "./navigator-identity.js";
import type { HostArch, HostOS } from "./navigator-identity.js";
import type { DOMFile } from "./file-list.js";
import { blobBytes } from "./fetch.js";
import type { BodyInit, NetworkBlob } from "./fetch.js";

type Bytes = Uint8Array<ArrayBuffer>;
type ClipboardData = [mime: string, bytes: Bytes][];
type FileData = [name: string, mime: string, modified: number, bytes: Bytes][];
type Host = {
  os?: HostOS;
  arch?: HostArch;
  clipboardRead?(request: number): void;
  clipboardWrite?(request: number, data: ClipboardData): void;
  fileOpen?(request: number, accept: string[], multiple: boolean): void;
  fileSave?(request: number, url: string, suggestedName: string): void;
  openExternal?(request: number, url: string): void;
  fullscreen?(request: number, enter: boolean): void;
  windowOp?(request: number, op: string, value?: unknown): void;
  utf8Encode(text: string): Bytes;
  utf8Decode(bytes: Bytes): string;
  postTask(callback: () => void): void;
};
const navigatorToken = Symbol("Navigator constructor");

export class Navigator {
  readonly userAgent: string;
  readonly platform: string;
  readonly language = "en-US";
  readonly languages = Object.freeze([this.language]);
  readonly maxTouchPoints = 0;
  constructor(token: symbol, clipboard: EventTarget, host: Pick<Host, "os" | "arch">) {
    if (token !== navigatorToken) throw new TypeError("Illegal constructor");
    const identity = navigatorIdentity(host.os, host.arch);
    this.userAgent = identity.userAgent;
    this.platform = identity.platform;
    Object.defineProperty(this, "clipboard", { enumerable: true, value: clipboard });
  }
}

export function installPlatform(global: Record<PropertyKey, unknown>, document: Document, host: Host) {
  const Blob = global.Blob as new (parts: BodyInit[], options: { type: string }) => NetworkBlob;
  const File = global.File as new (parts: BodyInit[], name: string, options: { type: string; lastModified?: number }) => DOMFile;
  const report = global.reportError as (error: unknown) => void;
  const pending = new Map<number, (ok: boolean, payload: unknown) => void>();
  let next = 0;
  function request<T>(send: (id: number) => void): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const id = ++next;
      if (id > 0xffffffff) throw new RangeError("Host request ID space exhausted");
      pending.set(id, (ok, payload) => {
        if (ok) resolve(payload as T);
        else {
          const [name, message] = payload as [string, string];
          reject(new DOMException(message, name));
        }
      });
      try { send(id); }
      catch (error) { pending.delete(id); reject(error); }
    });
  }
  const canRead = host.clipboardRead !== undefined;
  const canWrite = host.clipboardWrite !== undefined;
  function clipboardDenied<T>(): Promise<T> {
    return Promise.reject(new DOMException("Clipboard access is unavailable", "NotAllowedError"));
  }
  function read(): Promise<ClipboardData> {
    return canRead ? request(id => host.clipboardRead!(id)) : clipboardDenied();
  }
  function write(data: ClipboardData): Promise<void> {
    return canWrite ? request(id => host.clipboardWrite!(id, data)) : clipboardDenied();
  }
  const illegal = Symbol("Clipboard constructor");
  class Clipboard extends EventTarget {
    constructor(token: symbol) {
      super();
      if (token !== illegal) throw new TypeError("Illegal constructor");
    }
    async readText(): Promise<string> {
      const data = await read();
      const text = data.find(([mime]) => mime === "text/plain");
      return text === undefined ? "" : host.utf8Decode(text[1]);
    }
    writeText(text: string): Promise<void> { return write([["text/plain", host.utf8Encode(String(text))]]); }
    async read(): Promise<ClipboardItem[]> {
      const data = await read();
      if (data.length === 0) return [];
      return [new ClipboardItem(Object.fromEntries(data.map(([type, bytes]) => [type, new Blob([bytes], { type })])))];
    }
    async write(items: Iterable<ClipboardItem>): Promise<void> {
      const list = [...items];
      if (list.length === 0) return;
      if (list.length !== 1) throw new DOMException("The native clipboard accepts one item", "NotAllowedError");
      const [item] = list;
      if (!(item instanceof ClipboardItem)) throw new TypeError("Expected a ClipboardItem");
      const entries = await Promise.all(item.types.map(async type => [type, blobBytes(await item.getType(type))!] as ClipboardData[number]));
      await write(entries);
    }
  }

  const clipboard = new Clipboard(illegal);
  Object.assign(global, {
    Navigator, Clipboard, ClipboardItem, ClipboardEvent, DataTransfer, DataTransferItem, DataTransferItemList, FileList, DragEvent,
  });
  Object.defineProperty(global, "navigator", { configurable: true, enumerable: true, value: new Navigator(navigatorToken, clipboard, host) });

  function fire(target: Element, kind: string, data: DataTransfer, time: number): boolean {
    return dispatchTrustedEvent(target, new ClipboardEvent(kind, {
      clipboardData: data, bubbles: true, composed: true, cancelable: true,
    }), time);
  }
  async function paste(time: number): Promise<void> {
    const entries = await read();
    const target = focusedElement(document);
    if (target === null || !target.isConnected) return;
    const data = pasteTransfer(entries.map(([type, bytes]) => ({
      type,
      value: type.startsWith("text/") ? host.utf8Decode(bytes) :
        new File([bytes], type === "image/png" ? "image.png" : "clipboard", { type }),
    })), host.postTask);
    if (fire(target, "paste", data, time) && data.types.includes("text/plain")) {
      editClipboardSelection(document, target, "insertFromPaste", data.getData("text/plain"), time);
    }
  }
  function copyOrCut(target: Element, kind: "copy" | "cut", time: number): void {
    const data = new DataTransfer();
    if (!fire(target, kind, data, time)) {
      const entries: ClipboardData = clipboardEntries(data).map(({ type, value }) =>
        [type, typeof value === "string" ? host.utf8Encode(value) : blobBytes(value)!]);
      if (entries.length !== 0) void write(entries).catch(report);
    } else {
      const selection = clipboardSelection(document, target);
      if (selection === "") return;
      void clipboard.writeText(selection).catch(report);
      if (kind === "cut") editClipboardSelection(document, target, "deleteByCut", null, time);
    }
  }

  installPlatformActions(document, {
    clipboard: !canRead || !canWrite ? undefined : (target, key, time) => {
      if (key === "c") copyOrCut(target, "copy", time);
      else if (key === "x") copyOrCut(target, "cut", time);
      else if (key === "v") void paste(time).catch(report);
    },
    open: host.fileOpen === undefined ? undefined : async (accept, multiple) => {
      const files = await request<FileData | null>(id => host.fileOpen!(id, accept, multiple));
      return files?.map(([name, type, lastModified, bytes]) => new File([bytes], name, { type, lastModified })) ?? null;
    },
    save: host.fileSave === undefined ? undefined : (url, name) => {
      void request(id => host.fileSave!(id, url, name)).catch(report);
    },
    openExternal: host.openExternal === undefined ? undefined : url => {
      void request(id => host.openExternal!(id, url)).catch(report);
    },
    report,
  });
  if (host.fullscreen !== undefined) {
    installFullscreen(document, {
      send: enter => request(id => host.fullscreen!(id, enter)),
      postTask: host.postTask,
    });
  }
  if (host.windowOp !== undefined) {
    registerWindowRequests(global, (op, value) => request(id => host.windowOp!(id, op, value)));
  }
  return (id: number, ok: boolean, payload: unknown): void => {
    const resolve = pending.get(id);
    pending.delete(id);
    resolve?.(ok, payload);
  };
}
