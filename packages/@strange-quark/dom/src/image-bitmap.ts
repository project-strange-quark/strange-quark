// SPDX-License-Identifier: MIT

import { ImageData, readCanvasPixels } from "./canvas.js";
import { HTMLCanvasElement, HTMLImageElement } from "./document.js";
import type { Document } from "./document.js";
import { DOMException } from "./events.js";
import { blobBytes } from "./fetch.js";
import type { NetworkBlob } from "./fetch.js";

type Source = NetworkBlob | HTMLImageElement | HTMLCanvasElement | ImageBitmap | ImageData;
type BitmapOptions = {
  imageOrientation?: "from-image" | "flipY" | "none";
  resizeWidth?: number;
  resizeHeight?: number;
  resizeQuality?: "pixelated" | "low" | "medium" | "high";
};
type Host = {
  prepareImageBitmap(bytes: Uint8Array<ArrayBuffer>, request: string): string | null;
  postTask(callback: () => void): void;
};

const token = Symbol("ImageBitmap constructor");
const bitmaps = new WeakMap<ImageBitmap, HTMLImageElement>();

export class ImageBitmap {
  constructor(secret: typeof token, image: HTMLImageElement) {
    if (secret !== token) throw new TypeError("Illegal constructor");
    bitmaps.set(this, image);
  }
  get width(): number { return bitmaps.get(this)?.naturalWidth ?? 0; }
  get height(): number { return bitmaps.get(this)?.naturalHeight ?? 0; }
  close(): void { bitmaps.delete(this); }
}
Object.defineProperty(ImageBitmap.prototype, Symbol.toStringTag, { value: "ImageBitmap", configurable: true });

export function bitmapImage(bitmap: ImageBitmap): HTMLImageElement {
  const image = bitmaps.get(bitmap);
  if (image === undefined) throw new DOMException("The ImageBitmap is closed", "InvalidStateError");
  return image;
}

function dimension(value: unknown, name: string): number | undefined {
  if (value === undefined) return undefined;
  const number = Math.trunc(Number(value));
  if (!Number.isFinite(number) || number < 0 || number > 0xffff_ffff) {
    throw new TypeError(`${name} is outside the unsigned long range`);
  }
  if (number === 0) throw new DOMException("The resize dimensions must be nonzero", "InvalidStateError");
  return number;
}

export function installImageBitmap(global: Record<PropertyKey, unknown>, document: Document, host: Host): void {
  const fetchSource = global.fetch as (url: string) => Promise<{ ok: boolean; blob(): Promise<NetworkBlob> }>;
  const pending = new Set<HTMLImageElement>();

  async function createImageBitmap(source: Source, first?: number | BitmapOptions | null, sy?: number,
    sw?: number, sh?: number, last?: BitmapOptions | null): Promise<ImageBitmap> {
    const cropped = arguments.length >= 5;
    if (arguments.length > 2 && !cropped) throw new TypeError("Expected four crop coordinates");
    const options = (cropped ? last : first) as BitmapOptions | null | undefined;
    if (options != null && typeof options !== "object" && typeof options !== "function") {
      throw new TypeError("Expected an ImageBitmap options dictionary");
    }
    const crop = cropped ? [first, sy, sw, sh].map(value => Number(value) | 0) : undefined;
    if (crop !== undefined && (crop[2] === 0 || crop[3] === 0)) throw new RangeError("The source rectangle is empty");
    const orientation = String(options?.imageOrientation ?? "from-image");
    const quality = String(options?.resizeQuality ?? "low");
    if (!["from-image", "none", "flipY"].includes(orientation) ||
        !["pixelated", "low", "medium", "high"].includes(quality)) {
      throw new TypeError("Invalid ImageBitmap options");
    }
    const resize = [dimension(options?.resizeWidth, "resizeWidth"), dimension(options?.resizeHeight, "resizeHeight")];

    let bytes: Uint8Array<ArrayBuffer> | undefined;
    let mime = "";
    let size: [number, number] | undefined;
    let dataUrl: string | undefined;
    if (source instanceof HTMLCanvasElement || source instanceof ImageData) {
      size = [source.width, source.height];
      if (size.includes(0)) throw new DOMException("The bitmap source is empty", "InvalidStateError");
      if (size[0] * size[1] * 4 > 512 * 1024 * 1024) throw new RangeError("The bitmap source is too large");
      const pixels = source instanceof ImageData ? source.data : readCanvasPixels(source, 0, 0, ...size);
      bytes = new Uint8Array(pixels instanceof Float16Array ? Uint8ClampedArray.from(pixels, value => 255 * value) : pixels);
    } else if (source instanceof HTMLImageElement || source instanceof ImageBitmap) {
      const image = source instanceof ImageBitmap ? bitmapImage(source) : source;
      if (image.naturalWidth === 0 || image.naturalHeight === 0) {
        throw new DOMException("The image source is unavailable", "InvalidStateError");
      }
      const address = image.currentSrc;
      if (address.startsWith("data:")) {
        dataUrl = address;
      } else {
        try {
          const response = await fetchSource(address);
          if (!response.ok) throw new Error("Image request failed");
          const data = await response.blob();
          mime = data.type;
          bytes = blobBytes(data);
        } catch {
          throw new DOMException("The image source could not be read", "InvalidStateError");
        }
      }
    } else {
      bytes = blobBytes(source);
      if (bytes === undefined) throw new TypeError("Expected an ImageBitmap source");
      mime = (source as NetworkBlob).type;
    }
    if (!bytes?.length && dataUrl === undefined) throw new DOMException("The bitmap source is empty", "InvalidStateError");
    const pixels = bytes ?? new Uint8Array();
    const request = JSON.stringify({ mime, size, dataUrl, crop, resize, quality, flipY: orientation === "flipY" });
    const url = await new Promise<string | null>((resolve, reject) => {
      host.postTask(() => {
        try { resolve(host.prepareImageBitmap(pixels, request)); }
        catch (error) { reject(error); }
      });
    });
    if (url === null) throw new DOMException("The bitmap could not be decoded", "InvalidStateError");
    const snapshot = document.createElement("img") as HTMLImageElement;
    pending.add(snapshot);
    try {
      snapshot.src = url;
      await snapshot.decode();
    } finally {
      pending.delete(snapshot);
    }
    return new ImageBitmap(token, snapshot);
  }

  Object.assign(global, { ImageBitmap, createImageBitmap });
}
