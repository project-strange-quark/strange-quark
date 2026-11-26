// SPDX-License-Identifier: MIT

import { indexed } from "./collections.js";
import type { Element } from "./document.js";
import { DOMException } from "./events.js";

const internal = Symbol("CSSOM constructor");
const sheets = new WeakMap<CSSStyleSheet, SheetState>();
const ruleOwners = new WeakMap<CSSRule, CSSStyleSheet | null>();

export type SheetSource = {
  read(): readonly string[];
  parse(rule: string): string | null;
  insert(index: number, rule: string): void;
  remove(index: number): void;
};

type SheetState = {
  owner: Element | null;
  href: string | null;
  source: SheetSource | null;
  rules?: CSSRule[];
  list?: CSSRuleList;
  disabled: boolean;
};

export class CSSRule {
  static readonly STYLE_RULE = 1;
  static readonly CHARSET_RULE = 2;
  static readonly IMPORT_RULE = 3;
  static readonly MEDIA_RULE = 4;
  static readonly FONT_FACE_RULE = 5;
  static readonly PAGE_RULE = 6;
  static readonly KEYFRAMES_RULE = 7;
  static readonly KEYFRAME_RULE = 8;
  static readonly MARGIN_RULE = 9;
  static readonly NAMESPACE_RULE = 10;
  static readonly COUNTER_STYLE_RULE = 11;
  static readonly SUPPORTS_RULE = 12;
  static readonly FONT_FEATURE_VALUES_RULE = 14;

  #text: string;

  constructor(token?: symbol, sheet?: CSSStyleSheet, text = "") {
    if (token !== internal || sheet === undefined) throw new TypeError("Illegal CSSRule constructor");
    this.#text = text;
    ruleOwners.set(this, sheet);
  }

  get cssText(): string { return this.#text; }
  set cssText(_value: string) {}
  get parentStyleSheet(): CSSStyleSheet | null { return ruleOwners.get(this)!; }
  get parentRule(): null { return null; }
  get type(): number {
    // Stylo supplies serialized, validated text. Only identify the at-rule
    // name here to expose the CSSOM's legacy numeric constants.
    const atRule = /^@([\w-]+)/.exec(this.#text)?.[1];
    return atRule === undefined ? CSSRule.STYLE_RULE : legacyRuleTypes.get(atRule) ?? 0;
  }
}

const legacyRuleTypes = new Map([
  ["import", CSSRule.IMPORT_RULE], ["media", CSSRule.MEDIA_RULE], ["font-face", CSSRule.FONT_FACE_RULE],
  ["page", CSSRule.PAGE_RULE], ["keyframes", CSSRule.KEYFRAMES_RULE], ["-webkit-keyframes", CSSRule.KEYFRAMES_RULE],
  ["namespace", CSSRule.NAMESPACE_RULE], ["counter-style", CSSRule.COUNTER_STYLE_RULE],
  ["supports", CSSRule.SUPPORTS_RULE], ["font-feature-values", CSSRule.FONT_FEATURE_VALUES_RULE],
]);

const lists = new WeakMap<CSSRuleList | StyleSheetList, () => readonly (CSSRule | CSSStyleSheet)[]>();

export class CSSRuleList implements Iterable<CSSRule> {
  readonly [index: number]: CSSRule;
  constructor(token?: symbol, read?: () => readonly CSSRule[]) {
    if (token !== internal || read === undefined) throw new TypeError("Illegal CSSRuleList constructor");
    const list = indexed(this, read);
    lists.set(list, read);
    return list;
  }
  get length(): number { return lists.get(this)!().length; }
  item(index: number): CSSRule | null { return this[index >>> 0] ?? null; }
  *[Symbol.iterator](): IterableIterator<CSSRule> { for (let index = 0; index < this.length; index++) yield this[index]; }
}

export class StyleSheetList implements Iterable<CSSStyleSheet> {
  readonly [index: number]: CSSStyleSheet;
  constructor(token?: symbol, read?: () => readonly CSSStyleSheet[]) {
    if (token !== internal || read === undefined) throw new TypeError("Illegal StyleSheetList constructor");
    const list = indexed(this, read);
    lists.set(list, read);
    return list;
  }
  get length(): number { return lists.get(this)!().length; }
  item(index: number): CSSStyleSheet | null { return this[index >>> 0] ?? null; }
  *[Symbol.iterator](): IterableIterator<CSSStyleSheet> { for (let index = 0; index < this.length; index++) yield this[index]; }
}

function rules(sheet: CSSStyleSheet): CSSRule[] {
  const state = sheets.get(sheet)!;
  return state.rules ??= (state.source?.read() ?? []).map(text => new CSSRule(internal, sheet, text));
}

function ruleOrder(rule: CSSRule): number {
  if (rule.type === CSSRule.IMPORT_RULE) return 1;
  if (rule.type === CSSRule.NAMESPACE_RULE) return 2;
  return /^@layer\b[^{}]*;\s*$/.test(rule.cssText) ? 0 : 3;
}

export class CSSStyleSheet {
  constructor(token?: symbol, state?: SheetState) {
    if (token !== internal || state === undefined) {
      throw new TypeError("Constructed stylesheets are not supported");
    }
    sheets.set(this, state);
  }

  get type(): string { return "text/css"; }
  get href(): string | null { return sheets.get(this)!.href; }
  get ownerNode(): Element | null { return sheets.get(this)!.owner; }
  get parentStyleSheet(): null { return null; }
  get ownerRule(): null { return null; }
  get title(): string | null { return this.ownerNode?.getAttribute("title") ?? null; }
  get disabled(): boolean { return sheets.get(this)!.disabled; }
  set disabled(value: boolean) {
    const state = sheets.get(this)!;
    state.disabled = Boolean(value);
    state.owner?.toggleAttribute("disabled", state.disabled);
  }

  get cssRules(): CSSRuleList {
    rules(this);
    const state = sheets.get(this)!;
    return state.list ??= new CSSRuleList(internal, () => rules(this));
  }

  insertRule(rule: string, index = 0): number {
    if (arguments.length === 0) throw new TypeError("insertRule requires a rule");
    const text = String(rule);
    const offset = index >>> 0;
    const current = rules(this);
    if (offset > current.length) throw new DOMException("Rule index is out of bounds", "IndexSizeError");
    const source = sheets.get(this)!.source;
    if (source === null) throw new DOMException("The stylesheet has been detached", "NotAllowedError");
    const serialized = source.parse(text);
    if (serialized === null) throw new DOMException("Expected one CSS rule", "SyntaxError");
    const entry = new CSSRule(internal, this, serialized);
    const order = ruleOrder(entry);
    if (order !== 0 && current.some((rule, position) => {
      const other = ruleOrder(rule);
      return other !== 0 && (position < offset ? other > order : other < order);
    })) throw new DOMException("Rule cannot be inserted at this position", "HierarchyRequestError");
    source.insert(offset, serialized);
    current.splice(offset, 0, entry);
    return offset;
  }

  deleteRule(index: number): void {
    if (arguments.length === 0) throw new TypeError("deleteRule requires an index");
    const offset = index >>> 0;
    const current = rules(this);
    if (offset >= current.length) throw new DOMException("Rule index is out of bounds", "IndexSizeError");
    const state = sheets.get(this)!;
    state.source?.remove(offset);
    ruleOwners.set(current[offset], null);
    current.splice(offset, 1);
  }
}

export function createSheet(owner: Element, href: string | null, source: SheetSource): CSSStyleSheet {
  return new CSSStyleSheet(internal, { owner, href, source, disabled: owner.hasAttribute("disabled") });
}

export function detachSheet(sheet: CSSStyleSheet): void {
  const state = sheets.get(sheet)!;
  state.owner = null;
  // A held sheet can still be edited after disassociation from its owner.
  const parse = state.source?.parse;
  state.source = parse === undefined ? null : { parse, read: () => [], insert() {}, remove() {} };
}

export function styleSheetList(read: () => readonly CSSStyleSheet[]): StyleSheetList {
  return new StyleSheetList(internal, read);
}
