// SPDX-License-Identifier: MIT

import { DOMException } from "./events.js";

type Bytes = Uint8Array<ArrayBuffer>;
type Algorithm = string | { name: string };
type Usage = "encrypt" | "decrypt";
type Gcm = { name: string; iv: ArrayBuffer | ArrayBufferView; additionalData?: ArrayBuffer | ArrayBufferView; tagLength?: number };
type Jwk = { kty?: string; k?: string; alg?: string; use?: string; ext?: boolean; key_ops?: string[] };
type Host = {
  randomBytes(size: number): Bytes;
  randomUUID(): string;
  base64Encode(bytes: Uint8Array, url: boolean): string;
  base64Decode(text: string, url: boolean): Bytes | null;
  cryptoDigest(name: string, data: Uint8Array): Bytes;
  cryptoGenerateKey(bits: number, extractable: boolean, usages: number): number;
  cryptoImportKey(data: Uint8Array, extractable: boolean, usages: number): number;
  cryptoExportKey(key: number): Bytes;
  cryptoForgetKey(key: number): void;
  cryptoAesGcm(key: number, iv: Uint8Array, aad: Uint8Array, data: Uint8Array, encrypt: boolean): Bytes | null;
};

export function bufferBytes(data: ArrayBuffer | ArrayBufferView): Uint8Array<ArrayBufferLike> {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data) && data.buffer instanceof ArrayBuffer) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  throw new TypeError("Expected an ArrayBuffer or view");
}

function algorithmName(algorithm: Algorithm): string {
  return String(typeof algorithm === "string" ? algorithm : algorithm?.name).toUpperCase();
}

function requireAes(algorithm: Algorithm): void {
  if (algorithmName(algorithm) !== "AES-GCM") throw new DOMException("Expected AES-GCM", "NotSupportedError");
}

function supportedSize(bits: number): void {
  if (bits !== 128 && bits !== 256) throw new DOMException("AES key length must be 128 or 256 bits", "OperationError");
}

function checkedUsages(iterable: Iterable<string>): Usage[] {
  const values = Array.from(iterable, String);
  if (values.length === 0 || values.some(value => value !== "encrypt" && value !== "decrypt")) {
    throw new DOMException("Invalid AES-GCM key usages", "SyntaxError");
  }
  return [...new Set(values)] as Usage[];
}

function mask(usages: Usage[]): number {
  return Number(usages.includes("encrypt")) | (Number(usages.includes("decrypt")) << 1);
}

export function installCrypto(global: Record<PropertyKey, unknown>, host: Host): void {
  const secret = Symbol("crypto constructor");
  const keys = new WeakMap<CryptoKey, { id: number; length: number; extractable: boolean; usages: Usage[] }>();
  const reap = new FinalizationRegistry<number>(id => host.cryptoForgetKey(id));

  class CryptoKey {
    constructor(token: typeof secret) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get type(): string { return "secret"; }
    get extractable(): boolean { return keys.get(this)!.extractable; }
    get usages(): Usage[] { return [...keys.get(this)!.usages]; }
    get algorithm() { return { name: "AES-GCM", length: keys.get(this)!.length }; }
    get [Symbol.toStringTag](): string { return "CryptoKey"; }
  }

  function key(id: number, length: number, extractable: boolean, usages: Usage[]): CryptoKey {
    const key = new CryptoKey(secret);
    keys.set(key, { id, length, extractable, usages });
    reap.register(key, id);
    return key;
  }

  function keyInfo(key: CryptoKey) {
    const state = keys.get(key);
    if (state === undefined) throw new TypeError("Expected a CryptoKey");
    return state;
  }

  function crypt(params: Gcm, key: CryptoKey, data: ArrayBuffer | ArrayBufferView, usage: Usage): ArrayBuffer {
    requireAes(params);
    const state = keyInfo(key);
    if (!state.usages.includes(usage)) throw new DOMException("Key does not permit this operation", "InvalidAccessError");
    const iv = bufferBytes(params.iv);
    if (iv.byteLength !== 12 || params.tagLength !== undefined && Number(params.tagLength) !== 128) {
      throw new DOMException("AES-GCM requires a 96-bit IV and 128-bit tag", "OperationError");
    }
    const aad = params.additionalData === undefined ? new Uint8Array() : bufferBytes(params.additionalData);
    const output = host.cryptoAesGcm(state.id, iv, aad, bufferBytes(data), usage === "encrypt");
    if (output === null) throw new DOMException("AES-GCM operation failed", "OperationError");
    return output.buffer;
  }

  class SubtleCrypto {
    constructor(token: typeof secret) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    async digest(algorithm: Algorithm, data: ArrayBuffer | ArrayBufferView): Promise<ArrayBuffer> {
      const name = algorithmName(algorithm);
      if (name !== "SHA-1" && name !== "SHA-256") throw new DOMException("Unsupported digest", "NotSupportedError");
      return host.cryptoDigest(name, bufferBytes(data)).buffer;
    }
    async generateKey(algorithm: { name: string; length: number }, extractable: boolean, keyUsages: Iterable<string>): Promise<CryptoKey> {
      requireAes(algorithm);
      const length = Number(algorithm.length);
      supportedSize(length);
      const usages = checkedUsages(keyUsages);
      return key(host.cryptoGenerateKey(length, Boolean(extractable), mask(usages)), length, Boolean(extractable), usages);
    }
    async importKey(format: string, data: Jwk | ArrayBuffer | ArrayBufferView, algorithm: Algorithm, extractable: boolean,
        keyUsages: Iterable<string>): Promise<CryptoKey> {
      requireAes(algorithm);
      const usages = checkedUsages(keyUsages);
      let bytes: Uint8Array<ArrayBufferLike>;
      if (format === "jwk") {
        const jwk = data as Jwk;
        if (jwk?.kty !== "oct" || typeof jwk.k !== "string") throw new DOMException("Invalid AES JWK", "DataError");
        const decoded = host.base64Decode(jwk.k, true);
        if (decoded === null || ![16, 32].includes(decoded.length) || jwk.alg !== undefined && jwk.alg !== `A${decoded.length * 8}GCM` ||
            jwk.use !== undefined && jwk.use !== "enc" || jwk.ext === false && extractable ||
            jwk.key_ops !== undefined && (!Array.isArray(jwk.key_ops) || new Set(jwk.key_ops).size !== jwk.key_ops.length ||
                !usages.every(usage => jwk.key_ops!.includes(usage)))) {
          throw new DOMException("Invalid AES JWK", "DataError");
        }
        bytes = decoded;
      } else if (format === "raw") {
        bytes = bufferBytes(data as ArrayBuffer);
        if (![16, 32].includes(bytes.length)) throw new DOMException("Invalid raw AES key length", "DataError");
      } else {
        throw new DOMException("Unsupported key format", "NotSupportedError");
      }
      return key(host.cryptoImportKey(bytes, Boolean(extractable), mask(usages)), bytes.length * 8, Boolean(extractable), usages);
    }
    async exportKey(format: string, key: CryptoKey): Promise<Jwk | ArrayBuffer> {
      const state = keyInfo(key);
      if (!state.extractable) throw new DOMException("Key is not extractable", "InvalidAccessError");
      if (format !== "jwk" && format !== "raw") throw new DOMException("Unsupported key format", "NotSupportedError");
      const out = host.cryptoExportKey(state.id);
      if (format === "raw") return out.buffer;
      return { key_ops: [...state.usages], ext: true, kty: "oct", k: host.base64Encode(out, true), alg: `A${state.length}GCM` };
    }
    async encrypt(algorithm: Gcm, key: CryptoKey, data: ArrayBuffer | ArrayBufferView): Promise<ArrayBuffer> {
      return crypt(algorithm, key, data, "encrypt");
    }
    async decrypt(algorithm: Gcm, key: CryptoKey, data: ArrayBuffer | ArrayBufferView): Promise<ArrayBuffer> {
      return crypt(algorithm, key, data, "decrypt");
    }
  }

  const subtle = new SubtleCrypto(secret);
  const typedArrayName = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Uint8Array.prototype), Symbol.toStringTag)!.get!;
  const randomTypes = new Set(["Int8Array", "Uint8Array", "Uint8ClampedArray", "Int16Array", "Uint16Array",
    "Int32Array", "Uint32Array", "BigInt64Array", "BigUint64Array"]);
  class Crypto {
    constructor(token: typeof secret) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get subtle(): SubtleCrypto { return subtle; }
    randomUUID(): string { return host.randomUUID(); }
    getRandomValues<T extends ArrayBufferView>(view: T): T {
      if (!randomTypes.has(typedArrayName.call(view))) {
        throw new DOMException("Expected an integer typed array", "TypeMismatchError");
      }
      const bytes = bufferBytes(view);
      if (bytes.byteLength > 65536) throw new DOMException("Random value quota exceeded", "QuotaExceededError");
      bytes.set(host.randomBytes(bytes.byteLength));
      return view;
    }
  }

  Object.assign(global, { crypto: new Crypto(secret), Crypto, SubtleCrypto, CryptoKey });
}
