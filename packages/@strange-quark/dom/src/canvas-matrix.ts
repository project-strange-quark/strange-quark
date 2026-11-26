// SPDX-License-Identifier: MIT

import { DOMException } from "./events.js";

export type Matrix = readonly [number, number, number, number, number, number];
export const identity: Matrix = [1, 0, 0, 1, 0, 0];

/** The affine matrix values used by Canvas 2D. */
export class DOMMatrix {
  #components: number[];

  constructor(sequence: Iterable<number> = identity) {
    if (typeof sequence === "string" || sequence == null || !(Symbol.iterator in Object(sequence))) {
      throw new TypeError("Expected an affine matrix sequence");
    }
    this.#components = Array.from(sequence, Number);
    if (this.#components.length !== identity.length) throw new TypeError("Expected six matrix components");
  }

  get [Symbol.toStringTag](): string { return "DOMMatrix"; }
  get is2D(): boolean { return true; }
  get isIdentity(): boolean { return this.#components.every((value, index) => value === identity[index]); }
  get a(): number { return this.#components[0]; }
  set a(value: number) { this.#components[0] = Number(value); }
  get b(): number { return this.#components[1]; }
  set b(value: number) { this.#components[1] = Number(value); }
  get c(): number { return this.#components[2]; }
  set c(value: number) { this.#components[2] = Number(value); }
  get d(): number { return this.#components[3]; }
  set d(value: number) { this.#components[3] = Number(value); }
  get e(): number { return this.#components[4]; }
  set e(value: number) { this.#components[4] = Number(value); }
  get f(): number { return this.#components[5]; }
  set f(value: number) { this.#components[5] = Number(value); }

  get m11(): number { return this.a; }
  set m11(value: number) { this.a = value; }
  get m12(): number { return this.b; }
  set m12(value: number) { this.b = value; }
  get m21(): number { return this.c; }
  set m21(value: number) { this.c = value; }
  get m22(): number { return this.d; }
  set m22(value: number) { this.d = value; }
  get m41(): number { return this.e; }
  set m41(value: number) { this.e = value; }
  get m42(): number { return this.f; }
  set m42(value: number) { this.f = value; }

  toString(): string {
    if (!this.#components.every(Number.isFinite)) throw new DOMException("The matrix is not finite", "InvalidStateError");
    return `matrix(${this.#components.join(", ")})`;
  }
}
