// SPDX-License-Identifier: MIT

import type { NetworkBlob } from "./fetch.js";

export type DOMFile = NetworkBlob & { readonly name: string; readonly lastModified: number };

const fileListToken = Symbol("FileList constructor");
const contents = new WeakMap<FileList, DOMFile[]>();
export class FileList {
  readonly [index: number]: DOMFile;
  constructor(token: symbol) {
    if (token !== fileListToken) throw new TypeError("Illegal constructor");
    contents.set(this, []);
  }
  get length(): number { return contents.get(this)!.length; }
  item(index: number): DOMFile | null { return contents.get(this)![Number(index) >>> 0] ?? null; }
  [Symbol.iterator](): ArrayIterator<DOMFile> { return contents.get(this)!.values(); }
  get [Symbol.toStringTag](): string { return "FileList"; }
}

export function updateFileList(list: FileList, files: DOMFile[]): void {
  for (let i = 0; i < list.length; i++) Reflect.deleteProperty(list, String(i));
  contents.set(list, files);
  for (const [i, file] of files.entries()) {
    Object.defineProperty(list, i, { configurable: true, enumerable: true, value: file });
  }
}

export function fileList(files: DOMFile[] = []): FileList {
  const list = new FileList(fileListToken);
  updateFileList(list, files);
  return list;
}
