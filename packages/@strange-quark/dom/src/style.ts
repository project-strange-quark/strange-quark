// SPDX-License-Identifier: MIT

import { DOMException } from "./events.js";

type Declaration = { value: string; important: boolean };
type InlineStyle = {
  read(): string | null;
  commit(text: string, changes: ReadonlyMap<string, Declaration>): void;
  computed?(property: string): string;
};

const properties = new Set(`
  align-content align-items align-self animation animation-delay animation-duration animation-name
  appearance aspect-ratio background background-color background-image background-position
  background-repeat background-size border border-bottom border-color border-left border-radius
  border-right border-style border-top border-width bottom box-shadow box-sizing caret-color
  clear clip clip-path color column-gap contain content cursor direction display filter flex flex-basis
  flex-direction flex-flow flex-grow flex-shrink flex-wrap float font font-family font-size
  font-style font-variant font-weight gap grid grid-area grid-auto-columns grid-auto-flow
  grid-auto-rows grid-column grid-row grid-template grid-template-areas grid-template-columns
  grid-template-rows height inset isolation justify-content justify-items justify-self left letter-spacing
  line-clamp line-height list-style list-style-type margin margin-bottom margin-left margin-right
  margin-top max-height max-width min-height min-width object-fit object-position opacity order
  outline outline-color outline-offset outline-style outline-width overflow overflow-wrap
  overflow-x overflow-y padding padding-bottom padding-left padding-right padding-top perspective
  pointer-events position resize right row-gap scroll-behavior text-align text-decoration
  text-indent text-overflow text-shadow text-transform top touch-action transform transform-origin
  transition transition-delay transition-duration transition-property user-select vertical-align
  visibility white-space width will-change word-break word-spacing z-index
  fill fill-opacity stroke stroke-width stop-color stop-opacity -webkit-line-clamp -webkit-box-orient
`.trim().split(/\s+/));

function canonical(name: string): string {
  return name.startsWith("--") ? name : name.toLowerCase();
}

function namedProperty(name: string): string {
  if (name === "cssFloat") return "float";
  const css = name.replace(/[A-Z]/g, value => "-" + value.toLowerCase());
  return css.startsWith("webkit-") || css.startsWith("ms-") ? "-" + css : css;
}

function supported(name: string): boolean {
  return properties.has(name) || /^--[\w\p{L}-]+$/u.test(name);
}

/** Separators inside strings and CSS functions belong to their token. */
function* declarations(text: string): IterableIterator<string> {
  let start = 0;
  let depth = 0;
  let quote = "";
  for (let index = 0; index < text.length; index++) {
    const value = text[index];
    if (value === "\\") index++;
    else if (quote !== "") {
      if (value === quote) quote = "";
    } else if (value === '"' || value === "'") quote = value;
    else if (value === "(" || value === "[" || value === "{") depth++;
    else if (value === ")" || value === "]" || value === "}") depth--;
    else if (value === ";" && depth === 0) {
      yield text.slice(start, index);
      start = index + 1;
    }
  }
  if (quote === "" && depth === 0) yield text.slice(start);
}

function parse(text: string | null): Map<string, Declaration> {
  const result = new Map<string, Declaration>();
  for (const declaration of declarations((text ?? "").replace(/\/\*[\s\S]*?\*\//g, ""))) {
    const colon = declaration.indexOf(":");
    if (colon < 0) continue;
    const name = canonical(declaration.slice(0, colon).trim());
    if (!supported(name)) continue;
    const raw = declaration.slice(colon + 1).trim();
    const important = /!\s*important\s*$/i.exec(raw);
    const value = raw.slice(0, important?.index ?? raw.length).trim();
    if (value !== "" && (important !== null || !result.get(name)?.important)) {
      result.set(name, { value, important: important !== null });
    }
  }
  return result;
}

function serialize(values: ReadonlyMap<string, Declaration>): string {
  return [...values].map(([name, { value, important }]) =>
    `${name}: ${value}${important ? " !important" : ""};`).join(" ");
}

const owners = new WeakMap<CSSStyleDeclaration, InlineStyle>();
const propertyNames = [...properties].sort();

function writable(style: CSSStyleDeclaration): InlineStyle {
  const owner = owners.get(style)!;
  if (owner.computed !== undefined) {
    throw new DOMException("Computed styles are read-only", "NoModificationAllowedError");
  }
  return owner;
}

export function computedStyle(read: (name: string) => string): CSSStyleDeclaration {
  return new CSSStyleDeclaration({ read: () => null, computed: read, commit() {} });
}

export class CSSStyleDeclaration {
  readonly [index: number]: string;

  constructor(owner: InlineStyle) {
    if (owner === undefined) throw new TypeError("Illegal CSSStyleDeclaration constructor");
    const proxy = new Proxy(this, {
      get(target, key, receiver) {
        if (typeof key !== "string" || Reflect.has(target, key)) return Reflect.get(target, key, receiver);
        if (/^(0|[1-9]\d*)$/.test(key)) return receiver.item(Number(key)) || undefined;
        const name = namedProperty(key);
        return supported(name) ? receiver.getPropertyValue(name) : undefined;
      },
      set(target, key, value, receiver) {
        if (typeof key !== "string" || Reflect.has(target, key)) return Reflect.set(target, key, value, receiver);
        const name = namedProperty(key);
        if (!supported(name)) return Reflect.set(target, key, value, receiver);
        receiver.setProperty(name, value === null ? "" : String(value));
        return true;
      },
      has(target, key) {
        return Reflect.has(target, key) || typeof key === "string" && supported(namedProperty(key));
      },
    });
    owners.set(proxy, owner);
    return proxy;
  }

  get cssText(): string { return serialize(parse(owners.get(this)!.read())); }

  set cssText(value: string) {
    const owner = writable(this);
    const next = parse(String(value));
    const changes = new Map(next);
    for (const key of parse(owner.read()).keys()) {
      if (!next.has(key)) changes.set(key, { value: "", important: false });
    }
    owner.commit(serialize(next), changes);
  }

  get length(): number {
    const owner = owners.get(this)!;
    return owner.computed === undefined ? parse(owner.read()).size : propertyNames.length;
  }
  item(index: number): string {
    const owner = owners.get(this)!;
    return (owner.computed === undefined ? [...parse(owner.read()).keys()] : propertyNames)[index >>> 0] ?? "";
  }
  getPropertyValue(name: string): string {
    const owner = owners.get(this)!;
    name = canonical(String(name));
    if (owner.computed !== undefined) return owner.computed(name);
    return parse(owner.read()).get(name)?.value ?? "";
  }
  getPropertyPriority(name: string): string { return parse(owners.get(this)!.read()).get(canonical(String(name)))?.important ? "important" : ""; }

  setProperty(name: string, value: string, priority = ""): void {
    const owner = writable(this);
    name = canonical(String(name));
    value = value === null ? "" : String(value).trim();
    priority = priority === null ? "" : String(priority).toLowerCase();
    if (!supported(name)) return;
    if (value === "") {
      this.removeProperty(name);
      return;
    }
    if (priority !== "" && priority !== "important" || /!\s*important\s*$/i.test(value) ||
        [...declarations(value)].length !== 1 || /^(?:NaN|Infinity)(?:px)?$/.test(value)) return;
    const values = parse(owner.read());
    const declaration = { value, important: priority === "important" };
    if (values.get(name)?.value === value && values.get(name)?.important === declaration.important) return;
    values.set(name, declaration);
    owner.commit(serialize(values), new Map([[name, declaration]]));
  }

  removeProperty(name: string): string {
    const owner = writable(this);
    name = canonical(String(name));
    const values = parse(owner.read());
    const old = values.get(name);
    if (old === undefined) return "";
    values.delete(name);
    owner.commit(serialize(values), new Map([[name, { value: "", important: false }]]));
    return old.value;
  }
}
