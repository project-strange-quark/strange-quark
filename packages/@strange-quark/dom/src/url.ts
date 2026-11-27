// SPDX-License-Identifier: MIT

// URL and URLSearchParams over the host's WHATWG URL parser. They have no DOM
// dependency, so documents and workers share them.

export type ParsedURL = {
  href: string; protocol: string; origin: string; username: string; password: string;
  hostname: string; host: string; port: string; pathname: string; search: string; hash: string;
};

type MutableURLPart = Exclude<keyof ParsedURL, "href" | "origin">;
export type URLHost = {
  parseURL(input: string, base?: string): ParsedURL | undefined;
  updateURL(href: string, part: MutableURLPart, value: string): ParsedURL;
};

type URLInstance = {
  href: string; protocol: string; readonly origin: string; username: string; password: string;
  hostname: string; host: string; port: string; pathname: string; search: string; hash: string;
  readonly searchParams: URLSearchParams;
  toString(): string;
  toJSON(): string;
};
type URLConstructor = {
  new (input: string, base?: string): URLInstance;
  canParse(input: string, base?: string): boolean;
  parse(input: string, base?: string): URLInstance | null;
};

export function makeURL(host: URLHost): URLConstructor {
  class URL {
    #parts: ParsedURL;
    constructor(input: string, base?: string) {
      this.#parts = URL.#parse(String(input), base === undefined ? undefined : String(base));
    }
    static #parse(input: string, base?: string): ParsedURL {
      const parts = host.parseURL(input, base);
      if (parts === undefined) throw new TypeError("Invalid URL");
      return parts;
    }
    static canParse(input: string, base?: string): boolean {
      return host.parseURL(String(input), base === undefined ? undefined : String(base)) !== undefined;
    }
    static parse(input: string, base?: string): URL | null {
      return URL.canParse(input, base) ? new URL(input, base) : null;
    }
    get href(): string { return this.#parts.href; }
    set href(value: string) { this.#parts = URL.#parse(String(value)); }
    get protocol(): string { return this.#parts.protocol; }
    set protocol(value: string) { this.#set("protocol", value); }
    get origin(): string { return this.#parts.origin; }
    get username(): string { return this.#parts.username; }
    set username(value: string) { this.#set("username", value); }
    get password(): string { return this.#parts.password; }
    set password(value: string) { this.#set("password", value); }
    get hostname(): string { return this.#parts.hostname; }
    set hostname(value: string) { this.#set("hostname", value); }
    get host(): string { return this.#parts.host; }
    set host(value: string) { this.#set("host", value); }
    get port(): string { return this.#parts.port; }
    set port(value: string) { this.#set("port", value); }
    get pathname(): string { return this.#parts.pathname; }
    set pathname(value: string) { this.#set("pathname", value); }
    get search(): string { return this.#parts.search; }
    set search(value: string) { this.#set("search", value); }
    get hash(): string { return this.#parts.hash; }
    set hash(value: string) { this.#set("hash", value); }
    #set(part: MutableURLPart, value: string): void {
      this.#parts = host.updateURL(this.href, part, String(value));
    }
    #params = new URLSearchParams("", search => { this.search = search; }, () => this.search);
    get searchParams(): URLSearchParams { return this.#params; }
    toString(): string { return this.href; }
    toJSON(): string { return this.href; }
  }
  return URL;
}

function decodeParameter(input: string): string {
  return input.replace(/\+/g, " ").replace(/(?:%[\da-f]{2})+/gi, group => {
    try { return decodeURIComponent(group); }
    catch { return "�"; }
  });
}

function encodeParameter(input: string): string {
  return encodeURIComponent(input.toWellFormed()).replace(/[!'()~]/g, char => "%" + char.charCodeAt(0).toString(16).toUpperCase()).replace(/%20/g, "+");
}

export class URLSearchParams {
  #pairs: [string, string][];
  #write?: (value: string) => void;
  #read?: () => string;
  constructor(input: string | Iterable<Iterable<string>> | Record<string, string> = "", write?: (value: string) => void, read?: () => string) {
    this.#write = write;
    this.#read = read;
    if (typeof input === "string") this.#pairs = URLSearchParams.#decode(input);
    else if (Symbol.iterator in input) {
      this.#pairs = Array.from(input as Iterable<Iterable<string>>, pair => {
        const names = Array.from(pair, String);
        if (names.length !== 2) throw new TypeError("Expected parameter pair");
        return [names[0], names[1]];
      });
    } else this.#pairs = Object.entries(input).map(([key, value]) => [key, String(value)]);
  }
  static #decode(input: string): [string, string][] {
    return input.replace(/^\?/, "").split("&").filter(Boolean).map(part => {
      const split = part.indexOf("=");
      return split === -1 ? [decodeParameter(part), ""]
        : [decodeParameter(part.slice(0, split)), decodeParameter(part.slice(split + 1))];
    });
  }
  #current(): [string, string][] { return this.#read === undefined ? this.#pairs : URLSearchParams.#decode(this.#read()); }
  #replace(pairs: [string, string][]): void {
    this.#pairs = pairs;
    this.#write?.(URLSearchParams.#encode(pairs));
  }
  static #encode(pairs: [string, string][]): string {
    return pairs.map(([key, value]) => encodeParameter(key) + "=" + encodeParameter(value)).join("&");
  }
  get size(): number { return this.#current().length; }
  append(name: string, value: string): void { this.#replace([...this.#current(), [String(name), String(value)]]); }
  delete(name: string, value?: string): void {
    const key = String(name), match = value === undefined ? undefined : String(value);
    this.#replace(this.#current().filter(([key2, value2]) => key2 !== key || match !== undefined && value2 !== match));
  }
  get(name: string): string | null { return this.getAll(name)[0] ?? null; }
  getAll(name: string): string[] {
    const key = String(name);
    return this.#current().filter(([key2]) => key2 === key).map(([, value]) => value);
  }
  has(name: string, value?: string): boolean {
    const values = this.getAll(name);
    return value === undefined ? values.length > 0 : values.includes(String(value));
  }
  set(name: string, value: string): void {
    const key = String(name), replacement: [string, string] = [key, String(value)];
    const pairs = this.#current();
    const index = pairs.findIndex(([key2]) => key2 === key);
    if (index === -1) pairs.push(replacement);
    else pairs.splice(index, 1, replacement);
    this.#replace(pairs.filter(([key2], i) => key2 !== key || i === (index === -1 ? pairs.length - 1 : index)));
  }
  sort(): void { this.#replace([...this.#current()].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)); }
  *entries(): IterableIterator<[string, string]> {
    for (let i = 0; i < this.size; i++) yield [...this.#current()[i]];
  }
  *keys(): IterableIterator<string> { for (const [key] of this) yield key; }
  *values(): IterableIterator<string> { for (const [, value] of this) yield value; }
  [Symbol.iterator](): IterableIterator<[string, string]> { return this.entries(); }
  forEach(callback: (value: string, key: string, params: this) => void, thisArg?: unknown): void {
    for (const [key, value] of this) callback.call(thisArg, value, key, this);
  }
  toString(): string { return URLSearchParams.#encode(this.#current()); }
}
