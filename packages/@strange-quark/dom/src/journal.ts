// SPDX-License-Identifier: MIT

const HEADER_BYTES = 16;
export const RECORD_BYTES = 16;
const MAX_RECORD_BYTES = 65_536;
const WIRE_VERSION = 1;
export const NO_STRING = 0xffff_ffff;
export const STYLE_IMPORTANT = 1 << 0;
export const SELECTION_BACKWARD = 1 << 0;

export const Opcode = {
  CreateElement: 0x01,
  CreateText: 0x02,
  Insert: 0x03,
  Remove: 0x04,
  SetAttr: 0x05,
  RemoveAttr: 0x06,
  SetText: 0x07,
  SetStyle: 0x08,
  SetState: 0x09,
  SetValue: 0x0a,
  SheetInsert: 0x0b,
  SheetDelete: 0x0c,
  ScrollTo: 0x0d,
  Focus: 0x0e,
  Release: 0x0f,
  DefineAtom: 0x10,
  CanvasSubmit: 0x11,
  FontFace: 0x12,
  CanvasSize: 0x13,
  CreateNode: 0x14,
  Listeners: 0x15,
  SetSelection: 0x16,
  SetPreedit: 0x17,
} as const;

export const AtomKind = {
  Tag: 1,
  Attribute: 2,
  CssProperty: 3,
  Namespace: 4,
  QualifiedAttribute: 5,
} as const;

export const NodeType = {
  Comment: 8,
  Document: 9,
  DocumentType: 10,
  DocumentFragment: 11,
} as const;

export const FontFaceAction = {
  Define: 0,
  Add: 1,
  Remove: 2,
} as const;

export const ElementState = {
  Hover: 1 << 0,
  Active: 1 << 1,
  Focus: 1 << 2,
  FocusVisible: 1 << 3,
  FocusWithin: 1 << 4,
  Checked: 1 << 5,
  Indeterminate: 1 << 6,
  Disabled: 1 << 7,
  Selected: 1 << 8,
  ReadOnly: 1 << 9,
  PlaceholderShown: 1 << 10,
  Invalid: 1 << 11,
} as const;

export const ListenerFlags = {
  NonPassiveWheel: 1 << 0,
  NonPassiveTouchmove: 1 << 1,
} as const;

export const FocusFlags = {
  Blur: 1 << 0,
  PreventScroll: 1 << 1,
} as const;

type Opcode = (typeof Opcode)[keyof typeof Opcode];
type AtomKind = (typeof AtomKind)[keyof typeof AtomKind];
type Operand = number | string;
type Flush = (buffer: ArrayBuffer, length: number) => number;
export type JournalAtom = { kind: AtomKind; text: string };

function isWireOperand(value: Operand): boolean {
  return typeof value !== "number" || (value >>> 0) === value;
}

export class Journal {
  #view = new DataView(new ArrayBuffer(256));
  #recordBytes = 0;
  #strings: string[] = [];
  #codeUnits = 0;
  #atoms = new Map<AtomKind, Map<string, number>>();
  #nextAtom = 1;
  #sink: Flush | undefined;
  #flushing = false;
  #released: number[] = [];
  #recycled: number[] = [];
  #retained?: Set<object>;
  #started = false;
  #pending: { owner: object; seal: () => void } | undefined;

  constructor(sink?: Flush) {
    this.#sink = sink;
  }

  // Packed atoms are already in the mirror. Seed interning without recording definitions.
  restoreAtoms(definitions: ReadonlyMap<number, JournalAtom>): void {
    if (this.#started) {
      throw new Error("Bootstrap requires a fresh journal");
    }
    for (const [id, { kind, text }] of definitions) {
      let atoms = this.#atoms.get(kind);
      if (atoms === undefined) {
        atoms = new Map();
        this.#atoms.set(kind, atoms);
      }
      if (!atoms.has(text)) {
        atoms.set(text, id);
      }
      this.#nextAtom = Math.max(this.#nextAtom, id + 1);
    }
    this.#started = true;
  }

  intern(kind: AtomKind, text: string): number {
    const atoms = this.#atoms.get(kind);
    const existing = atoms?.get(text);
    if (existing !== undefined) {
      return existing;
    }

    const atom = this.#nextAtom;
    this.#append(Opcode.DefineAtom, atom, text, kind, 0);
    this.#nextAtom++;
    if (atoms === undefined) {
      this.#atoms.set(kind, new Map([[text, atom]]));
    } else {
      atoms.set(text, atom);
    }
    this.#flushWhenFull();
    return atom;
  }

  // String operands are indexed only after rollover so they live with the
  // record that references them.
  record(opcode: Opcode, a: number, b: Operand, c: Operand, flags = 0): void {
    this.#append(opcode, a, b, c, flags);
    this.#flushWhenFull();
  }

  release(id: number): void {
    this.#append(Opcode.Release, id, 0, 0, 0);
    this.#released.push(id);
    this.#flushWhenFull();
  }

  takeRecycledNodeId(): number | undefined { return this.#recycled.pop(); }

  #append(opcode: Opcode, a: number, b: Operand, c: Operand, flags: number): void {
    if (this.#flushing) {
      throw new Error("Cannot write while flushing the journal");
    }
    if (!isWireOperand(a) || !isWireOperand(b) || !isWireOperand(c)) {
      throw new RangeError("Journal operand is outside u32");
    }
    if (typeof flags !== "number" || (flags & 0xff) !== flags) {
      throw new RangeError("Journal flags are outside u8");
    }
    this.#sealPending();
    if (this.#recordBytes === MAX_RECORD_BYTES) {
      this.flush();
    }

    const strings = Number(typeof b === "string") + Number(typeof c === "string");
    const units = (typeof b === "string" ? b.length : 0) + (typeof c === "string" ? c.length : 0);
    const offset = HEADER_BYTES + this.#recordBytes;
    const length = offset + RECORD_BYTES + (this.#strings.length + strings + 1) * 4 +
      (this.#codeUnits + units) * 2;
    if (length > 0xffff_ffff) {
      throw new RangeError("Journal batch length is outside u32");
    }
    if (length > this.#view.byteLength) {
      const grown = new ArrayBuffer(Math.min(0xffff_ffff, Math.max(length, this.#view.byteLength * 2)));
      new Uint8Array(grown).set(new Uint8Array(this.#view.buffer, 0, offset));
      this.#view = new DataView(grown);
    }

    const view = this.#view;
    // Write opcode, flags and the two reserved zero bytes together.
    view.setUint32(offset, opcode & 0xff | flags << 8, true);
    view.setUint32(offset + 4, a, true);
    view.setUint32(offset + 8, this.#word(b), true);
    view.setUint32(offset + 12, this.#word(c), true);
    this.#recordBytes += RECORD_BYTES;
    this.#started = true;
  }

  #flushWhenFull(): void {
    if (this.#recordBytes === MAX_RECORD_BYTES && this.#sink !== undefined) {
      this.flush();
    }
  }

  #word(operand: Operand): number {
    if (typeof operand === "number") {
      return operand;
    }
    const index = this.#strings.length;
    this.#strings.push(operand);
    this.#codeUnits += operand.length;
    return index;
  }

  #encode(): number {
    const view = this.#view;
    view.setUint32(0, this.#recordBytes, true);
    view.setUint32(4, this.#strings.length, true);
    view.setUint32(8, this.#codeUnits, true);
    view.setUint32(12, WIRE_VERSION, true);

    const offsetsStart = HEADER_BYTES + this.#recordBytes;
    const stringsStart = offsetsStart + (this.#strings.length + 1) * 4;
    view.setUint32(offsetsStart, 0, true);
    let unitsWritten = 0;
    for (let index = 0; index < this.#strings.length; index++) {
      const value = this.#strings[index];
      for (let unit = 0; unit < value.length; unit++) {
        view.setUint16(stringsStart + unitsWritten * 2, value.charCodeAt(unit), true);
        unitsWritten++;
      }
      view.setUint32(offsetsStart + (index + 1) * 4, unitsWritten, true);
    }
    return stringsStart + unitsWritten * 2;
  }

  // A run of canvas commands must appear before the next journal record.
  // Switching owners also seals the run, keeping cross-canvas reads ordered.
  deferRecord(owner: object, seal: () => void): void {
    if (this.#pending?.owner === owner) return;
    this.#sealPending();
    this.#flushWhenFull();
    this.#pending = { owner, seal };
  }

  // A deferral is removed before sealing. Leave the sealed record in the
  // batch if its sink rejects, so retrying the flush consumes it only once.
  recordDeferred(opcode: Opcode, a: number, b: Operand, c: Operand): void {
    this.#append(opcode, a, b, c, 0);
  }

  #sealPending(): void {
    const pending = this.#pending;
    if (pending === undefined) return;
    this.#pending = undefined;
    try {
      pending.seal();
    } catch (error) {
      this.#pending = pending;
      throw error;
    }
  }

  snapshot(): ArrayBuffer {
    if (this.#flushing) {
      throw new Error("Cannot snapshot while flushing the journal");
    }
    this.#sealPending();
    return this.#view.buffer.slice(0, this.#encode());
  }

  // The sink borrows the first length bytes for the duration of the call.
  get hasPendingRecords(): boolean { return this.#recordBytes !== 0 || this.#pending !== undefined; }

  // A deferred draw can refer to a detached source. Keep it alive until
  // the engine has consumed the batch that names it.
  retainUntilFlush(source: object): void {
    (this.#retained ??= new Set()).add(source);
  }

  flush(): number {
    if (this.#flushing) {
      throw new Error("Cannot reenter a journal flush");
    }
    this.#sealPending();
    const count = this.#recordBytes / RECORD_BYTES;
    if (count === 0) {
      return 0;
    }
    if (this.#sink === undefined) {
      throw new Error("A journal sink is required to flush records");
    }
    const length = this.#encode();
    this.#flushing = true;
    try {
      if (this.#sink(this.#view.buffer, length) !== count) {
        throw new Error("The sink did not acknowledge the entire journal batch");
      }
      this.#recordBytes = 0;
      this.#strings.length = 0;
      this.#codeUnits = 0;
      for (const id of this.#released) {
        if ((id >>> 24) !== 255) this.#recycled.push(id + 0x0100_0000);
      }
      this.#released.length = 0;
      this.#retained?.clear();
      return count;
    } finally {
      this.#flushing = false;
    }
  }
}
