// SPDX-License-Identifier: MIT

import { DOMException } from "./events.js";

function indexOfKey(key: PropertyKey): number {
  if (typeof key !== "string") return -1;
  const index = Number(key);
  return Number.isInteger(index) && index >= 0 && String(index) === key ? index : -1;
}

export function indexed<T extends object, V>(target: T, values: () => readonly V[]): T {
  return new Proxy(target, {
    get(target, key, receiver) {
      const index = indexOfKey(key);
      return index < 0 ? Reflect.get(target, key, receiver) : values()[index];
    },
    has(target, key) {
      const index = indexOfKey(key);
      return index < 0 ? Reflect.has(target, key) : index < values().length;
    },
    ownKeys(target) {
      return [...values().map((_, index) => String(index)), ...Reflect.ownKeys(target)];
    },
    getOwnPropertyDescriptor(target, key) {
      const index = indexOfKey(key);
      if (index < 0) return Reflect.getOwnPropertyDescriptor(target, key);
      const items = values();
      if (index < items.length) return { value: items[index], configurable: true, enumerable: true };
    },
    set(target, key, value, receiver) {
      return indexOfKey(key) < 0 && Reflect.set(target, key, value, receiver);
    },
  });
}

const lists = new WeakMap<NodeList<unknown>, () => readonly unknown[]>();

export class NodeList<T> implements Iterable<T> {
  readonly [index: number]: T;

  constructor(items: () => readonly T[]) {
    const proxy = indexed(this, items);
    lists.set(proxy, items);
    return proxy;
  }

  get length(): number { return lists.get(this)!().length; }
  item(index: number): T | null { return this[index >>> 0] ?? null; }
  *[Symbol.iterator](): IterableIterator<T> { for (const pair of this.entries()) yield pair[1]; }
  values(): IterableIterator<T> { return this[Symbol.iterator](); }
  *keys(): IterableIterator<number> { for (let index = 0; index < this.length; index++) yield index; }
  *entries(): IterableIterator<[number, T]> { for (const index of this.keys()) yield [index, this[index]]; }

  forEach(callback: (value: T, index: number, list: NodeList<T>) => void, receiver?: unknown): void {
    for (const [index, value] of this.entries()) callback.call(receiver, value, index, this);
  }
}

type ClassAttribute = { read(): string | null; write(value: string): void };
const classAttributes = new WeakMap<DOMTokenList, ClassAttribute>();
const whiteSpace = /[\t\n\f\r ]/;

function tokens(value: string | null): string[] {
  return [...new Set((value ?? "").split(/[\t\n\f\r ]+/).filter(Boolean))];
}

function validated(values: string[]): string[] {
  const strings = values.map(String);
  for (const token of strings) {
    if (token.length === 0) throw new DOMException("A token cannot be empty", "SyntaxError");
    if (whiteSpace.test(token)) throw new DOMException("A token cannot contain whitespace", "InvalidCharacterError");
  }
  return strings;
}

export class DOMTokenList implements Iterable<string> {
  readonly [index: number]: string;

  constructor(attribute: ClassAttribute) {
    const proxy = indexed(this, () => tokens(attribute.read()));
    classAttributes.set(proxy, attribute);
    return proxy;
  }

  get value(): string { return classAttributes.get(this)!.read() ?? ""; }
  set value(value: string) { classAttributes.get(this)!.write(String(value)); }
  get length(): number { return tokens(this.value).length; }
  item(index: number): string | null { return this[index >>> 0] ?? null; }
  contains(token: string): boolean { return tokens(this.value).includes(String(token)); }
  toString(): string { return this.value; }
  [Symbol.iterator](): IterableIterator<string> { return this.values(); }
  values(): IterableIterator<string> { return tokens(this.value).values(); }
  keys(): IterableIterator<number> { return tokens(this.value).keys(); }
  entries(): IterableIterator<[number, string]> { return tokens(this.value).entries(); }
  forEach(callback: (value: string, index: number, list: DOMTokenList) => void, receiver?: unknown): void {
    for (const [index, value] of this.entries()) callback.call(receiver, value, index, this);
  }

  add(...values: string[]): void {
    const attribute = classAttributes.get(this)!;
    const current = attribute.read();
    const add = validated(values);
    if (current !== null || add.length !== 0) this.value = [...new Set([...tokens(current), ...add])].join(" ");
  }

  remove(...values: string[]): void {
    const remove = new Set(validated(values));
    const current = classAttributes.get(this)!.read();
    if (current !== null) this.value = tokens(current).filter((token) => !remove.has(token)).join(" ");
  }

  toggle(token: string, force?: boolean): boolean {
    if (arguments.length === 0) throw new TypeError("toggle requires a token");
    [token] = validated([token]);
    const current = tokens(this.value);
    const present = current.includes(token);
    const enabled = arguments.length === 1 ? !present : Boolean(force);
    if (present !== enabled) {
      if (enabled) current.push(token);
      this.value = (enabled ? current : current.filter((value) => value !== token)).join(" ");
    }
    return enabled;
  }

  replace(token: string, replacement: string): boolean {
    if (arguments.length < 2) throw new TypeError("replace requires both tokens");
    [token, replacement] = validated([token, replacement]);
    const current = tokens(this.value);
    if (!current.includes(token)) return false;
    this.value = [...new Set(current.map((value) => value === token ? replacement : value))].join(" ");
    return true;
  }

  supports(_token: string): boolean {
    throw new TypeError("Class names do not have a set of supported tokens");
  }
}

type DataAttributes = {
  entries(): Iterable<readonly [string, string]>;
  write(name: string, value: string): void;
  remove(name: string): void;
};

export function dataset(attributes: DataAttributes): Record<string, string> {
  const names = () => new Map(Array.from(attributes.entries(), ([name, value]) =>
    [name.slice(5).replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase()), value]));
  const attributeName = (key: string) => "data-" + key.replace(/[A-Z]/g, (letter) => "-" + letter.toLowerCase());
  return new Proxy({}, {
    get(target, key, receiver) {
      const values = names();
      return typeof key === "string" && values.has(key) ? values.get(key) : Reflect.get(target, key, receiver);
    },
    has(target, key) {
      return typeof key === "string" && names().has(key) || Reflect.has(target, key);
    },
    ownKeys() { return [...names().keys()]; },
    getOwnPropertyDescriptor(_target, key) {
      const values = names();
      if (typeof key === "string" && values.has(key)) {
        return { configurable: true, enumerable: true, writable: true, value: values.get(key) };
      }
    },
    set(_target, key, value) {
      if (typeof key !== "string") return false;
      if (/-[a-z]/.test(key)) throw new DOMException("Invalid data attribute key", "SyntaxError");
      attributes.write(attributeName(key), String(value));
      return true;
    },
    deleteProperty(_target, key) {
      if (typeof key === "string") attributes.remove(attributeName(key));
      return true;
    },
  });
}

type NamedElement = {
  id: string;
  namespaceURI: string | null;
  getAttributeNS(namespace: string | null, localName: string): string | null;
};
const htmlNamespace = "http://www.w3.org/1999/xhtml";
const collections = new WeakMap<object, {
  items: () => readonly NamedElement[];
  named: (name: string) => unknown;
}>();

function relatedNames(element: NamedElement): string[] {
  const name = element.namespaceURI === htmlNamespace ? element.getAttributeNS(null, "name") : null;
  return [element.id, name ?? ""].filter(Boolean);
}

export class HTMLCollection<T extends NamedElement, Named = T> implements Iterable<T> {
  readonly [index: number]: T;

  constructor(items: () => readonly T[], named?: (name: string) => Named | undefined) {
    if (typeof items !== "function") throw new TypeError("Illegal HTMLCollection constructor");
    const names = () => [...new Set(items().flatMap(relatedNames))];
    const item = named ?? ((key: string) => key === "" ? undefined : items().find(value => relatedNames(value).includes(key)));
    const proxy = new Proxy(indexed(this, items), {
      get(target, key, receiver) {
        if (Reflect.has(target, key) || typeof key !== "string") return Reflect.get(target, key, receiver);
        return item(key);
      },
      has(target, key) {
        return Reflect.has(target, key) || typeof key === "string" && item(key) !== undefined;
      },
      ownKeys(target) {
        const keys = Reflect.ownKeys(target);
        return [...keys, ...names().filter(key => !Reflect.has(target, key))];
      },
      getOwnPropertyDescriptor(target, key) {
        const descriptor = Reflect.getOwnPropertyDescriptor(target, key);
        if (descriptor !== undefined || Reflect.has(target, key) || typeof key !== "string") return descriptor;
        const value = item(key);
        if (value !== undefined) return { value, configurable: true, enumerable: false, writable: false };
      },
    });
    collections.set(proxy, { items, named: item });
    return proxy;
  }

  get length(): number { return collections.get(this)!.items().length; }
  item(index: number): T | null { return collections.get(this)!.items()[index >>> 0] as T ?? null; }
  namedItem(name: string): Named | null {
    return (collections.get(this)!.named(String(name)) as Named | undefined) ?? null;
  }
  *[Symbol.iterator](): IterableIterator<T> { for (let i = 0; i < this.length; i++) yield this.item(i)!; }
}
