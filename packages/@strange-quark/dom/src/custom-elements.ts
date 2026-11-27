// SPDX-License-Identifier: MIT

// Autonomous custom elements (HTML "Custom elements"). Customized built-ins
// are rejected the way Safari rejects them, so their feature checks fail.

import { inTemplateContents, shadowIncludingDescendants } from "./document.js";
import type { Document, Element, Node } from "./document.js";
import { DOMException } from "./events.js";

const HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";

type Callback = (...args: unknown[]) => unknown;
type LifecycleName = "connectedCallback" | "disconnectedCallback" | "adoptedCallback" | "attributeChangedCallback";
const lifecycleNames: readonly LifecycleName[] = ["connectedCallback", "disconnectedCallback", "adoptedCallback", "attributeChangedCallback"];
const formNames = ["formAssociatedCallback", "formResetCallback", "formDisabledCallback", "formStateRestoreCallback"];

const alreadyConstructed = Symbol("already constructed");

export type CustomElementDefinition = {
  readonly name: string;
  readonly localName: string;
  readonly constructor: Function;
  readonly document: Document;
  readonly observedAttributes: ReadonlySet<string>;
  /** `static disabledFeatures` lists "shadow": attachShadow() then throws. */
  readonly disableShadow: boolean;
  readonly callbacks: Readonly<Partial<Record<LifecycleName, Callback>>>;
  readonly constructionStack: (Element | typeof alreadyConstructed)[];
};

type Reaction = { upgrade: CustomElementDefinition } | { callback: Callback; args: unknown[] };
// Elements without a record are "undefined" or "uncustomized"; both may upgrade.
type ElementRecord = { state: "failed" | "precustomized" | "custom"; definition: CustomElementDefinition | null };

const records = new WeakMap<Element, ElementRecord>();
const reactionQueues = new WeakMap<Element, Reaction[]>();
const definitionsByConstructor = new WeakMap<Function, CustomElementDefinition>();
const registries = new WeakMap<Document, CustomElementRegistry>();

/** True once any registry has a definition; until then no reaction can exist. */
export let customElementsActive = false;

const reactionStack: Element[][] = [];
let backupQueue: Element[] = [];
let backupScheduled = false;

type RealmGlobal = {
  reportError?: (error: unknown) => void;
  console?: { error(error: unknown): void };
  queueMicrotask?: (callback: () => void) => void;
};
let realmGlobal: RealmGlobal = globalThis as RealmGlobal;

/** Reports an exception from a custom element constructor or reaction to the window. */
export function reportException(error: unknown): void {
  if (realmGlobal.reportError !== undefined) realmGlobal.reportError(error);
  else realmGlobal.console?.error(error);
}

function scheduleMicrotask(callback: () => void): void {
  if (realmGlobal.queueMicrotask !== undefined) realmGlobal.queueMicrotask(callback);
  else void Promise.resolve().then(callback);
}

const pcenChar = "-.0-9_a-z\\u00b7\\u00c0-\\u00d6\\u00d8-\\u00f6\\u00f8-\\u037d\\u037f-\\u1fff\\u200c\\u200d" +
  "\\u203f\\u2040\\u2070-\\u218f\\u2c00-\\u2fef\\u3001-\\ud7ff\\uf900-\\ufdcf\\ufdf0-\\ufffd\\u{10000}-\\u{effff}";
const potentialName = new RegExp(`^[a-z][${pcenChar}]*-[${pcenChar}]*$`, "u");
const reservedNames = new Set(["annotation-xml", "color-profile", "font-face", "font-face-src", "font-face-uri",
  "font-face-format", "font-face-name", "missing-glyph"]);

export function isValidCustomElementName(name: string): boolean {
  return potentialName.test(name) && !reservedNames.has(name);
}

function enqueueElement(element: Element): void {
  const current = reactionStack.at(-1);
  if (current !== undefined) {
    current.push(element);
    return;
  }
  backupQueue.push(element);
  if (backupScheduled) return;
  backupScheduled = true;
  scheduleMicrotask(() => {
    backupScheduled = false;
    const queue = backupQueue;
    backupQueue = [];
    invokeReactions(queue);
  });
}

function enqueueReaction(element: Element, reaction: Reaction): void {
  let queue = reactionQueues.get(element);
  if (queue === undefined) reactionQueues.set(element, queue = []);
  queue.push(reaction);
  enqueueElement(element);
}

function enqueueCallback(element: Element, name: LifecycleName, args: unknown[]): void {
  const definition = records.get(element)?.definition;
  const callback = definition?.callbacks[name];
  if (callback === undefined) return;
  if (name === "attributeChangedCallback" && !definition!.observedAttributes.has(args[0] as string)) return;
  enqueueReaction(element, { callback, args });
}

function invokeReactions(queue: Element[]): void {
  for (let index = 0; index < queue.length; index++) {
    const element = queue[index];
    const reactions = reactionQueues.get(element);
    while (reactions !== undefined && reactions.length !== 0) {
      const reaction = reactions.shift()!;
      try {
        if ("upgrade" in reaction) upgrade(element, reaction.upgrade);
        else Reflect.apply(reaction.callback, element, reaction.args);
      } catch (error) {
        reportException(error);
      }
    }
  }
}

/** Runs `operation` as a [CEReactions] IDL member. */
export function withCEReactions<T>(operation: () => T): T {
  reactionStack.push([]);
  try {
    return operation();
  } finally {
    const queue = reactionStack.pop()!;
    if (queue.length !== 0) invokeReactions(queue);
  }
}

/** Wraps a method or setter so that it behaves as a [CEReactions] member. */
export function ceWrap<T extends Function>(original: T): T {
  const wrapped = function (this: unknown): unknown {
    if (!customElementsActive) return Reflect.apply(original, this, arguments);
    reactionStack.push([]);
    try {
      return Reflect.apply(original, this, arguments);
    } finally {
      const queue = reactionStack.pop()!;
      if (queue.length !== 0) invokeReactions(queue);
    }
  };
  Object.defineProperty(wrapped, "name", { value: original.name });
  Object.defineProperty(wrapped, "length", { value: original.length });
  return wrapped as unknown as T;
}

/** Marks the named methods and attribute setters of `prototype` with [CEReactions]. */
export function ceReactions(prototype: object, names: readonly string[]): void {
  for (const name of names) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (descriptor === undefined) throw new Error(`[CEReactions] member ${name} is missing`);
    if (typeof descriptor.value === "function") descriptor.value = ceWrap(descriptor.value);
    else if (descriptor.set !== undefined) descriptor.set = ceWrap(descriptor.set);
    else throw new Error(`[CEReactions] member ${name} has no setter`);
    Object.defineProperty(prototype, name, descriptor);
  }
}

function upgrade(element: Element, definition: CustomElementDefinition): void {
  if (records.has(element)) return;
  records.set(element, { state: "failed", definition });
  for (const { localName, value, namespaceURI } of element.attributes) {
    enqueueCallback(element, "attributeChangedCallback", [localName, null, value, namespaceURI]);
  }
  if (element.isConnected) enqueueCallback(element, "connectedCallback", []);
  definition.constructionStack.push(element);
  try {
    records.get(element)!.state = "precustomized";
    const result = Reflect.construct(definition.constructor, []);
    if (result !== element) throw new TypeError("The custom element constructor did not produce the element being upgraded");
  } catch (error) {
    const record = records.get(element)!;
    record.state = "failed";
    record.definition = null;
    reactionQueues.get(element)?.splice(0);
    throw error;
  } finally {
    definition.constructionStack.pop();
  }
  records.get(element)!.state = "custom";
}

/** The definition `document` would use to create an element, if any. */
export function lookupDefinition(document: Document, namespace: string | null, localName: string): CustomElementDefinition | null {
  if (!customElementsActive || namespace !== HTML_NAMESPACE) return null;
  const registry = registries.get(document);
  return registry === undefined ? null : definitionNamed(registry, localName) ?? null;
}

export function isCustom(element: Element): boolean {
  return records.get(element)?.state === "custom";
}

export function markCustom(element: Element, definition: CustomElementDefinition): void {
  records.set(element, { state: "custom", definition });
}

export function markFailed(element: Element): void {
  records.set(element, { state: "failed", definition: null });
}

export function enqueueUpgrade(element: Element, definition: CustomElementDefinition): void {
  enqueueReaction(element, { upgrade: definition });
}

function tryUpgrade(element: Element): void {
  const definition = lookupDefinition(element.ownerDocument, element.namespaceURI, element.localName);
  if (definition !== null) enqueueUpgrade(element, definition);
}

/** Insertion steps for one inclusive descendant of an inserted node. */
export function customElementInserted(node: Node, connected: boolean): void {
  if (!customElementsActive || !connected || !isElement(node)) return;
  if (isCustom(node)) enqueueCallback(node, "connectedCallback", []);
  else tryUpgrade(node);
}

/** Removing steps for one inclusive descendant of a removed node. */
export function customElementRemoved(node: Node, parentConnected: boolean): void {
  if (customElementsActive && parentConnected && isElement(node) && isCustom(node)) {
    enqueueCallback(node, "disconnectedCallback", []);
  }
}

export function customElementAdopted(node: Node, oldDocument: Document, newDocument: Document): void {
  if (customElementsActive && isElement(node) && isCustom(node)) {
    enqueueCallback(node, "adoptedCallback", [oldDocument, newDocument]);
  }
}

export function customElementAttributeChanged(element: Element, localName: string, oldValue: string | null,
                                              value: string | null, namespace: string | null): void {
  if (customElementsActive && isCustom(element)) {
    enqueueCallback(element, "attributeChangedCallback", [localName, oldValue, value, namespace]);
  }
}

function isElement(node: Node): node is Element {
  return node.nodeType === 1;
}

/**
 * The HTML element constructor steps for a NewTarget that is not a built-in
 * interface. Returns the element being upgraded, or the definition to create.
 */
export function htmlElementConstructor(newTarget: Function, isBuiltin: (constructor: Function) => boolean,
                                       base: Function): CustomElementDefinition | Element {
  if (isBuiltin(newTarget)) throw new TypeError("Illegal constructor");
  const definition = definitionsByConstructor.get(newTarget);
  if (definition === undefined) throw new TypeError("Illegal constructor");
  // Built-in interfaces share this constructor, so the active function object
  // is the nearest built-in that NewTarget inherits from. A NewTarget with no
  // built-in ancestor reached it through Reflect.construct(HTMLElement, ...).
  let active: unknown = newTarget;
  while (typeof active === "function" && !isBuiltin(active)) active = Object.getPrototypeOf(active);
  if (typeof active === "function" && active !== base) throw new TypeError("Illegal constructor");
  const stack = definition.constructionStack;
  if (stack.length === 0) return definition;
  const element = stack.at(-1)!;
  if (element === alreadyConstructed) throw new TypeError("This custom element was already constructed");
  const prototype = (newTarget as { prototype?: unknown }).prototype;
  Object.setPrototypeOf(element, typeof prototype === "object" && prototype !== null ? prototype : (base as { prototype: object }).prototype);
  stack[stack.length - 1] = alreadyConstructed;
  return element;
}

const registryToken = Symbol("CustomElementRegistry constructor");

function toStringSequence(value: unknown): string[] {
  if (typeof value !== "object" || value === null || typeof (value as Iterable<unknown>)[Symbol.iterator] !== "function") {
    throw new TypeError("The value is not iterable");
  }
  return [...(value as Iterable<unknown>)].map(String);
}

// A proxy is a constructor exactly when its target is, and its construct
// trap keeps the check from reading anything on the candidate.
const constructProbe = { construct: () => ({}) };
function isConstructor(value: Function): boolean {
  try {
    new (new Proxy(value, constructProbe) as new () => object)();
    return true;
  } catch {
    return false;
  }
}

let definitionNamed: (registry: CustomElementRegistry, name: string) => CustomElementDefinition | undefined;

export class CustomElementRegistry {
  readonly #document: Document;
  readonly #byName = new Map<string, CustomElementDefinition>();
  readonly #byConstructor = new Map<Function, CustomElementDefinition>();
  readonly #whenDefined = new Map<string, { promise: Promise<Function>; resolve: (constructor: Function) => void }>();
  #running = false;

  static {
    definitionNamed = (registry, name) => registry.#byName.get(name);
  }

  constructor(token: symbol, document: Document) {
    if (token !== registryToken) throw new TypeError("Illegal constructor");
    this.#document = document;
  }

  define(name: string, constructor: Function, options?: { extends?: string } | null): void {
    if (arguments.length < 2) throw new TypeError("customElements.define requires a name and a constructor");
    name = String(name);
    if (typeof constructor !== "function") throw new TypeError("The custom element constructor is not a function");
    const extendsName = options?.extends === undefined ? undefined : String(options.extends);
    reactionStack.push([]);
    try {
      if (!isConstructor(constructor)) throw new TypeError("The custom element constructor is not a constructor");
      if (!isValidCustomElementName(name)) throw new DOMException(`"${name}" is not a valid custom element name`, "SyntaxError");
      if (this.#byName.has(name)) throw new DOMException(`"${name}" has already been defined`, "NotSupportedError");
      if (this.#byConstructor.has(constructor)) {
        throw new DOMException("This constructor has already been used with this registry", "NotSupportedError");
      }
      if (extendsName !== undefined) throw new DOMException("Customized built-in elements are not supported", "NotSupportedError");
      if (this.#running) throw new DOMException("A custom element definition is already running", "NotSupportedError");
      this.#running = true;
      const callbacks: Partial<Record<LifecycleName, Callback>> = {};
      let observedAttributes = new Set<string>();
      let disableShadow = false;
      try {
        const prototype = (constructor as { prototype?: unknown }).prototype;
        if (typeof prototype !== "object" || prototype === null) throw new TypeError("The constructor prototype is not an object");
        const callback = (key: string): Callback | undefined => {
          const value = (prototype as Record<string, unknown>)[key];
          if (value === undefined) return undefined;
          if (typeof value !== "function") throw new TypeError(`${key} is not a function`);
          return value as Callback;
        };
        for (const key of lifecycleNames) {
          const value = callback(key);
          if (value !== undefined) callbacks[key] = value;
        }
        if (callbacks.attributeChangedCallback !== undefined) {
          const observed = (constructor as { observedAttributes?: unknown }).observedAttributes;
          if (observed !== undefined) observedAttributes = new Set(toStringSequence(observed));
        }
        const disabled = (constructor as { disabledFeatures?: unknown }).disabledFeatures;
        if (disabled !== undefined) disableShadow = toStringSequence(disabled).includes("shadow");
        if ((constructor as { formAssociated?: unknown }).formAssociated) for (const key of formNames) callback(key);
      } finally {
        this.#running = false;
      }
      const definition: CustomElementDefinition = {
        name, localName: name, constructor, document: this.#document, observedAttributes, disableShadow, callbacks,
        constructionStack: [],
      };
      this.#byName.set(name, definition);
      this.#byConstructor.set(constructor, definition);
      definitionsByConstructor.set(constructor, definition);
      customElementsActive = true;
      for (const node of shadowIncludingDescendants(this.#document)) {
        if (isElement(node) && node.namespaceURI === HTML_NAMESPACE && node.localName === name) enqueueUpgrade(node, definition);
      }
      const pending = this.#whenDefined.get(name);
      if (pending !== undefined) {
        this.#whenDefined.delete(name);
        pending.resolve(constructor);
      }
    } finally {
      const queue = reactionStack.pop()!;
      if (queue.length !== 0) invokeReactions(queue);
    }
  }

  get(name: string): Function | undefined {
    if (arguments.length === 0) throw new TypeError("customElements.get requires a name");
    return this.#byName.get(String(name))?.constructor;
  }

  getName(constructor: Function): string | null {
    if (typeof constructor !== "function") throw new TypeError("customElements.getName requires a constructor");
    return this.#byConstructor.get(constructor)?.name ?? null;
  }

  whenDefined(name: string): Promise<Function> {
    if (arguments.length === 0) return Promise.reject(new TypeError("customElements.whenDefined requires a name"));
    name = String(name);
    if (!isValidCustomElementName(name)) {
      return Promise.reject(new DOMException(`"${name}" is not a valid custom element name`, "SyntaxError"));
    }
    const definition = this.#byName.get(name);
    if (definition !== undefined) return Promise.resolve(definition.constructor);
    let pending = this.#whenDefined.get(name);
    if (pending === undefined) {
      const { promise, resolve } = Promise.withResolvers<Function>();
      this.#whenDefined.set(name, pending = { promise, resolve });
    }
    return pending.promise;
  }

  upgrade(root: Node): void {
    if (arguments.length === 0 || typeof root !== "object" || root === null || typeof root.nodeType !== "number") {
      throw new TypeError("customElements.upgrade requires a Node");
    }
    if (inTemplateContents(root)) return;
    withCEReactions(() => {
      for (const node of [root, ...shadowIncludingDescendants(root)]) if (isElement(node)) tryUpgrade(node);
    });
  }
}
Object.defineProperty(CustomElementRegistry.prototype, Symbol.toStringTag, { value: "CustomElementRegistry", configurable: true });

/** Gives the window of `document` its CustomElementRegistry. */
export function installCustomElements(global: Record<PropertyKey, unknown>, document: Document): CustomElementRegistry {
  const registry = new CustomElementRegistry(registryToken, document);
  registries.set(document, registry);
  realmGlobal = global as RealmGlobal;
  Object.defineProperty(global, "customElements", { value: registry, configurable: true, enumerable: true, writable: true });
  global.CustomElementRegistry = CustomElementRegistry;
  return registry;
}
