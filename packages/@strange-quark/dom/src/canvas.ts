// SPDX-License-Identifier: MIT

import { CanvasPath, finite, pathCommands, Path2D } from "./canvas-path.js";
import { CanvasStream, Command } from "./canvas-stream.js";
import type { Color, PathCommand } from "./canvas-stream.js";
import { DOMMatrix, identity } from "./canvas-matrix.js";
import type { Matrix } from "./canvas-matrix.js";
import { canvasJournal, Document, flushDocument, HTMLCanvasElement, HTMLImageElement, nodeId } from "./document.js";
import { DOMException } from "./events.js";
import { Opcode } from "./journal.js";
import { bitmapImage, ImageBitmap, installImageBitmap } from "./image-bitmap.js";
import type { Journal } from "./journal.js";

type Size = readonly [number, number];
type MatrixInit = Partial<Record<"a" | "b" | "c" | "d" | "e" | "f" | "m11" | "m12" | "m21" | "m22" | "m41" | "m42", number>>;
type Box = [number, number, number, number];
type ImageDataSettings = { colorSpace?: "srgb" | "display-p3"; pixelFormat?: "rgba-unorm8" | "rgba-float16" };
type Pixels = Uint8ClampedArray<ArrayBuffer> | Float16Array<ArrayBuffer>;
type CanvasBlob = new (parts: Iterable<ArrayBuffer>, options: { type: string }) => object;
type Metrics = { width: number; actualBoundingBoxAscent: number; actualBoundingBoxDescent: number };
type Mime = "image/png" | "image/jpeg" | "image/webp";

export type CanvasHost = {
  stageBytes(bytes: Uint8Array): number;
  cssParseColor(text: string, id: number): Color | null;
  cssValidateFilter(text: string): boolean;
  cssParseCanvasFont(text: string, id: number): string | null;
  measureText(font: string, text: string): Metrics;
  canvasImageSize(id: number): Size | null;
  prepareImageBitmap(bytes: Uint8Array<ArrayBuffer>, request: string): string | null;
  canvasReadback(id: number, x: number, y: number, width: number, height: number): Uint8ClampedArray<ArrayBuffer>;
  canvasEncodeSync(id: number, mime: Mime, quality?: number): ArrayBuffer;
  canvasEncode(id: number, mime: Mime, quality?: number): Promise<ArrayBuffer>;
  postTask(callback: () => void): void;
};

type Installation = { host: CanvasHost; global: Record<PropertyKey, unknown> };
const installations = new WeakMap<Journal, Installation>();
const canvases = new WeakMap<HTMLCanvasElement, { sized: boolean; context?: CanvasRenderingContext2D }>();

function canvasState(canvas: HTMLCanvasElement) {
  let state = canvases.get(canvas);
  if (state === undefined) canvases.set(canvas, state = { sized: false });
  return state;
}

function canvasHost(canvas: HTMLCanvasElement): Installation | undefined {
  const journal = canvasJournal(canvas);
  return journal === null ? undefined : installations.get(journal);
}

const reset = Symbol("reset CanvasRenderingContext2D");
const construct = Symbol("construct CanvasRenderingContext2D");

export function resizeCanvas(canvas: HTMLCanvasElement): void {
  const state = canvasState(canvas);
  const journal = canvasJournal(canvas);
  if (journal !== null) {
    journal.record(Opcode.CanvasSize, nodeId(canvas), canvas.width, canvas.height);
  }
  state.sized = true;
  state.context?.[reset]();
}

function ensureSize(canvas: HTMLCanvasElement): void {
  if (!canvasState(canvas).sized) resizeCanvas(canvas);
}

export function canvasContext(canvas: HTMLCanvasElement, kind: string): CanvasRenderingContext2D | null {
  if (String(kind) !== "2d") return null;
  const installation = canvasHost(canvas);
  if (installation === undefined) return null;
  ensureSize(canvas);
  const state = canvasState(canvas);
  return state.context ??= new CanvasRenderingContext2D(construct, canvas, installation.host);
}

function serializeColor([red, green, blue, alpha]: Color): string {
  if (alpha === 255) return "#" + [red, green, blue].map(value => value.toString(16).padStart(2, "0")).join("");
  for (const factor of [10, 100, 1000]) {
    const value = Math.round(alpha * factor / 255) / factor;
    if (Math.round(value * 255) === alpha) return `rgba(${red}, ${green}, ${blue}, ${value})`;
  }
  return `rgba(${red}, ${green}, ${blue}, ${alpha / 255})`;
}

const caps = ["butt", "round", "square"] as const;
const joins = ["miter", "round", "bevel"] as const;
const qualities = ["low", "medium", "high"] as const;
const aligns = ["start", "end", "left", "right", "center"] as const;
const baselines = ["top", "hanging", "middle", "alphabetic", "ideographic", "bottom"] as const;

function defaults() {
  return {
    transform: identity, alpha: 1, filter: "none", smoothing: true, quality: "low" as typeof qualities[number],
    fill: "#000000", stroke: "#000000", width: 1, cap: "butt" as typeof caps[number],
    join: "miter" as typeof joins[number], miterLimit: 10, dash: [] as number[], dashOffset: 0, font: "10px sans-serif",
    align: "start" as typeof aligns[number], baseline: "alphabetic" as typeof baselines[number],
  };
}

function matrixParts(object: MatrixInit | null | undefined): number[] {
  object ??= {};
  return ([
    ["a", "m11", 1], ["b", "m12", 0], ["c", "m21", 0],
    ["d", "m22", 1], ["e", "m41", 0], ["f", "m42", 0],
  ] as const).map(([short, long, fallback]) => {
    const first = object[short], second = object[long];
    if (first !== undefined && second !== undefined && !Object.is(Number(first), Number(second)) && Number(first) !== Number(second)) {
      throw new TypeError("Conflicting members of a 2D matrix");
    }
    return Number(first !== undefined ? first : second !== undefined ? second : fallback);
  });
}

function multiply([a, b, c, d, e, f]: Matrix, [g, h, i, j, k, l]: Matrix): Matrix {
  return [a * g + c * h, b * g + d * h, a * i + c * j, b * i + d * j, a * k + c * l + e, b * k + d * l + f];
}

function normalizedBox(x: number, y: number, width: number, height: number): Box {
  return [width < 0 ? x + width : x, height < 0 ? y + height : y, Math.abs(width), Math.abs(height)];
}

function canvasLong(value: unknown): number {
  const number = Math.trunc(Number(value));
  if (!Number.isFinite(number) || number < -0x80000000 || number > 0x7fffffff) throw new TypeError("Canvas coordinate is outside long");
  return number === 0 ? 0 : number;
}

function imageRects(args: number[], width: number, height: number): number[] | undefined {
  const [x, y] = args;
  const source: Box = args.length >= 8 ? normalizedBox(x, y, args[2], args[3]) : [0, 0, width, height];
  const dest = args.length >= 8 ? normalizedBox(args[4], args[5], args[6], args[7]) :
    normalizedBox(x, y, args.length >= 4 ? args[2] : width, args.length >= 4 ? args[3] : height);
  const [sx, sy, sw, sh] = source, [dx, dy, dw, dh] = dest;
  if (sw === 0 || sh === 0 || dw === 0 || dh === 0) return;
  const left = Math.max(0, sx), top = Math.max(0, sy);
  const right = Math.min(width, sx + sw), bottom = Math.min(height, sy + sh);
  if (right <= left || bottom <= top) return;
  const area = [
    left, top, right - left, bottom - top,
    dx + ((left - sx) / sw) * dw, dy + ((top - sy) / sh) * dh,
    ((right - left) / sw) * dw, ((bottom - top) / sh) * dh,
  ];
  return area.every(Number.isFinite) ? area : undefined;
}

export class CanvasRenderingContext2D extends CanvasPath {
  #canvas: HTMLCanvasElement;
  #host: CanvasHost;
  #stream = new CanvasStream();
  #state = defaults();
  #stack: ReturnType<typeof defaults>[] = [];
  #seal = () => {
    const bytes = this.#stream.bytes();
    const id = this.#host.stageBytes(bytes);
  const journal = canvasJournal(this.#canvas);
  if (journal === null) return;
  journal.recordDeferred(Opcode.CanvasSubmit, nodeId(this.#canvas), id, bytes.byteLength);
    this.#stream.reset();
  };

  constructor(secret: typeof construct, canvas: HTMLCanvasElement, host: CanvasHost) {
    if (secret !== construct) throw new TypeError("Illegal constructor");
    super(commands => this.#path(commands));
    this.#canvas = canvas;
    this.#host = host;
  }

  get canvas(): HTMLCanvasElement { return this.#canvas; }

  [reset](): void {
    this.#state = defaults();
    this.#stack = [];
  }

  #record(): CanvasStream {
  canvasJournal(this.#canvas)?.deferRecord(this, this.#seal);
    return this.#stream;
  }

  save(): void {
    this.#record().geometry(Command.Save);
    this.#stack.push({ ...this.#state });
  }

  restore(): void {
    const state = this.#stack.at(-1);
    if (state === undefined) return;
    this.#record().geometry(Command.Restore);
    this.#stack.pop();
    this.#state = state;
  }

  #transform(matrix: Matrix): void {
    if (!matrix.every(Number.isFinite)) return;
    this.#record().geometry(Command.Transform, matrix);
    this.#state.transform = matrix;
  }

  getTransform(): DOMMatrix { return new DOMMatrix(this.#state.transform); }
  resetTransform(): void { this.#transform(identity); }

  setTransform(matrix?: MatrixInit | null): void;
  setTransform(a: number, b: number, c: number, d: number, e: number, f: number): void;
  setTransform(first?: number | MatrixInit | null, b?: number, c?: number, d?: number, e?: number, f?: number): void {
    if (arguments.length > 1 && arguments.length < 6) throw new TypeError("Expected six matrix components");
    if (arguments.length === 1 && first != null && typeof first !== "object" && typeof first !== "function") {
      throw new TypeError("Expected a matrix dictionary");
    }
    const values = arguments.length <= 1 ? matrixParts(first as MatrixInit | null) : finite([first, b, c, d, e, f]);
    if (values !== null) this.#transform(values as unknown as Matrix);
  }
  transform(a: number, b: number, c: number, d: number, e: number, f: number): void {
    if (arguments.length < 6) throw new TypeError("Expected six matrix components");
    const values = finite([a, b, c, d, e, f]);
    if (values !== null) this.#transform(multiply(this.#state.transform, values as unknown as Matrix));
  }
  scale(x: number, y: number): void {
    if (arguments.length < 2) throw new TypeError("Expected two scale factors");
    this.#transform(multiply(this.#state.transform, [Number(x), 0, 0, Number(y), 0, 0]));
  }
  translate(x: number, y: number): void {
    if (arguments.length < 2) throw new TypeError("Expected two translation coordinates");
    this.#transform(multiply(this.#state.transform, [1, 0, 0, 1, Number(x), Number(y)]));
  }
  rotate(angle: number): void {
    if (arguments.length < 1) throw new TypeError("Expected an angle");
    angle = Number(angle);
    const cos = Math.cos(angle), sin = Math.sin(angle);
    this.#transform(multiply(this.#state.transform, [cos, sin, -sin, cos, 0, 0]));
  }

  get globalAlpha(): number { return this.#state.alpha; }
  set globalAlpha(value: number) {
    value = Number(value);
    if (!Number.isFinite(value) || value < 0 || value > 1 || this.#state.alpha === value) return;
    this.#record().sampling(value, this.#state.smoothing);
    this.#state.alpha = value;
  }
  get imageSmoothingEnabled(): boolean { return this.#state.smoothing; }
  set imageSmoothingEnabled(value: boolean) {
    value = Boolean(value);
    if (this.#state.smoothing === value) return;
    this.#record().sampling(this.#state.alpha, value);
    this.#state.smoothing = value;
  }
  get imageSmoothingQuality(): string { return this.#state.quality; }
  set imageSmoothingQuality(value: string) {
    const quality = qualities.find(item => item === String(value));
    if (quality !== undefined) this.#state.quality = quality;
  }

  get filter(): string { return this.#state.filter; }
  set filter(value: string) {
    const text = String(value);
    if (text === this.#state.filter || !this.#host.cssValidateFilter(text)) return;
    this.#record().filter(text);
    this.#state.filter = text;
  }

  get fillStyle(): string { return this.#state.fill; }
  set fillStyle(value: string) { this.#paint("fill", value); }
  get strokeStyle(): string { return this.#state.stroke; }
  set strokeStyle(value: string) { this.#paint("stroke", value); }
  #paint(key: "fill" | "stroke", value: string): void {
    const color = this.#host.cssParseColor(String(value), nodeId(this.#canvas));
    if (color === null) return;
    const text = serializeColor(color);
    if (text === this.#state[key]) return;
    this.#record().paint(key === "fill" ? Command.FillPaint : Command.StrokePaint, color);
    this.#state[key] = text;
  }

  #rect(op: typeof Command.FillRect | typeof Command.StrokeRect | typeof Command.ClearRect, rect: Box): void {
    const value = finite(rect);
    if (value === null) return;
    const [, , width, height] = value;
    if (op === Command.StrokeRect ? width === 0 && height === 0 : width === 0 || height === 0) return;
    this.#record().geometry(op, value);
  }
  fillRect(x: number, y: number, w: number, h: number): void { this.#rect(Command.FillRect, [x, y, w, h]); }
  strokeRect(x: number, y: number, w: number, h: number): void { this.#rect(Command.StrokeRect, [x, y, w, h]); }
  clearRect(x: number, y: number, w: number, h: number): void { this.#rect(Command.ClearRect, [x, y, w, h]); }
  beginPath(): void { this.#record().geometry(Command.BeginPath); }
  #path(commands: readonly PathCommand[]): void { this.#record().path(commands); }

  #drawPath(op: typeof Command.Fill | typeof Command.Stroke | typeof Command.Clip, input?: Path2D | string, fillRule = "nonzero"): void {
    const path = input instanceof Path2D ? pathCommands(input) : undefined;
    const rule = path !== undefined || input === undefined ? String(fillRule) : String(input);
    if (rule !== "nonzero" && rule !== "evenodd") throw new TypeError("Invalid canvas fill rule");
    this.#record().drawPath(op, Number(rule === "evenodd"), path);
  }
  fill(pathOrRule?: Path2D | "nonzero" | "evenodd", rule?: "nonzero" | "evenodd"): void {
    this.#drawPath(Command.Fill, pathOrRule, rule);
  }
  stroke(path?: Path2D): void {
    if (path !== undefined && !(path instanceof Path2D)) throw new TypeError("Expected Path2D");
    this.#drawPath(Command.Stroke, path);
  }
  clip(pathOrRule?: Path2D | "nonzero" | "evenodd", rule?: "nonzero" | "evenodd"): void {
    this.#drawPath(Command.Clip, pathOrRule, rule);
  }

  get lineWidth(): number { return this.#state.width; }
  set lineWidth(value: number) {
    value = Number(value);
    if (!Number.isFinite(value) || value <= 0 || value === this.#state.width) return;
    this.#strokeOptions({ ...this.#state, width: value });
  }
  get lineCap(): string { return this.#state.cap; }
  set lineCap(value: string) {
    const cap = caps.find(item => item === String(value));
    if (cap === undefined || cap === this.#state.cap) return;
    this.#strokeOptions({ ...this.#state, cap: cap });
  }
  get lineJoin(): string { return this.#state.join; }
  set lineJoin(value: string) {
    const join = joins.find(item => item === String(value));
    if (join === undefined || join === this.#state.join) return;
    this.#strokeOptions({ ...this.#state, join: join });
  }
  get miterLimit(): number { return this.#state.miterLimit; }
  set miterLimit(value: number) {
    value = Number(value);
    if (!Number.isFinite(value) || value <= 0 || value === this.#state.miterLimit) return;
    this.#strokeOptions({ ...this.#state, miterLimit: value });
  }
  getLineDash(): number[] { return this.#state.dash.slice(); }
  setLineDash(segments: Iterable<number>): void {
    if (segments == null || typeof segments !== "object" && typeof segments !== "function" || !(Symbol.iterator in segments)) {
      throw new TypeError("Expected a sequence of dash lengths");
    }
    const values = Array.from(segments, Number);
    if (!values.every(value => Number.isFinite(value) && value >= 0)) return;
    const dash = values.length % 2 === 0 ? values : values.concat(values);
    this.#strokeOptions({ ...this.#state, dash });
  }
  get lineDashOffset(): number { return this.#state.dashOffset; }
  set lineDashOffset(value: number) {
    value = Number(value);
    if (!Number.isFinite(value) || value === this.#state.dashOffset) return;
    this.#strokeOptions({ ...this.#state, dashOffset: value });
  }
  #strokeOptions(next: ReturnType<typeof defaults>): void {
    const { width, miterLimit, dashOffset, cap, join, dash } = next;
    this.#record().stroke(width, miterLimit, dashOffset, caps.indexOf(cap), joins.indexOf(join), dash);
    this.#state = next;
  }

  get font(): string { return this.#state.font; }
  set font(value: string) {
    const font = this.#host.cssParseCanvasFont(String(value), nodeId(this.#canvas));
    if (font !== null) this.#state.font = font;
  }
  get textAlign(): string { return this.#state.align; }
  set textAlign(value: string) {
    const align = aligns.find(item => item === String(value));
    if (align !== undefined) this.#state.align = align;
  }
  get textBaseline(): string { return this.#state.baseline; }
  set textBaseline(value: string) {
    const baseline = baselines.find(item => item === String(value));
    if (baseline !== undefined) this.#state.baseline = baseline;
  }
  fillText(text: string, x: number, y: number, maxWidth?: number): void {
    text = String(text);
    const coords = finite([x, y]);
    const width = maxWidth === undefined ? NaN : Number(maxWidth);
    if (coords === null || maxWidth !== undefined && (!Number.isFinite(width) || width <= 0) || text.length === 0) return;
    const { font, align, baseline } = this.#state;
    this.#record().text(coords[0], coords[1], width, aligns.indexOf(align), baselines.indexOf(baseline), font, text);
  }
  measureText(text: string): Metrics {
    text = String(text);
    flushDocument(this.#canvas.ownerDocument);
    return this.#host.measureText(this.#state.font, text);
  }

  drawImage(source: HTMLCanvasElement | HTMLImageElement | ImageBitmap, dx: number, dy: number): void;
  drawImage(source: HTMLCanvasElement | HTMLImageElement | ImageBitmap, dx: number, dy: number, dw: number, dh: number): void;
  drawImage(source: HTMLCanvasElement | HTMLImageElement | ImageBitmap, sx: number, sy: number, sw: number, sh: number,
    dx: number, dy: number, dw: number, dh: number): void;
  drawImage(input: HTMLCanvasElement | HTMLImageElement | ImageBitmap, ...coordinates: number[]): void {
    const source = input instanceof ImageBitmap ? bitmapImage(input) : input;
    const isCanvas = source instanceof HTMLCanvasElement;
    if (!isCanvas && !(source instanceof HTMLImageElement)) throw new TypeError("Expected a canvas, image or bitmap");
    const count = coordinates.length;
    if (count < 2 || count === 3 || count > 4 && count < 8) throw new TypeError("Invalid drawImage argument count");
    const args = finite(coordinates.slice(0, 8));
    if (args === null) return;
    const id = nodeId(source);
    let size: Size | null;
    if (isCanvas) {
      size = [source.width, source.height];
      if (size.includes(0)) throw new DOMException("Canvas source has zero size", "InvalidStateError");
      ensureSize(source);
    } else {
      flushDocument(source.ownerDocument);
      size = this.#host.canvasImageSize(id) ?? [source.naturalWidth, source.naturalHeight];
      if (size.includes(0)) return;
    }
    const rects = imageRects(args, ...size);
    if (rects !== undefined) {
      this.#record().image(Number(isCanvas), id, rects);
      canvasJournal(this.#canvas)?.retainUntilFlush(source);
    }
  }
  createImageData(imageData: ImageData): ImageData;
  createImageData(width: number, height: number, settings?: ImageDataSettings | null): ImageData;
  createImageData(first: number | ImageData, height?: number, settings?: ImageDataSettings | null): ImageData {
    if (arguments.length === 1 && first instanceof ImageData) {
      return new ImageData(first.width, first.height, { colorSpace: first.colorSpace, pixelFormat: first.pixelFormat });
    }
    if (arguments.length < 2) throw new TypeError("Expected ImageData or a width and height");
    return new ImageData(Math.abs(canvasLong(first)), Math.abs(canvasLong(height)), settings);
  }
  getImageData(sx: number, sy: number, sw: number, sh: number): ImageData {
    const args = [sx, sy, sw, sh].map(canvasLong) as Box;
    const [x, y, width, height] = normalizedBox(...args);
    if (width === 0 || height === 0) throw new DOMException("Canvas readback rectangle is empty", "IndexSizeError");
    if (width * height * 4 > 0xffff_ffff) throw new RangeError("Canvas readback is too large");
    const pixels = readCanvasPixels(this.#canvas, Math.max(-0x80000000, x), Math.max(-0x80000000, y), width, height);
    return new ImageData(pixels, width, height);
  }

  putImageData(imageData: ImageData, dx: number, dy: number): void;
  putImageData(imageData: ImageData, dx: number, dy: number, dirtyX: number, dirtyY: number,
    dirtyWidth: number, dirtyHeight: number): void;
  putImageData(imageData: ImageData, dx: number, dy: number, dirtyX?: number, dirtyY?: number,
    dirtyWidth?: number, dirtyHeight?: number): void {
    const image = pixelState.get(imageData);
    if (image === undefined || arguments.length < 3 || (arguments.length > 3 && arguments.length < 7)) {
      throw new TypeError("Expected ImageData and two or six coordinates");
    }
    dx = canvasLong(dx);
    dy = canvasLong(dy);
    const [x, y, width, height] = arguments.length === 3 ? [0, 0, image.width, image.height] :
      normalizedBox(canvasLong(dirtyX), canvasLong(dirtyY), canvasLong(dirtyWidth), canvasLong(dirtyHeight));
    if (image.data.buffer.detached) throw new DOMException("ImageData pixels are detached", "InvalidStateError");

    // Intersect in source coordinates. The dirty rectangle does not move the image origin.
    const left = Math.max(0, x, -dx), top = Math.max(0, y, -dy);
    const right = Math.min(image.width, x + width, this.#canvas.width - dx);
    const bottom = Math.min(image.height, y + height, this.#canvas.height - dy);
    if (right <= left || bottom <= top) return;
    const pixels = this.#record().imagePixels(dx + left, dy + top, right - left, bottom - top);
    const rowLength = (right - left) * 4;
    for (let row = top; row < bottom; row++) {
      const source = (row * image.width + left) * 4;
      const target = (row - top) * rowLength;
      if (image.pixelFormat === "rgba-unorm8" && image.colorSpace === "srgb") {
        pixels.set(image.data.subarray(source, source + rowLength), target);
      } else {
        copyConvertedPixels(image, source, pixels.subarray(target, target + rowLength));
      }
    }
  }
}

export function readCanvasPixels(canvas: HTMLCanvasElement, x: number, y: number, width: number, height: number): Uint8ClampedArray<ArrayBuffer> {
  const installation = canvasHost(canvas);
  if (installation === undefined) throw new DOMException("Canvas readback is unavailable", "NotSupportedError");
  ensureSize(canvas);
  flushDocument(canvas.ownerDocument);
  return installation.host.canvasReadback(nodeId(canvas), x, y, width, height);
}

type ImageDataState = Required<ImageDataSettings> & { data: Pixels; width: number; height: number };
const pixelState = new WeakMap<ImageData, ImageDataState>();

function linearComponent(value: number): number {
  const magnitude = Math.abs(value);
  return magnitude <= 0.04045 ? value / 12.92 : Math.sign(value) * ((magnitude + 0.055) / 1.055) ** 2.4;
}

function srgbComponent(value: number): number {
  return value <= 0.0031308 ? value * 12.92 : 1.055 * value ** (1 / 2.4) - 0.055;
}

function copyConvertedPixels(image: ImageDataState, source: number, target: Uint8ClampedArray): void {
  const { data, pixelFormat, colorSpace } = image;
  if (colorSpace === "srgb") {
    for (let i = 0; i < target.length; i++) target[i] = data[source + i] * 255;
    return;
  }
  const divisor = pixelFormat === "rgba-unorm8" ? 255 : 1;
  // Display P3 and sRGB share a transfer function and D65 white point. Convert
  // the linear primaries before quantizing to the canvas's straight sRGB bytes.
  for (let i = 0; i < target.length; i += 4) {
    const r = linearComponent(data[source + i] / divisor);
    const g = linearComponent(data[source + i + 1] / divisor);
    const b = linearComponent(data[source + i + 2] / divisor);
    target[i] = srgbComponent(1.22494018 * r - 0.22494018 * g) * 255;
    target[i + 1] = srgbComponent(-0.04205695 * r + 1.04205695 * g) * 255;
    target[i + 2] = srgbComponent(-0.01963755 * r - 0.07863605 * g + 1.0982736 * b) * 255;
    target[i + 3] = data[source + i + 3] / divisor * 255;
  }
}

export class ImageData {
  constructor(width: number, height: number, settings?: ImageDataSettings | null);
  constructor(data: Pixels, width: number, height?: number, settings?: ImageDataSettings | null);
  constructor(first: number | Pixels, second: number, third?: number | ImageDataSettings | null, fourth?: ImageDataSettings | null) {
    if (arguments.length < 2) throw new TypeError("Expected ImageData dimensions");
    const data = first instanceof Uint8ClampedArray || first instanceof Float16Array ? first : undefined;
    const settings = data === undefined ? third as ImageDataSettings | null | undefined : fourth;
    if (settings != null && typeof settings !== "object" && typeof settings !== "function") throw new TypeError("Expected ImageData settings");
    const colorSpace = settings?.colorSpace === undefined ? "srgb" : String(settings.colorSpace);
    const pixelFormat = settings?.pixelFormat === undefined ? "rgba-unorm8" : String(settings.pixelFormat);
    if (colorSpace !== "srgb" && colorSpace !== "display-p3" || pixelFormat !== "rgba-unorm8" && pixelFormat !== "rgba-float16") {
      throw new TypeError("Invalid ImageData settings");
    }
    if (data !== undefined && (data instanceof Float16Array) !== (pixelFormat === "rgba-float16")) {
      throw new DOMException("The array type does not match the pixel format", "InvalidStateError");
    }
    const width = Number(data === undefined ? first : second) >>> 0;
    const height = data === undefined ? Number(second) >>> 0 : third === undefined ? data.length / (4 * width) : Number(third) >>> 0;
    if (width === 0 || height === 0) throw new DOMException("ImageData dimensions must be non-zero", "IndexSizeError");
    if (!Number.isInteger(height) || data !== undefined && data.length !== width * height * 4) {
      throw new DOMException("ImageData length does not match its dimensions", "IndexSizeError");
    }
    const length = width * height * 4;
    if (length > 0xffff_ffff) throw new RangeError("ImageData is too large");
    const pixels = data ?? (pixelFormat === "rgba-float16" ? new Float16Array(length) : new Uint8ClampedArray(length));
    pixelState.set(this, { data: pixels, width, height, colorSpace, pixelFormat });
  }
  get data(): Pixels { return pixelState.get(this)!.data; }
  get width(): number { return pixelState.get(this)!.width; }
  get height(): number { return pixelState.get(this)!.height; }
  get colorSpace(): Required<ImageDataSettings>["colorSpace"] { return pixelState.get(this)!.colorSpace; }
  get pixelFormat(): Required<ImageDataSettings>["pixelFormat"] { return pixelState.get(this)!.pixelFormat; }
}

function mimeType(value: string): Mime {
  const text = String(value).toLowerCase();
  return text === "image/jpeg" || text === "image/webp" ? text : "image/png";
}

function b64(buffer: ArrayBuffer): string {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const bytes = new Uint8Array(buffer);
  let encoded = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i], b = bytes[i + 1] ?? 0, c = bytes[i + 2] ?? 0;
    encoded += alphabet[a >>> 2] + alphabet[(a & 3) << 4 | b >>> 4] +
      (i + 1 < bytes.length ? alphabet[(b & 15) << 2 | c >>> 6] : "=") +
      (i + 2 < bytes.length ? alphabet[c & 63] : "=");
  }
  return encoded;
}

export function canvasDataURL(canvas: HTMLCanvasElement, type = "image/png", quality?: number): string {
  const mime = mimeType(type);
  if (canvas.width === 0 || canvas.height === 0) return "data:,";
  const installation = canvasHost(canvas);
  if (installation === undefined) throw new DOMException("Canvas encoding is unavailable", "NotSupportedError");
  ensureSize(canvas);
  flushDocument(canvas.ownerDocument);
  const bytes = installation.host.canvasEncodeSync(nodeId(canvas), mime, typeof quality === "number" ? quality : undefined);
  return `data:${mime};base64,${b64(bytes)}`;
}

export function canvasToBlob(canvas: HTMLCanvasElement, callback: (blob: object | null) => void, type = "image/png", quality?: number): void {
  if (typeof callback !== "function") throw new TypeError("Expected a Blob callback");
  const mime = mimeType(type);
  const installation = canvasHost(canvas);
  if (installation === undefined) throw new DOMException("Canvas encoding is unavailable", "NotSupportedError");
  const { host, global } = installation;
  const Blob = global.Blob as CanvasBlob;
  const complete = (blob: object | null) => host.postTask(() => callback(blob));
  if (canvas.width === 0 || canvas.height === 0) {
    complete(null);
    return;
  }
  ensureSize(canvas);
  flushDocument(canvas.ownerDocument);
  host.canvasEncode(nodeId(canvas), mime, typeof quality === "number" ? quality : undefined).then(
    bytes => complete(new Blob([bytes], { type: mime })),
    () => complete(null),
  );
}

export function installCanvas(global: Record<PropertyKey, unknown>, document: Document, host: CanvasHost): void {
  const journal = canvasJournal(document);
  if (journal !== null) installations.set(journal, { host, global });
  Object.assign(global, { CanvasRenderingContext2D, Path2D, ImageData, DOMMatrix });
  if (typeof host.prepareImageBitmap === "function") installImageBitmap(global, document, host);
}
