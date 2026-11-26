// SPDX-License-Identifier: MIT

export const Command = {
  Save: 1, Restore: 2, BeginPath: 3, Move: 4, Line: 5, Quadratic: 6, Cubic: 7,
  Close: 8, Ellipse: 9, Transform: 10, Fill: 11, Stroke: 12, Clip: 13,
  FillRect: 14, StrokeRect: 15, ClearRect: 16, DefinePath: 17, FillPaint: 18,
  StrokePaint: 19, DefinePaint: 20, Sampling: 21, StrokeOptions: 22, DrawImage: 23,
  PutImageData: 25, FillText: 26, FilterText: 27,
} as const;

type Opcode = (typeof Command)[keyof typeof Command];
type PathOpcode = typeof Command.Move | typeof Command.Line | typeof Command.Quadratic |
  typeof Command.Cubic | typeof Command.Close | typeof Command.Ellipse;
export type PathCommand = { op: PathOpcode; coordinates: readonly number[]; anticlockwise?: boolean };
export type Color = readonly [number, number, number, number];

function aligned(bytes: number): number { return Math.ceil(bytes / 8) * 8; }

function command(view: DataView, offset: number, op: Opcode, length: number): void {
  view.setUint8(offset, op);
  view.setUint32(offset + 4, length, true);
}

function doubles(view: DataView, offset: number, values: readonly number[]): number {
  for (const value of values) {
    view.setFloat64(offset, value, true);
    offset += 8;
  }
  return offset;
}

function words(view: DataView, offset: number, values: readonly number[]): number {
  for (const value of values) {
    view.setUint32(offset, value, true);
    offset += 4;
  }
  return offset;
}

function utf16(view: DataView, offset: number, text: string): number {
  for (let i = 0; i < text.length; i++) view.setUint16(offset + 2 * i, text.charCodeAt(i), true);
  return offset + 2 * text.length;
}

function pathPayload(path: PathCommand): number {
  return path.coordinates.length * 8 + (path.op === Command.Ellipse ? 4 : 0);
}

function pathRecord(view: DataView, offset: number, path: PathCommand): number {
  const length = pathPayload(path);
  command(view, offset, path.op, length);
  const end = doubles(view, offset + 8, path.coordinates);
  if (path.op === Command.Ellipse) view.setUint32(end, Number(path.anticlockwise), true);
  return offset + 8 + aligned(length);
}

export class CanvasStream {
  #view = new DataView(new ArrayBuffer(256));
  #length = 12;
  #count = 0;
  #nextPath = 1;
  #nextPaint = 1;

  #allocate(length: number): number {
    const offset = this.#length;
    const next = offset + 8 + aligned(length);
    if (next > 0xffff_ffff) throw new RangeError("Canvas stream is too large");
    if (next > this.#view.byteLength) {
      const grown = new ArrayBuffer(Math.min(0xffff_ffff, Math.max(next, this.#view.byteLength * 2)));
      new Uint8Array(grown).set(new Uint8Array(this.#view.buffer, 0, offset));
      this.#view = new DataView(grown);
    }
    this.#length = next;
    // Storage is reused across submissions. Clear headers, reserved fields and padding.
    new Uint8Array(this.#view.buffer, offset, this.#length - offset).fill(0);
    this.#count++;
    return offset;
  }

  #payload(op: Opcode, length: number): number {
    const offset = this.#allocate(length);
    command(this.#view, offset, op, length);
    return offset + 8;
  }

  geometry(op: Opcode, values: readonly number[] = []): void {
    const offset = this.#payload(op, values.length * 8);
    doubles(this.#view, offset, values);
  }

  path(paths: readonly PathCommand[]): void {
    for (const path of paths) {
      const offset = this.#allocate(pathPayload(path));
      pathRecord(this.#view, offset, path);
    }
  }

  drawPath(op: Opcode, rule: number, path?: readonly PathCommand[]): void {
    let id = 0;
    if (path !== undefined) {
      id = this.#nextPath++;
      const length = path.reduce((sum, item) => sum + 8 + aligned(pathPayload(item)), 0);
      let offset = this.#payload(Command.DefinePath, 8 + length);
      offset = words(this.#view, offset, [id, length]);
      for (const item of path) offset = pathRecord(this.#view, offset, item);
    }
    const offset = this.#payload(op, 8);
    words(this.#view, offset, [id, rule]);
  }

  paint(op: typeof Command.FillPaint | typeof Command.StrokePaint, rgba: Color): void {
    const id = this.#nextPaint++;
    const offset = this.#payload(Command.DefinePaint, 12);
    const color = words(this.#view, offset, [id, 0]);
    for (let i = 0; i < rgba.length; i++) this.#view.setUint8(color + i, rgba[i]);
    const reference = this.#payload(op, 4);
    this.#view.setUint32(reference, id, true);
  }

  sampling(alpha: number, smoothing: boolean): void {
    const offset = this.#payload(Command.Sampling, 16);
    doubles(this.#view, offset, [alpha]);
    words(this.#view, offset + 8, [0, Number(smoothing)]);
  }

  stroke(width: number, miterLimit: number, dashOffset: number, cap: number, join: number, dash: readonly number[]): void {
    const offset = this.#payload(Command.StrokeOptions, 36 + dash.length * 8);
    doubles(this.#view, offset, [width, miterLimit, dashOffset]);
    words(this.#view, offset + 24, [cap, join, dash.length]);
    doubles(this.#view, offset + 36, dash);
  }

  image(kind: number, id: number, rects: readonly number[]): void {
    const offset = this.#payload(Command.DrawImage, 72);
    words(this.#view, offset, [kind, id]);
    doubles(this.#view, offset + 8, rects);
  }

  imagePixels(x: number, y: number, width: number, height: number): Uint8ClampedArray<ArrayBuffer> {
    const length = width * height * 4;
    const offset = this.#payload(Command.PutImageData, 16 + length);
    this.#view.setInt32(offset, x, true);
    this.#view.setInt32(offset + 4, y, true);
    words(this.#view, offset + 8, [width, height]);
    return new Uint8ClampedArray(this.#view.buffer, offset + 16, length);
  }

  text(x: number, y: number, maxWidth: number, align: number, baseline: number, font: string, text: string): void {
    const offset = this.#payload(Command.FillText, 44 + (font.length + text.length) * 2);
    doubles(this.#view, offset, [x, y, maxWidth]);
    // CanvasDirection uses WebIDL order: ltr, rtl, inherit.
    const strings = words(this.#view, offset + 24, [align, baseline, 2, font.length, text.length]);
    utf16(this.#view, utf16(this.#view, strings, font), text);
  }

  filter(text: string): void {
    const offset = this.#payload(Command.FilterText, 4 + text.length * 2);
    this.#view.setUint32(offset, text.length, true);
    utf16(this.#view, offset + 4, text);
  }

  bytes(): Uint8Array<ArrayBuffer> {
    this.#view.setUint32(0, 0x3156434a, true); // JCV1
    this.#view.setUint32(4, this.#length, true);
    this.#view.setUint32(8, this.#count, true);
    return new Uint8Array(this.#view.buffer, 0, this.#length);
  }

  reset(): void {
    this.#length = 12;
    this.#count = 0;
    this.#nextPaint = this.#nextPath = 1;
  }
}
