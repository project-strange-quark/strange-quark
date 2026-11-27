// SPDX-License-Identifier: MIT

import { canvasContext, canvasDataURL, canvasToBlob, resizeCanvas } from "./canvas.js";
import type { CanvasRenderingContext2D } from "./canvas.js";

import { AtomKind, ElementState, FocusFlags, Journal, NO_STRING, NodeType, Opcode, RECORD_BYTES, SELECTION_BACKWARD, STYLE_IMPORTANT } from "./journal.js";
import type { JournalAtom } from "./journal.js";
import {
  aliasEventTarget, CompositionEvent, createLegacyEvent, CustomEvent, defaultPassive, dispatchTrustedEvent, DOMException,
  ErrorEvent, Event, eventActivation, EventTarget, eventParent, FocusEvent, InputEvent, installWindowEventTarget,
  KeyboardEvent, listenerFlagsChanged, MouseEvent, observeListenerFlags, PointerEvent, ProgressEvent, SubmitEvent, UIEvent, WheelEvent,
  Window,
} from "./events.js";
import { dataset, DOMTokenList, HTMLCollection, NodeList } from "./collections.js";
import { FileList, fileList } from "./file-list.js";
import { renderedInnerText } from "./inner-text.js";
import type { DOMFile } from "./file-list.js";
import { AbortController, AbortSignal } from "./abort.js";
import { computedStyle, CSSStyleDeclaration } from "./style.js";
import { NodeFilter, TreeWalker, treeWalker } from "./tree-walker.js";
import { MutationObserver, MutationRecord, observeDetachedSubtree, queueMutation } from "./mutation.js";
import {
  ceReactions, ceWrap, customElementAdopted, customElementAttributeChanged, customElementInserted, customElementRemoved,
  customElementsActive, enqueueUpgrade, htmlElementConstructor, installCustomElements, isValidCustomElementName,
  lookupDefinition, markCustom, markFailed, reportException,
} from "./custom-elements.js";
import type { CustomElementDefinition } from "./custom-elements.js";

import {
  createSheet, CSSRule, CSSRuleList, CSSStyleSheet, detachSheet, styleSheetList, StyleSheetList,
} from "./stylesheet.js";

const HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";
const SVG_NAMESPACE = "http://www.w3.org/2000/svg";
const XML_NAMESPACE = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE = "http://www.w3.org/2000/xmlns/";
const MAX_SLOT = 0x00ff_ffff;
const ELEMENT = 1;
const TEXT = 3;

type QuirksMode = 0 | 1 | 2;
type NamedProperty = (node: Node, name: string) => unknown;
type ReadyState = "loading" | "interactive" | "complete";
type BootstrapBatch = readonly [records: ArrayBuffer, strings: readonly string[]];
type Attribute = {
  name: string;
  localName: string;
  prefix: string | null;
  namespaceURI: string | null;
  value: string;
};

type NodeState = {
  id: number;
  owner: Document | null;
  parent: Node | null;
  children?: Node[];
  value: string | null;
  templateContent?: true;
  tracked?: TrackedNode;
  attributes?: Map<string, Attribute>;
};
const nodes = new WeakMap<Node, NodeState>();
const noChildren: readonly Node[] = [];

type Allocator = { nextSlot: number };
const allocators = new WeakMap<Journal, Allocator>();
type TrackedNode = { id: number; ref: WeakRef<Node>; parent: number; tracker: NodeTracker };
type NodeTracker = {
  journal: Journal;
  index: Map<number, TrackedNode>;
  decodingImages?: Set<HTMLImageElement>;
};
const runtimeNodes = new WeakMap<Journal, NodeTracker>();

const cleanupNodes = new FinalizationRegistry<TrackedNode>(
  ({ id, tracker: { index, journal } }) => {
    // Mirror release detaches children. A parent and child retain each other
    // in JS, so an unreachable child can release its unreachable ancestors first.
    const chain: TrackedNode[] = [];
    let entry = index.get(id);
    while (entry !== undefined) {
      if (entry.ref.deref() !== undefined) return;
      chain.push(entry);
      entry = index.get(entry.parent);
    }
    for (const node of chain.reverse()) {
      journal.release(node.id);
      index.delete(node.id);
    }
  },
);

function rememberNode(journal: Journal | null, state: NodeState, node: Node, finalize: boolean): void {
  if (journal === null) return;
  const tracker = runtimeNodes.get(journal);
  if (tracker === undefined) return;
  const { id } = state;
  const tracked: TrackedNode = { id, ref: new WeakRef(node), parent: 0, tracker };
  state.tracked = tracked;
  tracker.index.set(id, tracked);
  // The document belongs to the engine until the isolate is torn down.
  if (finalize) cleanupNodes.register(node, tracked);
}

type DocumentState = {
  journal: Journal | null;
  allocator: Allocator | null;
  quirks: QuirksMode;
  html: boolean;
  contentType: string;
  view: EventTarget | null;
  loadScript?: (script: HTMLScriptElement) => void;
  platform?: PlatformActions;
  navigateLink?: (url: string, blank: boolean) => void;
  active: HTMLElement | null;
  focusChain: Element[];
  focusRevision: number;
  selection?: Selection;
  focusVisible: boolean;
  keyboardModality: boolean;
  inputDetached?: (child: Node) => void;
  url: string;
  ready: ReadyState;
  sheetOwners: Map<number, Element>;
  styleSheets?: StyleSheetList;
  cssom?: SheetHost;
  bindings?: DocumentBindings;
};
type PlatformActions = {
  clipboard?: (target: Element, key: string, time: number) => void;
  open?: (accept: string[], multiple: boolean) => Promise<DOMFile[] | null>;
  save?: (url: string, name: string) => void;
  openExternal?: (url: string) => void;
  report: (error: unknown) => void;
};

export function installPlatformActions(document: Document, actions: PlatformActions): void {
  documents.get(document)!.platform = actions;
}

export function installLinkNavigation(document: Document, navigate: (url: string, blank: boolean) => void): void {
  documents.get(document)!.navigateLink = navigate;
}

export function openExternal(document: Document, url: string): void {
  documents.get(document)!.platform?.openExternal?.(url);
}

const documents = new WeakMap<Document, DocumentState>();
let realmDocument: Document | undefined;

function nodeState(node: Node): NodeState { return nodes.get(node)!; }

function childrenOf(node: Node): readonly Node[] { return nodeState(node).children ?? noChildren; }

function ownerOf(node: Node): Document {
  return node instanceof Document ? node : nodeState(node).owner!;
}

function defaultDocument(): Document {
  return realmDocument ??= createDocument(new Journal());
}

function lowerAscii(text: string): string {
  return text.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

function upperAscii(text: string): string {
  return text.replace(/[a-z]/g, (letter) => letter.toUpperCase());
}

function allocate(document: Document): number {
  const { allocator, journal } = documents.get(document)!;
  if (journal === null || allocator === null) return 0;
  const recycled = journal.takeRecycledNodeId();
  if (recycled !== undefined) return recycled;
  if (allocator.nextSlot > MAX_SLOT) throw new RangeError("The document ran out of node slots");
  return allocator.nextSlot++;
}

function journalFor(node: Node): Journal | null { return documents.get(ownerOf(node))!.journal; }

export function nodeId(node: Node): number {
  const state = nodeState(node);
  if (state.id === 0) {
    const journal = journalFor(node);
    if (journal === null) return 0;
    // A packed document already has its ID. A standalone document enters the
    // journal only when it first becomes a mutation target.
    state.id = allocate(ownerOf(node));
    journal.record(Opcode.CreateNode, state.id, NodeType.Document, NO_STRING);
    rememberNode(journal, state, node, false);
  }
  return state.id;
}

function fail(name: string, message: string): never { throw new DOMException(message, name); }

const nodeLists = new WeakMap<Node, NodeList<Node>>();
const elementLists = new WeakMap<Node, HTMLCollection<Element>>();

export class Node extends EventTarget {
  static readonly ELEMENT_NODE = 1;
  static readonly ATTRIBUTE_NODE = 2;
  static readonly TEXT_NODE = 3;
  static readonly CDATA_SECTION_NODE = 4;
  static readonly ENTITY_REFERENCE_NODE = 5;
  static readonly ENTITY_NODE = 6;
  static readonly PROCESSING_INSTRUCTION_NODE = 7;
  static readonly COMMENT_NODE = 8;
  static readonly DOCUMENT_NODE = 9;
  static readonly DOCUMENT_TYPE_NODE = 10;
  static readonly DOCUMENT_FRAGMENT_NODE = 11;
  static readonly NOTATION_NODE = 12;
  static readonly DOCUMENT_POSITION_DISCONNECTED = 1;
  static readonly DOCUMENT_POSITION_PRECEDING = 2;
  static readonly DOCUMENT_POSITION_FOLLOWING = 4;
  static readonly DOCUMENT_POSITION_CONTAINS = 8;
  static readonly DOCUMENT_POSITION_CONTAINED_BY = 16;
  static readonly DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC = 32;

  readonly nodeType: number;

  protected constructor(id: number, type: number, owner: Document | null, value: string | null = null,
                        named?: NamedProperty) {
    super();
    if (new.target === Node) throw new TypeError("Illegal Node constructor");
    this.nodeType = type;
    // The proxy must be returned from super() so subclass private members
    // are installed on the same object that script and event dispatch see.
    const node: Node = named === undefined ? this : new Proxy(this, {
      get(target, key, receiver) {
        if (typeof key === "string" && !Reflect.has(target, key)) {
          const item = named(receiver as Node, key);
          if (item !== null && item !== undefined) return item;
        }
        return Reflect.get(target, key, receiver);
      },
      has(target, key) {
        return Reflect.has(target, key) || typeof key === "string" && named(node, key) != null;
      },
    });
    if (node !== this) aliasEventTarget(this, node);
    const state = { id, owner, parent: null, value };
    nodes.set(node, state);
    if (owner !== null) rememberNode(documents.get(owner)!.journal, state, node, true);
    return node;
  }

  override [listenerFlagsChanged](flags: number): void {
    journalFor(this)?.record(Opcode.Listeners, nodeId(this), flags, 0);
  }

  override [defaultPassive](): boolean {
    return this instanceof Document || this instanceof Element && this.ownerDocument.body === this;
  }

  override [eventParent](event: Event): EventTarget | null {
    if (this instanceof Document) return event.type === "load" ? null : this.defaultView;
    return this.parentNode;
  }

  get nodeName(): string {
    switch (this.nodeType) {
      case TEXT: return "#text";
      case NodeType.Comment: return "#comment";
      case NodeType.Document: return "#document";
      case NodeType.DocumentFragment: return "#document-fragment";
      default: throw new TypeError("Unknown node kind");
    }
  }

  get ownerDocument(): Document | null { return nodeState(this).owner; }
  get parentNode(): Node | null { return nodeState(this).parent; }
  get parentElement(): Element | null { return this.parentNode instanceof Element ? this.parentNode : null; }
  get firstChild(): Node | null { return childrenOf(this)[0] ?? null; }
  get lastChild(): Node | null { return childrenOf(this).at(-1) ?? null; }
  get nextSibling(): Node | null { return sibling(this, 1); }
  get previousSibling(): Node | null { return sibling(this, -1); }
  get isConnected(): boolean { return this.getRootNode() instanceof Document; }

  get childNodes(): NodeList<Node> {
    let list = nodeLists.get(this);
    if (list === undefined) {
      list = new NodeList(() => childrenOf(this));
      nodeLists.set(this, list);
    }
    return list;
  }

  hasChildNodes(): boolean { return this.firstChild !== null; }

  getRootNode(_options?: { composed?: boolean }): Node {
    let root: Node = this;
    while (root.parentNode !== null) root = root.parentNode;
    return root;
  }

  contains(other: Node | null): boolean {
    if (other == null) return false;
    if (!nodes.has(other)) throw new TypeError("Expected a Node");
    for (let node: Node | null = other; node !== null; node = node.parentNode) {
      if (node === this) return true;
    }
    return false;
  }

  isSameNode(other: Node | null): boolean { return this === other; }

  compareDocumentPosition(other: Node): number {
    expectNode(other);
    if (other === this) return 0;
    const path = (node: Node): Node[] => {
      const result: Node[] = [];
      for (let current: Node | null = node; current !== null; current = current.parentNode) result.push(current);
      return result.reverse();
    };
    const first = path(this);
    const second = path(other);
    if (first[0] !== second[0]) {
      return Node.DOCUMENT_POSITION_DISCONNECTED | Node.DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC |
        (comparisonOrder(first[0]) < comparisonOrder(second[0]) ?
          Node.DOCUMENT_POSITION_FOLLOWING : Node.DOCUMENT_POSITION_PRECEDING);
    }
    let shared = 1;
    while (first[shared] !== undefined && first[shared] === second[shared]) shared++;
    if (shared === first.length) return Node.DOCUMENT_POSITION_FOLLOWING | Node.DOCUMENT_POSITION_CONTAINED_BY;
    if (shared === second.length) return Node.DOCUMENT_POSITION_PRECEDING | Node.DOCUMENT_POSITION_CONTAINS;
    const siblings = childrenOf(first[shared - 1]);
    return siblings.indexOf(first[shared]) < siblings.indexOf(second[shared]) ?
      Node.DOCUMENT_POSITION_FOLLOWING : Node.DOCUMENT_POSITION_PRECEDING;
  }

  get nodeValue(): string | null { return nodeState(this).value; }
  set nodeValue(value: string | null) {
    const state = nodeState(this);
    if (state.value !== null) {
      const oldValue = state.value;
      state.value = value == null ? "" : String(value);
      journalFor(this)?.record(Opcode.SetText, nodeId(this), state.value, 0);
      if (this instanceof Text && this.parentNode instanceof HTMLStyleElement) syncStyleText(this.parentNode, true);
      queueMutation(this, { type: "characterData", oldValue });
    }
  }

  get textContent(): string | null {
    if (this.nodeType === NodeType.Document || this.nodeType === NodeType.DocumentType || this.nodeValue !== null) {
      return this.nodeValue;
    }
    const text: string[] = [];
    for (const node of descendants(this)) if (node instanceof Text) text.push(node.data);
    return text.join("");
  }

  set textContent(value: string | null) {
    if (this.nodeValue !== null) this.nodeValue = value;
    else if (this instanceof Element || this instanceof DocumentFragment) {
      const text = value == null ? "" : String(value);
      this.replaceChildren(...(text === "" ? [] : [text]));
    }
  }

  appendChild<T extends Node>(child: T): T { return this.insertBefore(child, null); }

  insertBefore<T extends Node>(child: T, before: Node | null): T {
    if (arguments.length < 2) throw new TypeError("insertBefore requires a child and reference");
    mutate(this, [child], before ?? null);
    return child;
  }

  removeChild<T extends Node>(child: T): T {
    expectNode(child);
    if (child.parentNode !== this) fail("NotFoundError", "The node to be removed is not a child of this node.");
    unlink(this, child);
    return child;
  }

  replaceChild<T extends Node>(child: Node, oldChild: T): T {
    expectNode(oldChild);
    if (oldChild.parentNode !== this) fail("NotFoundError", "The node to be removed is not a child of this node.");
    mutate(this, [child], oldChild, [oldChild]);
    return oldChild;
  }

  normalize(): void {
    let previous: Text | null = null;
    for (const child of [...this.childNodes]) {
      if (child instanceof Text) {
        if (child.length === 0) this.removeChild(child);
        else if (previous !== null) {
          previous.appendData(child.data);
          this.removeChild(child);
        } else previous = child;
      } else {
        previous = null;
        child.normalize();
      }
    }
  }

  cloneNode(deep = false): Node {
    return cloneNodeInto(this, ownerOf(this), Boolean(deep), !inTemplateContents(this));
  }
}

for (const key of Object.keys(Node).filter((name) => name.endsWith("_NODE") || name.startsWith("DOCUMENT_POSITION_"))) {
  Object.defineProperty(Node.prototype, key, { value: Node[key as keyof typeof Node], enumerable: true });
}

const comparisonIds = new WeakMap<Node, number>();
let nextComparisonId = 0;

function comparisonOrder(node: Node): number {
  let id = comparisonIds.get(node);
  if (id === undefined) {
    id = nextComparisonId++;
    comparisonIds.set(node, id);
  }
  return id;
}

function expectNode(value: unknown): asserts value is Node {
  if (typeof value !== "object" || value === null || !nodes.has(value as Node)) throw new TypeError("Expected a Node");
}

function sibling(node: Node, direction: number): Node | null {
  const parent = node.parentNode;
  if (parent === null) return null;
  const siblings = childrenOf(parent);
  return siblings[siblings.indexOf(node) + direction] ?? null;
}

export function* descendants(parent: Node): IterableIterator<Node> {
  const roots = childrenOf(parent);
  if (roots.length === 0) return;
  const pending = roots.slice().reverse();
  while (pending.length !== 0) {
    const node = pending.pop()!;
    yield node;
    const children = childrenOf(node);
    for (let index = children.length - 1; index >= 0; index--) pending.push(children[index]);
  }
}

abstract class ParentNode extends Node {
  get children(): HTMLCollection<Element> {
    let list = elementLists.get(this);
    if (list === undefined) {
      list = new HTMLCollection(() => childrenOf(this).filter((child) => child instanceof Element));
      elementLists.set(this, list);
    }
    return list;
  }

  get childElementCount(): number { return this.children.length; }
  get firstElementChild(): Element | null { return this.children.item(0); }
  get lastElementChild(): Element | null { return this.children.item(this.children.length - 1); }

  append(...nodes: (Node | string)[]): void { mutate(this, convertNodes(this, nodes), null); }
  prepend(...nodes: (Node | string)[]): void { mutate(this, convertNodes(this, nodes), this.firstChild); }
  replaceChildren(...nodes: (Node | string)[]): void {
    mutate(this, convertNodes(this, nodes), null, [...this.childNodes]);
  }
}

const collections = new WeakMap<Node, Map<string, HTMLCollection<Element>>>();

function collection(node: Node, key: string, filter: (element: Element) => boolean): HTMLCollection<Element> {
  let cache = collections.get(node);
  if (cache === undefined) collections.set(node, cache = new Map());
  let list = cache.get(key);
  if (list === undefined) {
    list = new HTMLCollection(() => [...descendants(node)].filter(
      (child): child is Element => child instanceof Element && filter(child),
    ));
    cache.set(key, list);
  }
  return list;
}

function byTag(node: Node, name: string): HTMLCollection<Element> {
  return collection(node, "tag:" + name, child => {
    const qualified = child.prefix === null ? child.localName : child.prefix + ":" + child.localName;
    return name === "*" || qualified === (documents.get(ownerOf(node))!.html &&
      child.namespaceURI === HTML_NAMESPACE ? lowerAscii(name) : name);
  });
}

function byTagNS(node: Node, namespace: string | null, name: string): HTMLCollection<Element> {
  const ns = namespace == null || namespace === "" ? null : String(namespace);
  return collection(node, "ns:" + JSON.stringify([ns, name]), child =>
    (ns === "*" || child.namespaceURI === ns) && (name === "*" || child.localName === name));
}

function byClass(node: Node, names: string): HTMLCollection<Element> {
  const split = (value: string) => value.split(/[\t\n\f\r ]+/).filter(Boolean);
  const classes = split(names);
  return collection(node, "class:" + names, child => {
    const quirks = documents.get(ownerOf(node))!.quirks === 2;
    const normal = (value: string) => quirks ? lowerAscii(value) : value;
    const available = new Set(split(child.className).map(normal));
    return classes.length !== 0 && classes.every(name => available.has(normal(name)));
  });
}

function convertNodes(context: Node, values: (Node | string)[]): Node[] {
  const document = ownerOf(context);
  return values.map((value) => nodes.has(value as Node) ? value as Node : document.createTextNode(String(value)));
}

function neighborElement(node: Node, direction: number): Element | null {
  for (let candidate = sibling(node, direction); candidate !== null; candidate = sibling(candidate, direction)) {
    if (candidate instanceof Element) return candidate;
  }
  return null;
}

function relativeInsert(node: Node, values: (Node | string)[], position: "before" | "after" | "replace"): void {
  const parent = node.parentNode;
  if (parent === null) return;
  const incoming = convertNodes(node, values);
  mutate(parent, incoming, position === "after" ? node.nextSibling : node, position === "replace" ? [node] : []);
}

export abstract class CharacterData extends Node {
  get data(): string { return this.nodeValue!; }
  set data(value: string) { this.nodeValue = value === null ? "" : String(value); }
  get length(): number { return this.data.length; }
  get nextElementSibling(): Element | null { return neighborElement(this, 1); }
  get previousElementSibling(): Element | null { return neighborElement(this, -1); }
  before(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "before"); }
  after(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "after"); }
  replaceWith(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "replace"); }
  remove(): void { this.parentNode?.removeChild(this); }

  appendData(data: string): void {
    if (arguments.length < 1) throw new TypeError("appendData requires data");
    this.data += String(data);
  }

  substringData(offset: number, count: number): string {
    if (arguments.length < 2) throw new TypeError("substringData requires offset and count");
    offset >>>= 0;
    count >>>= 0;
    if (offset > this.length) fail("IndexSizeError", "Offset exceeds the data length");
    return this.data.slice(offset, offset + count);
  }

  insertData(offset: number, data: string): void {
    if (arguments.length < 2) throw new TypeError("insertData requires offset and data");
    this.replaceData(offset, 0, data);
  }

  deleteData(offset: number, count: number): void {
    if (arguments.length < 2) throw new TypeError("deleteData requires offset and count");
    this.replaceData(offset, count, "");
  }

  replaceData(offset: number, count: number, data: string): void {
    if (arguments.length < 3) throw new TypeError("replaceData requires offset, count and data");
    offset >>>= 0;
    count >>>= 0;
    data = String(data);
    if (offset > this.length) fail("IndexSizeError", "Offset exceeds the data length");
    this.data = this.data.slice(0, offset) + data + this.data.slice(offset + count);
  }
}

export class Text extends CharacterData {
  constructor(data = "", owner = defaultDocument(), id?: number) {
    const value = String(data);
    const slot = id ?? allocate(owner);
    super(slot, TEXT, owner, value);
    if (id === undefined) documents.get(owner)!.journal?.record(Opcode.CreateText, slot, value, 0);
  }

  splitText(offset: number): Text {
    if (arguments.length === 0) throw new TypeError("splitText requires offset");
    offset >>>= 0;
    if (offset > this.length) fail("IndexSizeError", "Offset exceeds the data length");
    const split = ownerOf(this).createTextNode(this.data.slice(offset));
    this.parentNode?.insertBefore(split, this.nextSibling);
    this.data = this.data.slice(0, offset);
    return split;
  }

  get wholeText(): string {
    let first: Text = this;
    while (first.previousSibling instanceof Text) first = first.previousSibling;
    let value = "";
    for (let node: Node | null = first; node instanceof Text; node = node.nextSibling) value += node.data;
    return value;
  }
}

export class Comment extends CharacterData {
  constructor(data = "", owner = defaultDocument(), id?: number) {
    const value = String(data);
    const slot = id ?? allocate(owner);
    super(slot, NodeType.Comment, owner, value);
    if (id === undefined) documents.get(owner)!.journal?.record(Opcode.CreateNode, slot, NodeType.Comment, value);
  }
}

export class DocumentType extends Node {
  readonly name: string;
  readonly publicId: string;
  readonly systemId: string;

  constructor(id: number, owner: Document, name: string, publicId = "", systemId = "") {
    super(id, NodeType.DocumentType, owner);
    this.name = name;
    this.publicId = publicId;
    this.systemId = systemId;
  }

  override get nodeName(): string { return this.name; }
  remove(): void { this.parentNode?.removeChild(this); }
}

function byId(parent: Node, id: string): Element | null {
  if (id === "") return null;
  for (const node of descendants(parent)) if (node instanceof Element && node.getAttributeNS(null, "id") === id) return node;
  return null;
}

export class DocumentFragment extends ParentNode {
  constructor(owner = defaultDocument(), id?: number, templateId = NO_STRING) {
    const slot = id ?? allocate(owner);
    super(slot, NodeType.DocumentFragment, owner);
    if (id === undefined) journalFor(this)?.record(Opcode.CreateNode, slot, NodeType.DocumentFragment, templateId);
  }

  getElementById(id: string): Element | null { return byId(this, String(id)); }
}

function validateLocalName(name: string, element: boolean): string {
  if (name === "" || (element ? /[\0\t\n\f\r <>/]/ : /[\0\t\n\f\r />=]/).test(name)) {
    fail("InvalidCharacterError", "Invalid DOM name");
  }
  return name;
}

function qualifiedName(namespace: string | null, qualified: string): { prefix: string | null; localName: string } {
  const qname = /^[\p{L}_][\p{L}\p{N}\p{M}_\-.\u00b7]*(?::[\p{L}_][\p{L}\p{N}\p{M}_\-.\u00b7]*)?$/u;
  if (!qname.test(qualified)) fail("InvalidCharacterError", "Invalid qualified name");
  const split = qualified.indexOf(":");
  const prefix = split === -1 ? null : qualified.slice(0, split);
  const localName = qualified.slice(split + 1);
  if ((prefix !== null && namespace === null) || (prefix === "xml" && namespace !== XML_NAMESPACE) ||
      ((qualified === "xmlns" || prefix === "xmlns") !== (namespace === XMLNS_NAMESPACE))) {
    fail("NamespaceError", "The qualified name does not match the namespace");
  }
  return { prefix, localName };
}

const classLists = new WeakMap<Element, DOMTokenList>();
const styles = new WeakMap<Element, CSSStyleDeclaration>();
const datasets = new WeakMap<Element, Record<string, string>>();
const attributeLists = new WeakMap<Element, NodeList<Attribute>>();

export class Element extends ParentNode {
  readonly localName: string;
  readonly namespaceURI: string | null;
  readonly prefix: string | null;

  constructor(localName: string, namespace: string | null, id: number, owner: Document, prefix: string | null = null,
              named?: NamedProperty) {
    super(id, ELEMENT, owner, null, named);
    if (!documents.has(owner)) throw new TypeError("Illegal Element constructor");
    this.localName = localName;
    this.namespaceURI = namespace;
    this.prefix = prefix;
  }

  override get ownerDocument(): Document { return nodeState(this).owner!; }

  #html(): boolean { return this.namespaceURI === HTML_NAMESPACE && documents.get(this.ownerDocument)!.html; }
  #normalName(name: string): string { return this.#html() ? lowerAscii(String(name)) : String(name); }

  override get nodeName(): string { return this.tagName; }
  get tagName(): string {
    const qualified = this.prefix === null ? this.localName : this.prefix + ":" + this.localName;
    return this.#html() ? upperAscii(qualified) : qualified;
  }

  get nextElementSibling(): Element | null { return neighborElement(this, 1); }
  get previousElementSibling(): Element | null { return neighborElement(this, -1); }
  before(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "before"); }
  after(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "after"); }
  replaceWith(...nodes: (Node | string)[]): void { relativeInsert(this, nodes, "replace"); }
  remove(): void { this.parentNode?.removeChild(this); }
  getElementsByTagName(name: string): HTMLCollection<Element> { return byTag(this, String(name)); }
  getElementsByTagNameNS(namespace: string | null, name: string): HTMLCollection<Element> { return byTagNS(this, namespace, String(name)); }
  getElementsByClassName(names: string): HTMLCollection<Element> { return byClass(this, String(names)); }

  get innerHTML(): string { return documents.get(this.ownerDocument)!.html ? htmlChildren(this) : xmlChildren(this); }
  set innerHTML(value: string) {
    const target = this instanceof TemplateElement ? this.content ?? this : this;
    const html = value == null ? "" : String(value);
    if (html === "") {
      target.replaceChildren();
      return;
    }
    replaceParsedChildren(target, parseMarkup(this, html));
  }

  insertAdjacentElement(position: string, element: Element): Element | null {
    if (!(element instanceof Element)) throw new TypeError("Expected an Element");
    const target = adjacentPosition(this, position);
    if (target === null) return null;
    target.parent.insertBefore(element, target.before);
    return element;
  }

  insertAdjacentText(position: string, data: string): void {
    const target = adjacentPosition(this, position);
    if (target !== null) target.parent.insertBefore(this.ownerDocument.createTextNode(String(data)), target.before);
  }

  insertAdjacentHTML(position: string, html: string): void {
    const target = adjacentPosition(this, position);
    if (target === null || target.parent instanceof Document) {
      fail("NoModificationAllowedError", "The position has no element or fragment parent");
    }
    const document = this.ownerDocument;
    const parent = target.parent;
    const context = parent instanceof Element && parent !== document.documentElement ?
      parent : document.body ?? document.createElement("body");
    mutate(parent, parseMarkup(context, String(html)), target.before);
  }

  get outerHTML(): string { return documents.get(this.ownerDocument)!.html ? serializeHTML(this) : serializeXML(this); }

  get id(): string { return this.getAttributeNS(null, "id") ?? ""; }
  set id(value: string) { this.setAttributeNS(null, "id", value); }
  get className(): string { return this.getAttributeNS(null, "class") ?? ""; }
  set className(value: string) { this.setAttributeNS(null, "class", value); }

  get classList(): DOMTokenList {
    let list = classLists.get(this);
    if (list === undefined) {
      list = new DOMTokenList({ read: () => this.getAttributeNS(null, "class"), write: (value) => { this.className = value; } });
      classLists.set(this, list);
    }
    return list;
  }

  // [PutForwards=value]
  set classList(value: string) { this.classList.value = value; }

  get dataset(): Record<string, string> {
    let data = datasets.get(this);
    if (data === undefined) {
      data = dataset({
        entries: () => [...this.attributes].filter(({ name, namespaceURI }) =>
          namespaceURI === null && name.startsWith("data-") && !/[A-Z]/.test(name)).map(({ name, value }) => [name, value]),
        write: (name, value) => this.setAttribute(name, value),
        remove: (name) => this.removeAttribute(name),
      });
      datasets.set(this, data);
    }
    return data;
  }

  get style(): CSSStyleDeclaration {
    let style = styles.get(this);
    if (style === undefined) {
      style = new CSSStyleDeclaration({
        read: () => this.getAttributeNS(null, "style"),
        commit: (text, changes) => {
          const oldValue = this.getAttributeNS(null, "style");
          storeAttribute(this, { name: "style", localName: "style", prefix: null, namespaceURI: null, value: text });
          const journal = journalFor(this);
          if (journal !== null) {
            for (const [name, declaration] of changes) {
              const property = journal.intern(AtomKind.CssProperty, name);
              journal.record(Opcode.SetStyle, nodeId(this), property, declaration.value, declaration.important ? STYLE_IMPORTANT : 0);
            }
          }
          queueMutation(this, { type: "attributes", name: "style", namespace: null, oldValue });
          customElementAttributeChanged(this, "style", oldValue, text, null);
        },
      });
      styles.set(this, style);
    }
    return style;
  }

  get attributes(): NodeList<Attribute> {
    let list = attributeLists.get(this);
    if (list === undefined) {
      list = new NodeList(() => [...(nodeState(this).attributes?.values() ?? [])].map((attribute) => ({ ...attribute })));
      attributeLists.set(this, list);
    }
    return list;
  }

  getAttributeNames(): string[] { return [...this.attributes].map(({ name }) => name); }
  hasAttributes(): boolean { return (nodeState(this).attributes?.size ?? 0) > 0; }
  hasAttribute(name: string): boolean { return this.getAttribute(name) !== null; }
  hasAttributeNS(namespace: string | null, localName: string): boolean { return this.getAttributeNS(namespace, localName) !== null; }

  #namedAttribute(name: string): Attribute | undefined {
    name = this.#normalName(name);
    for (const attribute of nodeState(this).attributes?.values() ?? []) if (attribute.name === name) return attribute;
  }

  getAttribute(qualifiedName: string): string | null { return this.#namedAttribute(qualifiedName)?.value ?? null; }
  getAttributeNS(namespace: string | null, localName: string): string | null {
    return nodeState(this).attributes?.get(attributeKey(namespace, localName))?.value ?? null;
  }

  setAttribute(name: string, value: string): void {
    if (arguments.length < 2) throw new TypeError("setAttribute requires name and value");
    name = validateLocalName(this.#normalName(name), false);
    const attribute = this.#namedAttribute(name) ?? { name, localName: name, namespaceURI: null, prefix: null };
    writeAttribute(this, { ...attribute, value: String(value) });
  }

  setAttributeNS(namespace: string | null, name: string, value: string): void {
    if (arguments.length < 3) throw new TypeError("setAttributeNS requires namespace, name and value");
    namespace = namespace == null || namespace === "" ? null : String(namespace);
    name = String(name);
    value = String(value);
    const { localName, prefix } = qualifiedName(namespace, name);
    const previous = nodeState(this).attributes?.get(attributeKey(namespace, localName));
    writeAttribute(this, previous === undefined ? { name, localName, prefix, namespaceURI: namespace, value } : { ...previous, value });
  }

  removeAttribute(name: string): void {
    const attribute = this.#namedAttribute(name);
    if (attribute !== undefined) eraseAttribute(this, attribute);
  }

  removeAttributeNS(namespace: string | null, localName: string): void {
    const attribute = nodeState(this).attributes?.get(attributeKey(namespace, localName));
    if (attribute !== undefined) eraseAttribute(this, attribute);
  }

  toggleAttribute(name: string, force?: boolean): boolean {
    if (arguments.length === 0) throw new TypeError("toggleAttribute requires a name");
    name = validateLocalName(this.#normalName(name), false);
    const present = this.hasAttribute(name);
    const enabled = arguments.length === 1 ? !present : Boolean(force);
    if (present !== enabled) {
      if (enabled) this.setAttribute(name, "");
      else this.removeAttribute(name);
    }
    return enabled;
  }
}

const attributeChanged = Symbol("attribute changed");

export function stateOn(element: Element, mask: number, enabled: boolean): void {
  journalFor(element)?.record(Opcode.SetState, nodeId(element), enabled ? mask : 0, mask);
}

function writeValue(element: Element, value: string): void {
  journalFor(element)?.record(Opcode.SetValue, nodeId(element), value, 0);
}

type Handler = ((event: Event) => unknown) | null;
const handlers = new WeakMap<EventTarget, Map<string, { callback: Exclude<Handler, null>; listener: (event: Event) => void }>>();

export function reflectedEvent(prototype: object, type: string): void {
  Object.defineProperty(prototype, "on" + type, {
    configurable: true, enumerable: true,
    get(this: EventTarget): Handler { return handlers.get(this)?.get(type)?.callback ?? null; },
    set(this: EventTarget, value: Handler) {
      let data = handlers.get(this);
      if (data === undefined) handlers.set(this, data = new Map());
      const old = data.get(type);
      if (typeof value !== "function") {
        if (old !== undefined) this.removeEventListener(type, old.listener);
        data.delete(type);
      } else if (old !== undefined) {
        old.callback = value;
      } else {
        const handler = {
          callback: value,
          listener: (event: Event) => {
            if (handler.callback.call(this, event) === false) event.preventDefault();
          },
        };
        data.set(type, handler);
        this.addEventListener(type, handler.listener, { passive: false });
      }
    },
  });
}

const clicking = new WeakSet<HTMLElement>();
type TextControl = HTMLInputElement | HTMLTextAreaElement;
const editableInputTypes = new Set(["text", "search", "url", "tel", "password", "email", "number"]);

function textControl(element: EventTarget | null): element is TextControl {
  return element instanceof HTMLTextAreaElement || element instanceof HTMLInputElement && editableInputTypes.has(element.type);
}

function formControl(element: Element): element is HTMLInputElement | HTMLTextAreaElement | HTMLButtonElement | HTMLSelectElement {
  return element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement ||
    element instanceof HTMLButtonElement || element instanceof HTMLSelectElement;
}

export function disabledControl(element: Element): boolean {
  if (!formControl(element)) return false;
  if (element.disabled) return true;
  for (let parent = element.parentElement; parent !== null; parent = parent.parentElement) {
    if (parent.localName !== "fieldset" || parent.namespaceURI !== HTML_NAMESPACE || !parent.hasAttribute("disabled")) continue;
    const legend = [...parent.children].find(child => child.localName === "legend" && child.namespaceURI === HTML_NAMESPACE);
    if (!legend?.contains(element)) return true;
  }
  return false;
}

function naturalTabIndex(element: HTMLElement): number {
  return formControl(element) || element instanceof HTMLIFrameElement ||
    (element.localName === "a" || element.localName === "area") && element.hasAttribute("href") ||
    editableHost(element) ? 0 : -1;
}

function editableHost(element: HTMLElement): boolean {
  const value = lowerAscii(element.getAttribute("contenteditable") ?? "inherit");
  return value === "" || value === "true" || value === "plaintext-only";
}

function renderedStyle(element: Element, property: "display" | "visibility"): string {
  if (documents.get(element.ownerDocument)!.bindings !== undefined) {
    return nativeRead(element, 4, { property, pseudo: null }) as string;
  }
  if (property === "display") return element.style.getPropertyValue("display");
  for (let node: Element | null = element; node !== null; node = node.parentElement) {
    const value = node.style.getPropertyValue("visibility");
    if (value !== "" && value !== "inherit" && value !== "unset") return value;
  }
  return "visible";
}

function displayed(element: Element, known: Map<Element, boolean>): boolean {
  const cached = known.get(element);
  if (cached !== undefined) return cached;
  const visible = !element.hasAttribute("hidden") && !element.hasAttribute("inert") &&
    renderedStyle(element, "display") !== "none" &&
    (element.parentElement === null || displayed(element.parentElement, known));
  known.set(element, visible);
  return visible;
}

function focusable(element: HTMLElement, known = new Map<Element, boolean>()): boolean {
  if (!element.isConnected || disabledControl(element) || element instanceof HTMLInputElement && element.type === "hidden") return false;
  if (!/^[+-]?\d/.test(element.getAttribute("tabindex")?.trim() ?? "") && naturalTabIndex(element) < 0) return false;
  return displayed(element, known) && !["hidden", "collapse"].includes(renderedStyle(element, "visibility"));
}

export function elementChain(target: Node | null): Element[] {
  const chain: Element[] = [];
  for (let node = target; node !== null; node = node.parentNode) {
    if (node instanceof Element) chain.push(node);
  }
  return chain;
}

export function changeStateChain(previous: readonly Element[], next: readonly Element[], mask: number): void {
  const retained = new Set(next);
  const existing = new Set(previous);
  for (const element of previous) if (!retained.has(element)) stateOn(element, mask, false);
  for (const element of next) if (!existing.has(element)) stateOn(element, mask, true);
}

function focus(document: Document, next: HTMLElement | null, preventScroll: boolean, time?: number): void {
  const state = documents.get(document)!;
  const previous = state.active;
  if (previous === next || next !== null && !focusable(next)) return;
  const revision = ++state.focusRevision;
  if (previous !== null) {
    finishComposition(document, time);
    if (revision !== state.focusRevision) return;
    state.active = null;
    stateOn(previous, ElementState.Focus | ElementState.FocusVisible, false);
    journalFor(previous)?.record(Opcode.Focus, nodeId(previous), FocusFlags.Blur, 0);
    dispatchTrustedEvent(previous, new FocusEvent("blur", { relatedTarget: next, composed: true }), time);
    const relatedTarget = revision === state.focusRevision ? next : null;
    dispatchTrustedEvent(previous, new FocusEvent("focusout", { bubbles: true, relatedTarget, composed: true }), time);
    if (revision !== state.focusRevision) return;
  }
  if (next !== null && focusable(next)) {
    state.active = next;
    state.focusVisible = state.keyboardModality || textControl(next) || editableHost(next);
    stateOn(next, ElementState.Focus | (state.focusVisible ? ElementState.FocusVisible : 0), true);
    journalFor(next)?.record(Opcode.Focus, nodeId(next), preventScroll ? FocusFlags.PreventScroll : 0, 0);
  }
  const chain = elementChain(state.active);
  changeStateChain(state.focusChain, chain, ElementState.FocusWithin);
  state.focusChain = chain;
  if (state.active !== null) {
    dispatchTrustedEvent(state.active, new FocusEvent("focus", { relatedTarget: previous, composed: true }), time);
    if (revision === state.focusRevision) {
      dispatchTrustedEvent(state.active, new FocusEvent("focusin", { bubbles: true, relatedTarget: previous, composed: true }), time);
    }
  }
}

export function notePointerModality(document: Document): void {
  documents.get(document)!.keyboardModality = false;
}

export function focusFromPointer(document: Document, target: Element | null, time: number): void {
  const path = elementChain(target);
  if (path.some(disabledControl)) return;
  const next = path.find((node): node is HTMLElement => node instanceof HTMLElement && focusable(node));
  if (next !== undefined && textControl(next) && !selections.has(next)) moveCaret(next, next.value);
  focus(document, next ?? null, false, time);
}

export type PointerSelection = { control: TextControl; anchor: number };

function pointerTextOffset(control: TextControl, x: number, y: number): number | null {
  if (!control.isConnected || control.ownerDocument.activeElement !== control || disabledControl(control)) return null;
  try {
    const offset = nativeRead(control, 12, { x, y });
    return typeof offset === "number" ? offset : null;
  } catch {
    // Controls that lack laid-out text have no offset to select.
    return null;
  }
}

export function beginPointerSelection(document: Document, target: Element, x: number, y: number, extend: boolean): PointerSelection | null {
  const control = document.activeElement;
  if (!textControl(control) || !control.contains(target) || isComposing(document)) return null;
  const offset = pointerTextOffset(control, x, y);
  if (offset === null) return null;
  const selection = selectionFor(control);
  const anchor = extend ? (selection.direction === "backward" ? selection.end : selection.start) : offset;
  setTextSelection(control, Math.min(anchor, offset), Math.max(anchor, offset), offset < anchor ? "backward" : "forward", extend);
  return { control, anchor };
}

export function updatePointerSelection({ control, anchor }: PointerSelection, x: number, y: number): void {
  const offset = pointerTextOffset(control, x, y);
  if (offset !== null) setTextSelection(control, Math.min(anchor, offset), Math.max(anchor, offset), offset < anchor ? "backward" : "forward");
}

export function noteKeyboardModality(document: Document): void {
  const state = documents.get(document)!;
  state.keyboardModality = true;
  if (state.active !== null && !state.focusVisible) {
    state.focusVisible = true;
    stateOn(state.active, ElementState.FocusVisible, true);
  }
}

export function moveTabFocus(document: Document, backwards: boolean, time: number): void {
  const known = new Map<Element, boolean>();
  const traversal = [...descendants(document)];
  const order = traversal.filter((node): node is HTMLElement =>
    node instanceof HTMLElement && node.tabIndex >= 0 && focusable(node, known))
    .sort((a, b) => (a.tabIndex || Infinity) - (b.tabIndex || Infinity));
  const previous = documents.get(document)!.active;
  const radioGroups = new Map<string, Map<HTMLFormElement | null, HTMLInputElement>>();
  for (const node of order) {
    if (!(node instanceof HTMLInputElement) || node.type !== "radio" || node.name === "") continue;
    let groups = radioGroups.get(node.name);
    if (groups === undefined) radioGroups.set(node.name, groups = new Map());
    const form = node.form;
    const picked = groups.get(form);
    if (picked === undefined || node === previous || picked !== previous && (node.checked || backwards && !picked.checked)) {
      groups.set(form, node);
    }
  }
  const stops = order.filter(node => !(node instanceof HTMLInputElement) || node.type !== "radio" ||
    node.name === "" || radioGroups.get(node.name)!.get(node.form) === node);
  const step = backwards ? -1 : 1;
  const index = previous === null ? -1 : stops.indexOf(previous);
  let next: HTMLElement | null;
  if (index < 0 && previous !== null) {
    const position = traversal.indexOf(previous);
    const natural = stops.filter(node => node.tabIndex === 0);
    if (backwards) natural.reverse();
    next = natural.find(node => (traversal.indexOf(node) - position) * step > 0) ?? null;
    if (next === null && backwards) next = stops.filter(node => node.tabIndex > 0).at(-1) ?? null;
  } else {
    next = stops[index < 0 && backwards ? stops.length - 1 : index + step] ?? null;
  }
  if (next !== null && textControl(next)) setTextSelection(next, 0, next.value.length);
  focus(document, next, false, time);
}

export function reflect(prototype: object, fields: Record<string, string>, boolean = false): void {
  for (const [property, attribute] of Object.entries(fields)) {
    Object.defineProperty(prototype, property, {
      configurable: true, enumerable: true,
      get(this: Element) { return boolean ? this.hasAttribute(attribute) : this.getAttribute(attribute) ?? ""; },
      set: ceWrap(function (this: Element, value: unknown) {
        reflectAttribute(this, attribute, boolean ? Boolean(value) : String(value));
      }),
    });
  }
}

function reflectAttribute(element: Element, name: string, value: string | boolean): void {
  const previous = nodeState(element).attributes?.get(attributeKey(null, name));
  if (value === false) {
    if (previous !== undefined) eraseAttribute(element, previous);
  } else if (value !== true || previous === undefined) {
    writeAttribute(element, { name, localName: name, namespaceURI: null, prefix: null, value: value === true ? "" : value });
  }
}

// Set while squark-dom itself constructs an element, so the HTML element
// constructor steps apply only to script-defined custom element classes.
let internalElement = false;
let builtinInterfaces: Set<Function> | undefined;

function isBuiltinInterface(constructor: Function): boolean {
  builtinInterfaces ??= new Set<Function>([HTMLElement, HTMLUnknownElement, TemplateElement, ...htmlElementTypes.values()]);
  return builtinInterfaces.has(constructor);
}

export class HTMLElement extends Element {
  declare onclick: Handler;
  declare oninput: Handler;
  declare title: string;
  declare hidden: boolean;
  declare lang: string;
  declare dir: string;

  constructor(...args: ConstructorParameters<typeof Element>) {
    if (internalElement) {
      internalElement = false;
      super(...args);
      return;
    }
    const target = htmlElementConstructor(new.target, isBuiltinInterface, HTMLElement);
    if (nodes.has(target as HTMLElement)) return target as HTMLElement;
    const definition = target as CustomElementDefinition;
    const document = definition.document;
    const id = allocate(document);
    recordElement(document, id, definition.localName, HTML_NAMESPACE);
    super(definition.localName, HTML_NAMESPACE, id, document, null);
    // A NewTarget whose prototype is not an object leaves the ordinary-object fallback.
    if (Object.getPrototypeOf(this) === Object.prototype) Object.setPrototypeOf(this, HTMLElement.prototype);
    markCustom(this, definition);
  }

  get innerText(): string {
    const bindings = documents.get(this.ownerDocument)!.bindings;
    return renderedInnerText(this, bindings === undefined ?
      (element, property) => element.style.getPropertyValue(property) :
      (element, property) => nativeRead(element, 4, { property, pseudo: null }) as string);
  }

  set innerText(value: string) {
    const lines = (value === null ? "" : String(value)).split(/\r\n?|\n/);
    const children: Node[] = [];
    for (const [index, line] of lines.entries()) {
      if (index !== 0) children.push(this.ownerDocument.createElement("br"));
      if (line !== "") children.push(this.ownerDocument.createTextNode(line));
    }
    this.replaceChildren(...children);
  }

  get tabIndex(): number {
    const attribute = this.getAttribute("tabindex")?.trim();
    if (attribute && /^[+-]?\d/.test(attribute)) return Number.parseInt(attribute, 10) | 0;
    return naturalTabIndex(this);
  }

  set tabIndex(value: number) { this.setAttribute("tabindex", String(value | 0)); }

  focus(options?: { preventScroll?: boolean }): void {
    focus(this.ownerDocument, this, Boolean(options?.preventScroll));
  }

  blur(): void {
    if (documents.get(this.ownerDocument)!.active === this) focus(this.ownerDocument, null, false);
  }

  click(): void {
    if (disabledControl(this) || clicking.has(this)) return;
    clicking.add(this);
    try {
      this.dispatchEvent(new PointerEvent("click", { bubbles: true, cancelable: true, composed: true, pointerId: -1 }));
    } finally {
      clicking.delete(this);
    }
  }

  override [eventActivation](event: MouseEvent): ((canceled: boolean) => void) | undefined {
    return activation(this, event);
  }

  [attributeChanged](name: string, _oldValue?: string | null): void {
    if (name === "disabled" && (this instanceof HTMLInputElement || this instanceof HTMLTextAreaElement ||
      this instanceof HTMLSelectElement || this instanceof HTMLOptionElement || this instanceof HTMLButtonElement ||
      this.localName === "fieldset" || this.localName === "optgroup")) {
      stateOn(this, ElementState.Disabled, this.hasAttribute(name));
    }
    if (name === "readonly" && (this instanceof HTMLInputElement || this instanceof HTMLTextAreaElement)) {
      stateOn(this, ElementState.ReadOnly, this.hasAttribute(name));
    }
  }
}
reflect(HTMLElement.prototype, { title: "title", lang: "lang", dir: "dir", accessKey: "accesskey", slot: "slot" });
reflect(HTMLElement.prototype, { hidden: "hidden", draggable: "draggable" }, true);
for (const name of ["click", "dblclick", "input", "change", "focus", "blur", "error", "load", "reset", "submit", "beforeinput",
  "focusin", "focusout", "select", "search", "cancel", "copy", "cut", "paste", "compositionstart", "compositionupdate", "compositionend", "keydown", "keyup", "keypress", "wheel", "touchmove", "mousedown", "mouseup", "mousemove",
  "mouseover", "mouseout", "mouseenter", "mouseleave", "pointerdown", "pointerup", "pointermove", "pointercancel",
  "pointerover", "pointerout", "pointerenter", "pointerleave", "gotpointercapture", "lostpointercapture",
  "transitionrun", "transitionstart", "transitionend", "transitioncancel",
  "animationstart", "animationiteration", "animationend", "animationcancel"]) {
  reflectedEvent(HTMLElement.prototype, name);
}

export function formFor(element: HTMLElement): HTMLFormElement | null {
  const explicit = element.getAttribute("form");
  if (explicit !== null) {
    const form = element.isConnected && element.ownerDocument.getElementById(explicit);
    return form instanceof HTMLFormElement ? form : null;
  }
  for (let parent = element.parentElement; parent !== null; parent = parent.parentElement) {
    if (parent instanceof HTMLFormElement) return parent;
  }
  return null;
}

const selectionTypes = new Set(["text", "search", "url", "tel", "password"]);
type ControlSelection = { start: number; end: number; direction: "forward" | "backward" };
const selections = new WeakMap<TextControl, ControlSelection>();

function selectionFor(element: TextControl): ControlSelection {
  const { start, end, direction } = selections.get(element) ?? { start: 0, end: 0, direction: "forward" };
  return { start: Math.min(start, element.value.length), end: Math.min(end, element.value.length), direction };
}

function writeSelection(element: TextControl, selection: ControlSelection): void {
  journalFor(element)?.record(Opcode.SetSelection, nodeId(element), selection.start, selection.end,
    selection.direction === "backward" ? SELECTION_BACKWARD : 0);
}

function setTextSelection(element: TextControl, start: number, end: number, direction?: string, notify = true, preview = false): void {
  end = Math.min(end, element.value.length);
  start = Math.min(start, end);
  const next: ControlSelection = { start, end, direction: direction === "backward" ? "backward" : "forward" };
  const previous = selectionFor(element);
  selections.set(element, next);
  if (start === previous.start && end === previous.end && next.direction === previous.direction) return;
  const composition = compositions.get(element.ownerDocument);
  if (!preview && textControl(element) && !(composition?.control === element && composition.preview)) writeSelection(element, next);
  if (!notify) return;
  endHistoryGroup(element);
  const dispatch = () => dispatchTrustedEvent(element, new Event("select", { bubbles: true }));
  const host = documents.get(element.ownerDocument)!.bindings;
  if (host !== undefined) host.postTask(dispatch);
  else {
    const view = element.ownerDocument.defaultView as { setTimeout?: (callback: () => void, delay: number) => void } | null;
    if (view?.setTimeout !== undefined) view.setTimeout(dispatch, 0);
    else Promise.resolve().then(dispatch);
  }
}

function selectRange(element: TextControl, start: number, end: number, direction?: string): void {
  if (element instanceof HTMLInputElement && !selectionTypes.has(element.type)) {
    fail("InvalidStateError", "The input type does not support selection");
  }
  setTextSelection(element, start >>> 0, end >>> 0, direction);
}

function selectAll(element: TextControl): void {
  if (!textControl(element)) return;
  element.focus();
  setTextSelection(element, 0, element.value.length);
}

function moveCaret(element: TextControl, value: string): void {
  setTextSelection(element, value.length, value.length, undefined, false);
}

const selectionToken = Symbol("Selection constructor");
export class Selection {
  #document: Document;
  constructor(token: symbol, document: Document) {
    if (token !== selectionToken) throw new TypeError("Illegal Selection constructor");
    this.#document = document;
  }

  toString(): string {
    const target = this.#document.activeElement;
    if (!textControl(target)) return "";
    const { start, end } = selectionFor(target);
    return target instanceof HTMLInputElement && target.type === "password"
      ? "•".repeat(end - start)
      : target.value.slice(start, end);
  }
}

const inputTypes = new Set("button checkbox color date datetime-local email file hidden image month number password radio range reset search submit tel text time url week".split(" "));
const attrValueTypes = new Set(["button", "hidden", "image", "reset", "submit", "checkbox", "radio"]);

const userValue = Symbol("user value");
const userChecked = Symbol("user checked");
const cloneInput = Symbol("clone input state");
const resetControl = Symbol("reset control");
const openFile = Symbol("open file picker");

export class HTMLInputElement extends HTMLElement {
  #value: string | null = null;
  #checked: boolean | null = null;
  #indeterminate = false;
  #files: FileList | null = null;
  #fileDialogPending = false;

  declare name: string;
  declare disabled: boolean;
  declare readOnly: boolean;
  declare accept: string;
  declare multiple: boolean;

  get form(): HTMLFormElement | null { return formFor(this); }

  get type(): string {
    const type = lowerAscii(this.getAttribute("type") ?? "text");
    return inputTypes.has(type) ? type : "text";
  }
  set type(value: string) { reflectAttribute(this, "type", String(value)); }

  get value(): string {
    const type = this.type;
    if (attrValueTypes.has(type)) return this.getAttribute("value") ?? (type === "radio" || type === "checkbox" ? "on" : "");
    if (type === "file") {
      const file = this.files?.item(0);
      return file == null ? "" : "C:\\fakepath\\" + file.name;
    }
    return this.#sanitize(this.#value ?? this.defaultValue);
  }

  set value(value: string) {
    value = value === null ? "" : String(value);
    const type = this.type;
    if (attrValueTypes.has(type)) { reflectAttribute(this, "value", value); return; }
    if (type === "file") {
      if (value !== "") fail("InvalidStateError", "File value can only be cleared");
      this.#files = null;
      return;
    }
    if (this[userValue](value)) editingHistory.delete(this);
  }

  get files(): FileList | null { return this.type === "file" ? this.#files ??= fileList() : null; }
  set files(files: FileList | null) {
    if (files !== null && !(files instanceof FileList)) throw new TypeError("Expected a FileList");
    if (this.type === "file" && files !== null) this.#files = files;
  }

  async [openFile](open: NonNullable<PlatformActions["open"]>): Promise<void> {
    if (this.#fileDialogPending) return;
    this.#fileDialogPending = true;
    try {
      const accept = this.accept.split(",").map(type => type.trim()).filter(Boolean);
      const multiple = this.multiple;
      const result = await open(accept, multiple);
      if (this.type !== "file") return;
      if (result === null || result.length === 0) {
        dispatchTrustedEvent(this, new Event("cancel", { bubbles: true }));
        return;
      }
      this.#files = fileList(multiple ? result : result.slice(0, 1));
      dispatchTrustedEvent(this, new Event("input", { bubbles: true, composed: true }));
      dispatchTrustedEvent(this, new Event("change", { bubbles: true }));
    } finally {
      this.#fileDialogPending = false;
    }
  }

  [userValue](value: string, preview = false): boolean {
    const previous = this.value;
    this.#value = this.#sanitize(value);
    if (previous === this.value) return false;
    if (!preview) {
      writeControlValue(this);
      moveCaret(this, this.value);
    }
    return true;
  }

  #sanitize(value: string): string {
    switch (this.type) {
      case "number":
        return /^-?(?:\d+(?:\.\d+)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value) && Number.isFinite(Number(value)) ? value : "";
      case "range": {
        const min = Number(this.getAttribute("min") ?? 0);
        const max = Number(this.getAttribute("max") ?? 100);
        const number = value.trim() !== "" && Number.isFinite(Number(value)) ? Number(value) : (min + max) / 2;
        return String(Math.max(min, Math.min(max, number)));
      }
      case "color": return /^#[0-9a-f]{6}$/i.test(value) ? lowerAscii(value) : "#000000";
      case "email":
      case "url": return value.replace(/[\r\n]/g, "").trim();
      default: return value.replace(/[\r\n]/g, "");
    }
  }

  get defaultValue(): string { return this.getAttribute("value") ?? ""; }
  set defaultValue(value: string) { reflectAttribute(this, "value", String(value)); }

  get checked(): boolean { return this.#checked ?? this.defaultChecked; }
  set checked(value: boolean) { this[userChecked](Boolean(value)); }

  [userChecked](value: boolean): void {
    const previous = this.checked;
    this.#checked = value;
    if (previous !== this.checked) stateOn(this, ElementState.Checked, this.checked);
    this.syncRadioGroup();
  }

  get defaultChecked(): boolean { return this.hasAttribute("checked"); }
  set defaultChecked(value: boolean) { reflectAttribute(this, "checked", Boolean(value)); }

  get indeterminate(): boolean { return this.#indeterminate; }
  set indeterminate(value: boolean) {
    this.#indeterminate = Boolean(value);
    stateOn(this, ElementState.Indeterminate, this.#indeterminate);
  }

  [cloneInput](copy: HTMLInputElement): void {
    copy.#value = this.#value;
    copy.#checked = this.#checked;
    if (this.#value !== null) writeValue(copy, copy.value);
    if (this.#checked !== null) stateOn(copy, ElementState.Checked, copy.checked);
  }

  [resetControl](): void {
    this.#value = null;
    this.#checked = null;
    this.#files = null;
    writeValue(this, this.value);
    stateOn(this, ElementState.Checked, this.checked);
    this.syncRadioGroup();
  }

  *radioGroup(): IterableIterator<HTMLInputElement> {
    if (this.type !== "radio" || this.name === "") return;
    const form = this.form;
    for (const sibling of descendants(this.getRootNode())) {
      if (sibling !== this && sibling instanceof HTMLInputElement && sibling.type === "radio" &&
          sibling.name === this.name && sibling.form === form) yield sibling;
    }
  }

  syncRadioGroup(): void {
    if (!this.checked) return;
    for (const sibling of this.radioGroup()) {
      if (sibling.checked) {
        sibling.#checked = false;
        stateOn(sibling, ElementState.Checked, false);
      }
    }
  }

  override [attributeChanged](name: string, oldValue: string | null = null): void {
    super[attributeChanged](name);
    if (name === "type" && this.type !== "file") this.#files = null;
    if (name === "checked" && this.#checked === null) {
      stateOn(this, ElementState.Checked, this.checked);
      this.syncRadioGroup();
    } else if (name === "value" && this.#value === null || name === "type") {
      if (name === "type" && oldValue !== null && attrValueTypes.has(oldValue) && !attrValueTypes.has(this.type)) this.#value = null;
      writeValue(this, this.value);
      this.syncRadioGroup();
    } else if (name === "name" || name === "form") {
      this.syncRadioGroup();
    }
  }

  get selectionStart(): number | null { return selectionTypes.has(this.type) ? selectionFor(this).start : null; }
  set selectionStart(value: number | null) {
    const start = (value ?? 0) >>> 0;
    this.setSelectionRange(start, Math.max(start, this.selectionEnd ?? 0), this.selectionDirection ?? undefined);
  }
  get selectionEnd(): number | null { return selectionTypes.has(this.type) ? selectionFor(this).end : null; }
  set selectionEnd(value: number | null) { this.setSelectionRange(this.selectionStart ?? 0, value ?? 0, this.selectionDirection ?? undefined); }
  get selectionDirection(): string | null { return selectionTypes.has(this.type) ? selectionFor(this).direction : null; }
  set selectionDirection(value: string | null) { this.setSelectionRange(this.selectionStart ?? 0, this.selectionEnd ?? 0, value ?? ""); }
  setSelectionRange(start: number, end: number, direction?: string): void {
    if (arguments.length < 2) throw new TypeError("setSelectionRange requires start and end");
    selectRange(this, start, end, direction);
  }
  select(): void { selectAll(this); }
}
reflect(HTMLInputElement.prototype, { name: "name", placeholder: "placeholder", min: "min", max: "max", step: "step", accept: "accept", autoComplete: "autocomplete" });
reflect(HTMLInputElement.prototype, { disabled: "disabled", readOnly: "readonly", required: "required", multiple: "multiple", autofocus: "autofocus" }, true);

export class HTMLTextAreaElement extends HTMLElement {
  #value: string | null = null;
  declare name: string;
  declare disabled: boolean;
  declare readOnly: boolean;
  get form(): HTMLFormElement | null { return formFor(this); }
  get type(): string { return "textarea"; }
  get value(): string { return this.#value ?? this.defaultValue.replace(/\r\n?/g, "\n"); }
  set value(value: string) { if (this[userValue](String(value ?? ""))) editingHistory.delete(this); }

  [userValue](value: string, preview = false): boolean {
    const previous = this.value;
    this.#value = value.replace(/\r\n?/g, "\n");
    if (previous === this.value) return false;
    if (!preview) {
      writeControlValue(this);
      moveCaret(this, this.value);
    }
    return true;
  }

  get defaultValue(): string { return this.textContent ?? ""; }
  set defaultValue(value: string) { this.textContent = String(value); }
  get selectionStart(): number { return selectionFor(this).start; }
  set selectionStart(value: number) {
    const start = value >>> 0;
    this.setSelectionRange(start, Math.max(start, this.selectionEnd), this.selectionDirection);
  }
  get selectionEnd(): number { return selectionFor(this).end; }
  set selectionEnd(value: number) { this.setSelectionRange(this.selectionStart, value, this.selectionDirection); }
  get selectionDirection(): string { return selectionFor(this).direction; }
  set selectionDirection(value: string) { this.setSelectionRange(this.selectionStart, this.selectionEnd, value); }
  setSelectionRange(start: number, end: number, direction?: string): void {
    if (arguments.length < 2) throw new TypeError("setSelectionRange requires start and end");
    selectRange(this, start, end, direction);
  }
  select(): void { selectAll(this); }
  [resetControl](): void { this.#value = null; writeValue(this, this.value); }
}
reflect(HTMLTextAreaElement.prototype, { name: "name", placeholder: "placeholder", wrap: "wrap", autoComplete: "autocomplete" });
reflect(HTMLTextAreaElement.prototype, { disabled: "disabled", readOnly: "readonly", required: "required", autofocus: "autofocus" }, true);

function selectFor(option: HTMLOptionElement): HTMLSelectElement | null {
  const parent = option.parentElement;
  return parent instanceof HTMLSelectElement ? parent :
    parent?.localName === "optgroup" && parent.parentElement instanceof HTMLSelectElement ? parent.parentElement : null;
}

const selectionState = Symbol("set selectedness");
const resetOption = Symbol("reset option");

export class HTMLOptionElement extends HTMLElement {
  #selected: boolean | null = null;
  declare disabled: boolean;

  get form(): HTMLFormElement | null { return selectFor(this)?.form ?? null; }
  get value(): string { return this.getAttribute("value") ?? this.text; }
  set value(value: string) { this.setAttribute("value", value); }
  get text(): string { return (this.textContent ?? "").replace(/[\t\n\f\r ]+/g, " ").trim(); }
  set text(value: string) { this.textContent = String(value); }
  get label(): string { return this.getAttribute("label") ?? this.text; }
  set label(value: string) { this.setAttribute("label", value); }
  get defaultSelected(): boolean { return this.hasAttribute("selected"); }
  set defaultSelected(value: boolean) { this.toggleAttribute("selected", Boolean(value)); }
  get selected(): boolean { return this.#selected ?? this.defaultSelected; }
  set selected(value: boolean) {
    this[selectionState](Boolean(value));
    selectFor(this)?.updateSelection(this.selected ? this : undefined);
  }

  [selectionState](selected: boolean): void {
    const previous = this.selected;
    this.#selected = selected;
    if (previous !== selected) stateOn(this, ElementState.Selected, selected);
  }

  [resetOption](): void {
    this.#selected = null;
    stateOn(this, ElementState.Selected, this.selected);
  }

  override [attributeChanged](name: string, _oldValue?: string | null): void {
    super[attributeChanged](name);
    if (name === "selected" && this.#selected === null) {
      stateOn(this, ElementState.Selected, this.selected);
      selectFor(this)?.updateSelection(this.selected ? this : undefined);
    }
  }
}
reflect(HTMLOptionElement.prototype, { disabled: "disabled" }, true);

export class HTMLSelectElement extends HTMLElement {
  #options = new NodeList(() => [...descendants(this)].filter(
    (option): option is HTMLOptionElement => option instanceof HTMLOptionElement && selectFor(option) === this,
  ));
  declare name: string;
  declare disabled: boolean;
  declare multiple: boolean;

  get form(): HTMLFormElement | null { return formFor(this); }
  get options(): NodeList<HTMLOptionElement> { return this.#options; }
  get length(): number { return this.#options.length; }
  get type(): string { return this.multiple ? "select-multiple" : "select-one"; }
  get size(): number { return Math.max(0, Number.parseInt(this.getAttribute("size") ?? "", 10) | 0); }
  set size(value: number) { this.setAttribute("size", String(value >>> 0)); }

  get selectedIndex(): number { return [...this.options].findIndex(option => option.selected); }
  set selectedIndex(value: number) {
    const target = this.options.item(value | 0);
    for (const option of this.options) option[selectionState](option === target);
    writeValue(this, target?.value ?? "");
  }

  get value(): string { return this.options.item(this.selectedIndex)?.value ?? ""; }
  set value(value: string) {
    value = String(value);
    this.selectedIndex = [...this.options].findIndex(option => option.value === value);
  }

  updateSelection(preferred?: HTMLOptionElement): void {
    if (this.multiple) return;
    const options = [...this.options];
    const selected = preferred ?? options.findLast(option => option.selected);
    const target = selected ?? (this.size <= 1 ? options.find(option => !option.disabled && !option.parentElement?.hasAttribute("disabled")) : undefined);
    for (const option of options) if (option.selected !== (option === target)) option[selectionState](option === target);
  }

  override [attributeChanged](name: string, _oldValue?: string | null): void {
    super[attributeChanged](name);
    if (name === "size" || name === "multiple") this.updateSelection();
  }

  [resetControl](): void {
    for (const option of this.options) option[resetOption]();
    this.updateSelection();
    writeValue(this, this.value);
  }
}
reflect(HTMLSelectElement.prototype, { name: "name", autoComplete: "autocomplete" });
reflect(HTMLSelectElement.prototype, { disabled: "disabled", multiple: "multiple", required: "required", autofocus: "autofocus" }, true);

export class HTMLButtonElement extends HTMLElement {
  declare disabled: boolean;
  get form(): HTMLFormElement | null { return formFor(this); }
  get type(): string {
    const type = lowerAscii(this.getAttribute("type") ?? "");
    return type === "button" || type === "reset" ? type : "submit";
  }
  set type(value: string) { this.setAttribute("type", value); }
}
reflect(HTMLButtonElement.prototype, { name: "name", value: "value" });
reflect(HTMLButtonElement.prototype, { disabled: "disabled", autofocus: "autofocus" }, true);

type FormControl = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | HTMLButtonElement;

function associatedControls(form: HTMLFormElement): FormControl[] {
  return [...descendants(form.getRootNode())].filter((node): node is FormControl =>
    node instanceof Element && formControl(node) && node.form === form);
}

export class RadioNodeList extends NodeList<FormControl> {
  get value(): string {
    for (const item of this) if (item instanceof HTMLInputElement && item.type === "radio" && item.checked) return item.value;
    return "";
  }
  set value(value: string) {
    value = String(value);
    for (const item of this) if (item instanceof HTMLInputElement && item.type === "radio" && item.value === value) {
      item.checked = true;
      return;
    }
  }
}

export class HTMLFormControlsCollection extends HTMLCollection<FormControl, FormControl | RadioNodeList> {
  constructor(token: symbol, form: HTMLFormElement) {
    if (token !== formControlsToken) throw new TypeError("Illegal constructor");
    const controls = () => associatedControls(form).filter(
      control => !(control instanceof HTMLInputElement && control.type === "image"));
    super(controls, name => {
      const matches = () => controls().filter(control => control.id === name || getFormName(control) === name);
      const entries = name ? matches() : [];
      return entries.length > 1 ? new RadioNodeList(matches) : entries[0];
    });
  }
}

const formControlsToken = Symbol("HTMLFormControlsCollection constructor");
const formCollections = new WeakMap<HTMLFormElement, HTMLFormControlsCollection>();
function formElements(form: HTMLFormElement): HTMLFormControlsCollection {
  let list = formCollections.get(form);
  if (list === undefined) {
    list = new HTMLFormControlsCollection(formControlsToken, form);
    formCollections.set(form, list);
  }
  return list;
}

function getFormName(control: FormControl): string { return control.getAttribute("name") ?? ""; }

export function formDataEntries(form: HTMLFormElement, submitter: HTMLInputElement | HTMLButtonElement | null,
                                emptyFile: () => DOMFile): [string, string | DOMFile][] {
  if (submitter !== null) {
    if (!isSubmitter(submitter)) throw new TypeError("Expected a submit button");
    if (submitter.form !== form) fail("NotFoundError", "The submitter belongs to a different form");
  }
  const entries: [string, string | DOMFile][] = [];
  for (const control of associatedControls(form)) {
    if (disabledControl(control)) continue;
    const name = getFormName(control);
    if (control instanceof HTMLInputElement && control.type === "image") {
      if (control === submitter) entries.push([name ? name + ".x" : "x", "0"], [name ? name + ".y" : "y", "0"]);
      continue;
    }
    if (name === "") continue;
    if (control instanceof HTMLButtonElement) {
      if (control === submitter) entries.push([name, control.getAttribute("value") ?? ""]);
    } else if (control instanceof HTMLInputElement) {
      if (["button", "reset", "submit"].includes(control.type) && control !== submitter) continue;
      if (["checkbox", "radio"].includes(control.type) && !control.checked) continue;
      if (control.type === "file") {
        const files = control.files!;
        if (files.length === 0) entries.push([name, emptyFile()]);
        else for (const file of files) entries.push([name, file]);
      } else entries.push([name, control.value]);
    } else if (control instanceof HTMLSelectElement) {
      for (const option of control.options) {
        if (option.selected && !option.disabled &&
            !(option.parentElement?.localName === "optgroup" && option.parentElement.hasAttribute("disabled"))) {
          entries.push([name, option.value]);
        }
      }
    } else entries.push([name, control.value]);
  }
  return entries;
}

export class HTMLFormElement extends HTMLElement {
  constructor(localName: string, namespace: string | null, id: number, owner: Document, prefix?: string | null) {
    super(localName, namespace, id, owner, prefix, (node, name) => formElements(node as HTMLFormElement).namedItem(name));
  }
  get elements(): HTMLFormControlsCollection { return formElements(this); }
  get length(): number { return formElements(this).length; }

  requestSubmit(submitter?: HTMLButtonElement | HTMLInputElement): void {
    if (submitter !== undefined) {
      if (!isSubmitter(submitter)) throw new TypeError("Expected a submit button");
      if (submitter.form !== this) fail("NotFoundError", "The submitter belongs to a different form");
    }
    submitForm(this, submitter ?? null);
  }

  reset(): void {
    if (!dispatchTrustedEvent(this, new Event("reset", { bubbles: true, cancelable: true }))) return;
    for (const node of descendants(this.getRootNode())) {
      if ((node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement ||
          node instanceof HTMLSelectElement) && node.form === this) node[resetControl]();
    }
  }

  get method(): string {
    const method = lowerAscii(this.getAttribute("method") ?? "");
    return method === "post" || method === "dialog" ? method : "get";
  }
  set method(value: string) { this.setAttribute("method", value); }
}
reflect(HTMLFormElement.prototype, { name: "name", target: "target", action: "action" });

export class HTMLLabelElement extends HTMLElement {
  declare htmlFor: string;

  get control(): HTMLElement | null {
    const explicit = this.getAttribute("for");
    const root = this.getRootNode();
    if (explicit !== null) {
      const control = byId(root, explicit);
      return control !== null && labelable(control) ? control : null;
    }
    for (const descendant of descendants(this)) {
      if (descendant instanceof Element && labelable(descendant)) return descendant;
    }
    return null;
  }

  get form(): HTMLFormElement | null {
    const control = this.control;
    return control === null ? null : formFor(control);
  }
}
reflect(HTMLLabelElement.prototype, { htmlFor: "for" });

function labelable(element: Element): element is HTMLElement {
  return formControl(element) ? !(element instanceof HTMLInputElement && element.type === "hidden") :
    element instanceof HTMLElement && ["meter", "output", "progress"].includes(element.localName);
}

function isSubmitter(element: unknown): element is HTMLButtonElement | HTMLInputElement {
  return element instanceof HTMLButtonElement && element.type === "submit" ||
    element instanceof HTMLInputElement && (element.type === "submit" || element.type === "image");
}

function submitForm(form: HTMLFormElement, submitter: HTMLElement | null, time?: number): void {
  dispatchTrustedEvent(form, new SubmitEvent("submit", { bubbles: true, cancelable: true, submitter }), time);
}

function toggling(input: HTMLInputElement, event: MouseEvent): (canceled: boolean) => void {
  const radio = input.type === "radio";
  const checked = input.checked;
  const indeterminate = input.indeterminate;
  const previous = radio ? [...input.radioGroup()].find(node => node.checked) : undefined;
  if (!radio || !checked) input[userChecked](radio || !checked);
  if (!radio && indeterminate) input.indeterminate = false;
  return canceled => {
    if (canceled) {
      if (!radio || !checked) input[userChecked](checked);
      if (previous !== undefined && [...input.radioGroup()].includes(previous)) previous[userChecked](true);
      if (!radio && input.indeterminate !== indeterminate) input.indeterminate = indeterminate;
    } else if ((!radio || !checked) && input.isConnected) {
      dispatchTrustedEvent(input, new Event("input", { bubbles: true, composed: true }), event.timeStamp);
      dispatchTrustedEvent(input, new Event("change", { bubbles: true }), event.timeStamp);
    }
  };
}

function activation(element: HTMLElement, event: MouseEvent): ((canceled: boolean) => void) | undefined {
  if (disabledControl(element)) return () => {};
  if (element instanceof HTMLInputElement && (element.type === "checkbox" || element.type === "radio")) {
    return toggling(element, event);
  }
  const platform = documents.get(element.ownerDocument)?.platform;
  if (element instanceof HTMLInputElement && element.type === "file" && platform?.open !== undefined) {
    const open = platform.open;
    return canceled => { if (!canceled) void element[openFile](open).catch(platform.report); };
  }
  if (element instanceof HTMLAnchorElement && element.hasAttribute("href")) {
    return canceled => {
      if (canceled || !element.hasAttribute("href")) return;
      const url = element.href;
      const name = element.getAttribute("download");
      if (name !== null && url.startsWith("blob:")) platform?.save?.(url, name);
      else documents.get(element.ownerDocument)!.navigateLink?.(url, lowerAscii(element.getAttribute("target") ?? "") === "_blank");
    };
  }
  if (isSubmitter(element)) {
    return canceled => { if (!canceled && element.form !== null) submitForm(element.form, element, event.timeStamp); };
  }
  if ((element instanceof HTMLButtonElement || element instanceof HTMLInputElement) && element.type === "reset") {
    return canceled => { if (!canceled) element.form?.reset(); };
  }
  if (element instanceof HTMLLabelElement) {
    const target = event.target;
    for (let child = target instanceof Node ? target : null; child !== null && child !== element; child = child.parentNode) {
      if (child instanceof Element && (labelable(child) || child instanceof HTMLAnchorElement ||
          child.hasAttribute("contenteditable"))) return undefined;
    }
    return canceled => {
      const control = element.control;
      if (canceled || control === null || disabledControl(control)) return;
      const document = element.ownerDocument;
      if (event.isTrusted && focusable(control)) focusFromPointer(document, control, event.timeStamp);
      const click = new PointerEvent("click", {
        bubbles: true, cancelable: true, composed: true, view: document.defaultView, pointerId: -1,
        detail: event.detail, clientX: event.clientX, clientY: event.clientY, button: event.button,
        ctrlKey: event.ctrlKey, shiftKey: event.shiftKey, altKey: event.altKey, metaKey: event.metaKey,
      });
      // A label click forwards activation to its control. For wrapped controls,
      // the interactive-descendant check above excludes a second forward.
      if (event.isTrusted) dispatchTrustedEvent(control, click, event.timeStamp);
      else control.dispatchEvent(click);
    };
  }
  return undefined;
}

function keyClick(element: HTMLElement, time: number): void {
  dispatchTrustedEvent(element, new PointerEvent("click", {
    bubbles: true, cancelable: true, composed: true, pointerId: -1, view: element.ownerDocument.defaultView,
  }), time);
}

function implicitSubmit(element: HTMLInputElement, time: number): void {
  const form = element.form;
  if (form === null) return;
  const controls = [...descendants(form.getRootNode())];
  const submitter = controls.find((node): node is HTMLButtonElement | HTMLInputElement =>
    isSubmitter(node) && node.form === form);
  if (submitter !== undefined) {
    if (!disabledControl(submitter)) keyClick(submitter, time);
  } else if (!controls.some(node => node instanceof HTMLInputElement && node !== element &&
      node.form === form && editableInputTypes.has(node.type))) {
    submitForm(form, null, time);
  }
}

function previousCharacter(value: string, index: number): number {
  const before = value.charCodeAt(index - 1);
  const preceding = value.charCodeAt(index - 2);
  return Math.max(0, index - (before >= 0xdc00 && before <= 0xdfff && preceding >= 0xd800 && preceding <= 0xdbff ? 2 : 1));
}

function nextCharacter(value: string, index: number): number {
  return Math.min(value.length, index + ((value.codePointAt(index) ?? 0) > 0xffff ? 2 : 1));
}

type Composition = {
  control: TextControl; start: number; end: number; value: string; committed: string;
  text: string; updated: boolean; preview: boolean;
};
const compositions = new WeakMap<Document, Composition>();

function writePreedit(control: TextControl, text: string, cursor: number): void {
  journalFor(control)?.record(Opcode.SetPreedit, nodeId(control), text, cursor);
}

function writeControlValue(control: TextControl): void {
  const composition = compositions.get(control.ownerDocument);
  if (composition?.control === control && textControl(control)) {
    if (composition.preview) writePreedit(control, "", 0);
    composition.preview = false;
    composition.committed = control.value;
  }
  writeValue(control, control.value);
}

function fireComposition(control: TextControl, type: string, data: string, time?: number): void {
  dispatchTrustedEvent(control, new CompositionEvent(type, {
    data, bubbles: true, cancelable: true, composed: true, view: control.ownerDocument.defaultView,
  }), time);
}

function finishComposition(document: Document, time?: number): void {
  const current = compositions.get(document);
  if (current === undefined) return;
  compositions.delete(document);
  const { control } = current;
  if (current.updated && textControl(control)) {
    if (current.preview) writePreedit(control, "", 0);
    if (control.value !== current.committed) writeValue(control, control.value);
    writeSelection(control, selectionFor(control));
  }
  if (control.isConnected) fireComposition(control, "compositionend", current.text, time);
}

export function isComposing(document: Document): boolean {
  return compositions.has(document);
}

function editable(control: TextControl): boolean {
  return control.isConnected && control.ownerDocument.activeElement === control && !control.readOnly && !disabledControl(control);
}

function beginComposition(control: TextControl, time: number): Composition {
  const { start, end } = selectionFor(control);
  const value = control.value;
  const session = { control, start, end, value, committed: value, text: "", updated: false, preview: false };
  compositions.set(control.ownerDocument, session);
  fireComposition(control, "compositionstart", session.value.slice(start, end), time);
  return session;
}

function textBeforeInput(control: TextControl, type: string, data: string | null, composing: boolean, time: number): boolean {
  return dispatchTrustedEvent(control, new InputEvent("beforeinput", {
    inputType: type, data, isComposing: composing, bubbles: true, composed: true, cancelable: !composing,
  }), time);
}

function textInput(control: TextControl, type: string, data: string | null, composing: boolean, time: number): void {
  dispatchTrustedEvent(control, new InputEvent("input", {
    inputType: type, data, isComposing: composing, bubbles: true, composed: true,
  }), time);
}

function replaceText(control: TextControl, from: number, to: number, replacement: string, preview = false): boolean {
  const value = control.value;
  return control[userValue](value.slice(0, from) + replacement + value.slice(to), preview);
}

function updateComposition(session: Composition, text: string, selection: readonly [number, number] | null, time: number): void {
  const { control, start, end } = session;
  const document = control.ownerDocument;
  if (compositions.get(document) !== session || !editable(control)) return;
  if (control.value !== session.value) {
    // A controlled component can replace the value during input. Its new
    // text takes ownership of the range, so there is no stale span to edit.
    finishComposition(document, time);
    return;
  }
  if (session.updated) setTextSelection(control, start, end, undefined, false, true);
  fireComposition(control, "compositionupdate", text, time);
  if (compositions.get(document) !== session || !editable(control)) return;
  textBeforeInput(control, "insertCompositionText", text, true, time);
  if (compositions.get(document) !== session || !editable(control)) return;
  if (control.value !== session.value) {
    finishComposition(document, time);
    return;
  }
  replaceText(control, start, end, text, true);
  const length = Math.max(0, control.value.length - (session.value.length - (end - start)));
  const [anchor, caret] = selection ?? [length, length];
  setTextSelection(control, start + Math.min(anchor, caret, length),
    start + Math.min(Math.max(anchor, caret), length), anchor > caret ? "backward" : "forward", false, true);
  if (!session.updated) writeSelection(control, { start, end, direction: "forward" });
  writePreedit(control, control.value.slice(start, start + length), Math.min(caret, length));
  session.preview = length !== 0;
  session.end = start + length;
  session.value = control.value;
  session.text = text;
  session.updated = true;
  textInput(control, "insertCompositionText", text === "" ? null : text, true, time);
}

export type ImeKind = "start" | "update" | "commit" | "end";

export function inputComposition(document: Document, target: number, kind: ImeKind,
  text: string, selection: readonly [number, number] | null, time: number): boolean {
  const control = document.activeElement;
  if (target !== 0 && (control === null || nodeId(control) !== target)) return false;
  let session = compositions.get(document);
  if (session !== undefined && session.control !== control) return false;
  if (!textControl(control) || !editable(control)) {
    finishComposition(document, time);
    return false;
  }
  if (kind === "end") {
    if (session?.updated && session.text !== "") updateComposition(session, "", null, time);
    finishComposition(document, time);
    return false;
  }
  if (kind === "start") {
    if (session === undefined) beginComposition(control, time);
    return false;
  }
  if (kind === "update") {
    if (text === "" && session === undefined) return false;
    session ??= beginComposition(control, time);
    updateComposition(session, text, selection, time);
    return false;
  }
  if (session !== undefined) {
    updateComposition(session, text, null, time);
    finishComposition(document, time);
    return false;
  }
  // An IME can commit without establishing preedit, for example when a
  // single candidate was accepted immediately.
  if (text === "") return false;
  return editText(control, selectionFor(control), text, "insertText", time);
}

type EditState = { value: string; selection: ControlSelection };
type HistoryEntry = { before: EditState; after: EditState; type: string; time: number };
type EditHistory = { undo: HistoryEntry[]; redo: HistoryEntry[]; grouping: boolean };
const editingHistory = new WeakMap<TextControl, EditHistory>();

function editState(control: TextControl): EditState {
  return { value: control.value, selection: selectionFor(control) };
}

function endHistoryGroup(control: TextControl): void {
  const history = editingHistory.get(control);
  if (history !== undefined) history.grouping = false;
}

function rememberEdit(control: TextControl, before: EditState, type: string, time: number): void {
  let history = editingHistory.get(control);
  if (history === undefined) {
    history = { undo: [], redo: [], grouping: false };
    editingHistory.set(control, history);
  }
  const after = editState(control);
  const prior = history.undo.at(-1);
  const groupable = ["insertText", "deleteContentBackward", "deleteContentForward"].includes(type) &&
    before.selection.start === before.selection.end;
  if (history.grouping && groupable && prior?.type === type && prior.after.value === before.value &&
      prior.after.selection.end === before.selection.end && time - prior.time <= 1000) {
    prior.after = after;
    prior.time = time;
  } else {
    history.undo.push({ before, after, type, time });
    if (history.undo.length > 100) history.undo.shift();
  }
  history.grouping = groupable;
  history.redo.length = 0;
}

function applyEditHistory(control: TextControl, redo: boolean, time: number): boolean {
  if (!editable(control)) return false;
  const history = editingHistory.get(control);
  if (history === undefined) return false;
  const from = redo ? history.redo : history.undo;
  const to = redo ? history.undo : history.redo;
  const entry = from.at(-1);
  if (entry === undefined) return false;
  const type = redo ? "historyRedo" : "historyUndo";
  const value = control.value;
  if (!textBeforeInput(control, type, null, false, time)) return true;
  if (!editable(control) || control.value !== value) return false;
  const state = redo ? entry.after : entry.before;
  from.pop();
  to.push(entry);
  history.grouping = false;
  control[userValue](state.value);
  const { start, end, direction } = state.selection;
  setTextSelection(control, start, end, direction, false);
  textInput(control, type, null, false, time);
  return false;
}

function editText(control: TextControl, range: { start: number; end: number }, data: string | null, inputType: string, time: number): boolean {
  const before = editState(control);
  const value = before.value;
  const { start, end } = range;
  const length = control.getAttribute("maxlength")?.trim().match(/^\d+/)?.[0];
  if (data !== null && length !== undefined && value.length - (end - start) + data.length > Number(length)) return false;
  const inputData = inputType === "insertText" || inputType === "insertFromPaste" ? data : null;
  if (!textBeforeInput(control, inputType, inputData, false, time)) return true;
  if (!editable(control) || control.value !== value) return false;
  const replacement = data ?? "";
  if (replaceText(control, start, end, replacement)) {
    const caret = Math.min(start + replacement.length, control.value.length);
    setTextSelection(control, caret, caret, undefined, false);
    rememberEdit(control, before, inputType, time);
    textInput(control, inputType, inputData, false, time);
  }
  return false;
}

export function clipboardSelection(document: Document, target: Element): string {
  if (target !== document.activeElement || !textControl(target) ||
      target instanceof HTMLInputElement && target.type === "password") return "";
  const { start, end } = selectionFor(target);
  return target.value.slice(start, end);
}

export function editClipboardSelection(document: Document, target: Element, type: "deleteByCut" | "insertFromPaste",
                                       text: string | null, time: number): void {
  if (target !== document.activeElement || !target.isConnected || !textControl(target) || !editable(target)) return;
  const range = selectionFor(target);
  if (type === "deleteByCut" && range.start === range.end) return;
  editText(target, range, text, type, time);
}

function editControl(control: TextControl, key: string, time: number): boolean {
  const value = control.value;
  const { start, end } = selectionFor(control);
  let from = Math.min(start, value.length);
  let to = Math.min(end, value.length);
  let data: string | null;
  let inputType: string;
  switch (key) {
    case "Backspace":
      if (from === to) from = previousCharacter(value, from);
      data = null;
      inputType = "deleteContentBackward";
      break;
    case "Delete":
      if (from === to) to = nextCharacter(value, to);
      data = null;
      inputType = "deleteContentForward";
      break;
    case "Enter":
      if (!(control instanceof HTMLTextAreaElement)) return false;
      data = "\n";
      inputType = "insertLineBreak";
      break;
    default:
      if ([...key].length !== 1) return false;
      data = key;
      inputType = "insertText";
  }
  if (data === null && from === to) return false;
  return editText(control, { start: from, end: to }, data, inputType, time);
}

function lineStart(value: string, position: number): number {
  return position === 0 ? 0 : value.lastIndexOf("\n", position - 1) + 1;
}

function lineEnd(value: string, position: number): number {
  const next = value.indexOf("\n", position);
  return next === -1 ? value.length : next;
}

const movementKeys = new Set(["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"]);

function moveTextCaret(control: TextControl, event: KeyboardEvent): void {
  const value = control.value;
  const { start, end, direction } = selectionFor(control);
  const focus = direction === "backward" ? start : end;
  const anchor = direction === "backward" ? end : start;
  const multiline = control instanceof HTMLTextAreaElement;
  const root = event.ctrlKey || event.metaKey;
  let next: number;
  switch (event.key) {
    case "ArrowLeft":
      next = !event.shiftKey && start !== end ? start : previousCharacter(value, focus);
      break;
    case "ArrowRight":
      next = !event.shiftKey && start !== end ? end : nextCharacter(value, focus);
      break;
    case "ArrowUp": {
      const line = lineStart(value, focus);
      next = multiline && line > 0 ? Math.min(lineStart(value, line - 1) + focus - line, line - 1) : 0;
      break;
    }
    case "ArrowDown": {
      const line = lineEnd(value, focus);
      next = multiline && line < value.length ?
        Math.min(line + 1 + focus - lineStart(value, focus), lineEnd(value, line + 1)) : value.length;
      break;
    }
    case "Home":
      next = root || !multiline ? 0 : lineStart(value, focus);
      break;
    default:
      next = root || !multiline ? value.length : lineEnd(value, focus);
  }
  endHistoryGroup(control);
  if (event.shiftKey) setTextSelection(control, Math.min(anchor, next), Math.max(anchor, next), next < anchor ? "backward" : "forward");
  else setTextSelection(control, next, next);
}

export function spaceActivates(target: Element | null): target is HTMLElement {
  return target instanceof HTMLButtonElement || target instanceof HTMLInputElement &&
    ["button", "submit", "reset", "radio", "checkbox"].includes(target.type);
}

export function keyboardDefault(document: Document, target: Element, key: KeyboardEvent, time: number): boolean {
  if (key.type === "keydown" && key.key === "Escape" && !key.ctrlKey && !key.altKey && !key.metaKey) {
    const session = compositions.get(document);
    if (session !== undefined) inputComposition(document, nodeId(session.control), "end", "", null, time);
    else if (!key.isComposing && target instanceof HTMLInputElement && target.type === "search" && editable(target) &&
        target.value !== "" && target[userValue]("")) {
      dispatchTrustedEvent(target, new Event("input", { bubbles: true, composed: true }), time);
      dispatchTrustedEvent(target, new Event("search", { bubbles: true }), time);
    }
    return false;
  }
  if (!key.isComposing && key.type === "keydown" && (key.ctrlKey || key.metaKey) && !key.altKey && !key.shiftKey) {
    const letter = key.key.toLowerCase();
    if (letter === "c" || letter === "x" || letter === "v") {
      documents.get(document)!.platform?.clipboard?.(target, letter, time);
      return false;
    }
  }
  if (!key.isComposing && key.type === "keydown" && key.key.toLowerCase() === "a" &&
      (key.ctrlKey || key.metaKey) && !key.altKey && !key.shiftKey &&
      target === document.activeElement && target.isConnected && !disabledControl(target) && textControl(target)) {
    setTextSelection(target, 0, target.value.length);
    return false;
  }
  if (!key.isComposing && key.type === "keydown" && !key.altKey && target === document.activeElement &&
      target.isConnected && !disabledControl(target) && textControl(target)) {
    const letter = key.key.toLowerCase();
    if ((key.ctrlKey || key.metaKey) && (letter === "z" || letter === "y" && !key.shiftKey)) {
      return applyEditHistory(target, key.shiftKey || letter === "y", time);
    }
    if (movementKeys.has(key.key)) {
      moveTextCaret(target, key);
      return false;
    }
  }
  if (key.isComposing || key.ctrlKey || key.altKey || key.metaKey) return false;
  if (key.type === "keydown" && key.key === "Tab") {
    moveTabFocus(document, key.shiftKey, time);
    return false;
  }
  if (target !== document.activeElement || !target.isConnected || disabledControl(target)) return false;
  if (key.type === "keydown" && key.key === "Enter" && target instanceof HTMLInputElement &&
      editableInputTypes.has(target.type)) {
    implicitSubmit(target, time);
  } else if (spaceActivates(target) &&
      (key.type === "keydown" && key.key === "Enter" && !(target instanceof HTMLInputElement && ["radio", "checkbox"].includes(target.type)) ||
       key.type === "keyup" && key.key === " ")) {
    keyClick(target, time);
  } else if (key.type === "keydown" && textControl(target) && !target.readOnly) {
    return editControl(target, key.key, time);
  }
  return false;
}

export class HTMLStyleElement extends HTMLElement {
  declare media: string;
  declare type: string;

  get sheet(): CSSStyleSheet | null { return attachedSheet(this); }
  get disabled(): boolean { return this.sheet?.disabled ?? false; }
  set disabled(value: boolean) { if (this.sheet !== null) this.sheet.disabled = value; }

  override [attributeChanged](name: string): void {
    super[attributeChanged](name);
    if (name === "type" && !isCSS(this)) removeSheet(this);
    else if (name === "type" && this.isConnected) syncStyleText(this);
  }
}
reflect(HTMLStyleElement.prototype, { media: "media", type: "type" });

export class HTMLLinkElement extends HTMLElement {
  declare rel: string;
  declare media: string;

  get href(): string { return resolvedURL(this, "href"); }
  set href(value: string) { this.setAttribute("href", value); }
  get sheet(): CSSStyleSheet | null { return attachedSheet(this); }

  override [attributeChanged](name: string): void {
    super[attributeChanged](name);
    if (name === "rel" || name === "href") removeSheet(this);
  }
}
reflect(HTMLLinkElement.prototype, { media: "media", rel: "rel" });
reflect(HTMLLinkElement.prototype, { disabled: "disabled" }, true);

type SheetOwner = HTMLStyleElement | HTMLLinkElement;
type SheetHost = {
  query(kind: number, id: number): unknown;
  cssParseRule(rule: string): string | null;
};
type SheetBacking = {
  count: number;
  text?: string;
  href?: string;
  packedLink?: string;
  raw: boolean;
  rules?: readonly string[];
  sheet?: CSSStyleSheet;
};
const sheetBackings = new WeakMap<SheetOwner, SheetBacking>();

function styleText(owner: HTMLStyleElement): string {
  return [...owner.childNodes].filter(child => child instanceof Text).map(child => child.data).join("");
}

function isCSS(owner: HTMLStyleElement): boolean {
  return owner.type === "" || lowerAscii(owner.type) === "text/css";
}

function isSheetOwner(node: Node): node is SheetOwner {
  return node instanceof HTMLStyleElement || node instanceof HTMLLinkElement;
}

function backing(owner: SheetOwner): SheetBacking {
  let state = sheetBackings.get(owner);
  if (state === undefined) sheetBackings.set(owner, state = { count: 0, raw: false });
  return state;
}

function insertSheetRule(owner: SheetOwner, state: SheetBacking, index: number, rule: string): void {
  journalFor(owner)?.record(Opcode.SheetInsert, nodeId(owner), index, rule);
  state.count++;
}

function removeSheetRule(owner: SheetOwner, state: SheetBacking, index: number): void {
  journalFor(owner)?.record(Opcode.SheetDelete, nodeId(owner), index, 0);
  state.count--;
}

function replaceSheetRecords(owner: SheetOwner, state: SheetBacking, rules: readonly string[]): void {
  while (state.count !== 0) removeSheetRule(owner, state, state.count - 1);
  for (const [index, rule] of rules.entries()) insertSheetRule(owner, state, index, rule);
}

function normalizeSheet(owner: SheetOwner, state: SheetBacking): readonly string[] {
  if (!state.raw) return state.rules ?? [];
  const document = owner.ownerDocument;
  const host = documents.get(document)!.cssom;
  if (host === undefined) fail("NotSupportedError", "Stylesheet rules require the document engine");
  flushDocument(document);
  const serialized = host.query(10, nodeId(owner)) as string[];
  replaceSheetRecords(owner, state, serialized);
  state.raw = false;
  return state.rules = serialized;
}

function removeSheet(owner: SheetOwner): void {
  const state = sheetBackings.get(owner);
  if (state === undefined) return;
  if (state.sheet !== undefined) detachSheet(state.sheet);
  state.sheet = undefined;
  state.rules = undefined;
  state.text = undefined;
  state.raw = false;
  replaceSheetRecords(owner, state, []);
}

function syncStyleText(owner: HTMLStyleElement, changed = false): void {
  if (!owner.isConnected || !isCSS(owner)) return;
  const state = backing(owner);
  const text = styleText(owner);
  if (!changed && state.text === text) return;
  if (state.sheet !== undefined) detachSheet(state.sheet);
  state.sheet = undefined;
  state.rules = undefined;
  state.text = text;
  state.raw = text !== "";
  replaceSheetRecords(owner, state, text === "" ? [] : [text]);
}

function attachedSheet(owner: SheetOwner): CSSStyleSheet | null {
  if (!owner.isConnected) return null;
  const state = backing(owner);
  if (owner instanceof HTMLStyleElement) {
    if (!isCSS(owner)) return null;
  } else {
    const rel = lowerAscii(owner.rel).split(/[\t\n\f\r ]+/);
    if (state.href === undefined || owner.getAttribute("href") !== state.packedLink ||
        !rel.includes("stylesheet") || rel.includes("alternate")) return null;
    if (state.sheet === undefined) state.raw = true;
  }
  return state.sheet ??= createSheet(owner, state.href ?? null, {
    read: () => normalizeSheet(owner, state),
    parse: rule => {
      const host = documents.get(owner.ownerDocument)!.cssom;
      if (host === undefined) fail("NotSupportedError", "Stylesheet rules require the document engine");
      return host.cssParseRule(rule);
    },
    insert: (index, rule) => insertSheetRule(owner, state, index, rule),
    remove: index => removeSheetRule(owner, state, index),
  });
}

export function installCSSOM(document: Document, host: SheetHost, packedLinks: readonly [number, string][] = []): void {
  documents.get(document)!.cssom = host;
  for (const [id, href] of packedLinks) {
    const node = connectedInputTarget(document, id);
    if (!(node instanceof HTMLLinkElement)) continue;
    const state = backing(node);
    state.href = href;
    state.packedLink = node.getAttribute("href") ?? "";
    state.raw = true;
  }
}

export class HTMLIFrameElement extends HTMLElement {
  get contentDocument(): null { return null; }
  get contentWindow(): null { return null; }
}
reflect(HTMLIFrameElement.prototype, { src: "src", srcdoc: "srcdoc", name: "name", width: "width", height: "height", allow: "allow" });
reflect(HTMLIFrameElement.prototype, { allowFullscreen: "allowfullscreen" }, true);

export class HTMLUnknownElement extends HTMLElement {}

export class HTMLBRElement extends HTMLElement { declare clear: string; }
reflect(HTMLBRElement.prototype, { clear: "clear" });

export function resolvedURL(element: Element, attribute: string): string {
  const raw = element.getAttribute(attribute);
  if (raw === null) return "";
  const URL = (globalThis as { URL?: new (url: string, base: string) => { href: string } }).URL;
  try { return URL === undefined ? raw : new URL(raw, element.ownerDocument.URL).href; }
  catch { return raw; }
}

export class HTMLCanvasElement extends HTMLElement {
  #dimension(name: string, fallback: number): number {
    const raw = this.getAttribute(name);
    const parsed = raw?.match(/^[\t\n\f\r ]*\+?(\d+)/)?.[1];
    const value = parsed === undefined ? NaN : Number(parsed);
    return value <= 0x7fff_ffff ? value : fallback;
  }
  #setDimension(name: string, value: number, fallback: number): void {
    const width = Number(value) >>> 0;
    this.setAttribute(name, String(width <= 0x7fff_ffff ? width : fallback));
  }

  get width(): number { return this.#dimension("width", 300); }
  set width(value: number) { this.#setDimension("width", value, 300); }
  get height(): number { return this.#dimension("height", 150); }
  set height(value: number) { this.#setDimension("height", value, 150); }

  override [attributeChanged](name: string, oldValue?: string | null): void {
    super[attributeChanged](name, oldValue);
    if (name === "width" || name === "height") resizeCanvas(this);
  }

  getContext(kind: string, _attributes?: object | null): CanvasRenderingContext2D | null { return canvasContext(this, kind); }
  toDataURL(type?: string, quality?: number): string { return canvasDataURL(this, type, quality); }
  toBlob(callback: (blob: object | null) => void, type?: string, quality?: number): void {
    canvasToBlob(this, callback, type, quality);
  }
}
Object.defineProperty(HTMLCanvasElement.prototype, Symbol.toStringTag, { value: "HTMLCanvasElement", configurable: true });

export function documentJournal(node: Document): Journal { return journalFor(node)!; }

export function canvasJournal(node: HTMLCanvasElement | Document): Journal | null {
  return journalFor(node);
}

const imageSourceChanged = Symbol("image source changed");
const finishImage = Symbol("finish image");

export class HTMLImageElement extends HTMLElement {
  #generation = 0;
  #status: "empty" | "pending" | "loaded" | "failed" = "empty";
  #width = 0;
  #height = 0;
  #decode?: ReturnType<typeof Promise.withResolvers<void>>;

  get src(): string { return resolvedURL(this, "src"); }
  set src(value: string) { this.setAttribute("src", value); }
  get currentSrc(): string { return this.#status === "loaded" || this.#status === "failed" ? this.src : ""; }
  get complete(): boolean { return this.#status !== "pending" || this.getAttribute("src") === ""; }
  get naturalWidth(): number { return this.#width; }
  get naturalHeight(): number { return this.#height; }
  get width(): number { return this.#dimension("width", this.naturalWidth); }
  set width(value: number) { this.setAttribute("width", String(Number(value) >>> 0)); }
  get height(): number { return this.#dimension("height", this.naturalHeight); }
  set height(value: number) { this.setAttribute("height", String(Number(value) >>> 0)); }

  #dimension(name: string, intrinsic: number): number {
    const parsed = this.getAttribute(name)?.match(/^[\t\n\f\r ]*\+?(\d+)/)?.[1];
    return parsed === undefined ? intrinsic : Number(parsed) >>> 0;
  }

  async decode(): Promise<void> {
    if (this.#status === "loaded") return;
    if (this.#status !== "pending" || this.getAttribute("src") === "") {
      fail("EncodingError", "The image could not be decoded");
    }
    if (this.#decode === undefined) {
      this.#decode = Promise.withResolvers<void>();
      // The resource completion reaches this element through a weak node ID lookup.
      const tracker = nodeState(this).tracked?.tracker;
      if (tracker !== undefined) (tracker.decodingImages ??= new Set()).add(this);
    }
    await this.#decode.promise;
  }

  #takeDecode() {
    const pending = this.#decode;
    if (pending !== undefined) {
      this.#decode = undefined;
      nodeState(this).tracked?.tracker.decodingImages?.delete(this);
    }
    return pending;
  }

  [imageSourceChanged](): void {
    this.#generation = (this.#generation + 1) >>> 0;
    this.#status = this.hasAttribute("src") ? "pending" : "empty";
    this.#width = 0;
    this.#height = 0;
    this.#takeDecode()?.reject(new DOMException("The image source changed", "EncodingError"));
  }

  [finishImage](generation: number, size: readonly [number, number] | null): boolean {
    if (generation !== this.#generation || this.#status !== "pending") return false;
    const pending = this.#takeDecode();
    if (size === null) {
      this.#status = "failed";
      pending?.reject(new DOMException("The image could not be decoded", "EncodingError"));
    } else {
      this.#status = "loaded";
      [this.#width, this.#height] = size;
      pending?.resolve();
    }
    dispatchTrustedEvent(this, new Event(size === null ? "error" : "load"));
    return true;
  }

  override [attributeChanged](name: string, oldValue?: string | null): void {
    super[attributeChanged](name, oldValue);
    if (name === "src") this[imageSourceChanged]();
  }
}

export function dispatchImageResult(document: Document, id: number, generation: number,
                                    size: readonly [number, number] | null): boolean {
  const journal = journalFor(document);
  if (journal === null) return false;
  const node = runtimeNodes.get(journal)?.index.get(id)?.ref.deref();
  return node instanceof HTMLImageElement && node.ownerDocument === document && node[finishImage](generation, size);
}

Object.defineProperty(HTMLImageElement.prototype, Symbol.toStringTag, { value: "HTMLImageElement", configurable: true });
reflect(HTMLImageElement.prototype, { alt: "alt", srcset: "srcset", sizes: "sizes", loading: "loading", crossOrigin: "crossorigin" });

export class HTMLAnchorElement extends HTMLElement {
  get href(): string { return resolvedURL(this, "href"); }
  set href(value: string) { this.setAttribute("href", value); }
}
reflect(HTMLAnchorElement.prototype, { target: "target", download: "download", rel: "rel", name: "name" });

const parserInsertedScripts = new WeakSet<HTMLScriptElement>();

export class HTMLScriptElement extends HTMLElement {
  get src(): string { return resolvedURL(this, "src"); }
  set src(value: string) { this.setAttribute("src", value); }
  override [attributeChanged](name: string, oldValue?: string | null): void {
    super[attributeChanged](name, oldValue);
    if (name === "src" && this.isConnected && !parserInsertedScripts.has(this)) {
      documents.get(this.ownerDocument)?.loadScript?.(this);
    }
  }
}
reflect(HTMLScriptElement.prototype, { type: "type", integrity: "integrity", crossOrigin: "crossorigin" });
reflect(HTMLScriptElement.prototype, { async: "async", defer: "defer", noModule: "nomodule" }, true);

export function installScriptLoader(document: Document, load: (script: HTMLScriptElement) => void): void {
  documents.get(document)!.loadScript = load;
}

const ordinaryHtmlElements = new Set(`abbr address area article aside audio b base bdi bdo big blockquote
  body br canvas caption center cite code col colgroup data datalist dd del details dfn dialog div dl dt em embed
  fieldset figcaption figure font footer frame frameset h1 h2 h3 h4 h5 h6 head header hgroup hr html i ins kbd label
  legend li link main map mark menu meta meter nav nobr noembed noframes noscript object ol optgroup output p param
  picture plaintext pre progress q rb rp rt rtc ruby s samp script search section slot small source span strike strong
  style sub summary sup table tbody td th thead time tr track tt u ul var video wbr xmp`.split(/\s+/));
const htmlElementTypes = new Map<string, typeof HTMLElement>([
  ["input", HTMLInputElement], ["textarea", HTMLTextAreaElement], ["option", HTMLOptionElement],
  ["br", HTMLBRElement],
  ["select", HTMLSelectElement], ["button", HTMLButtonElement], ["form", HTMLFormElement], ["label", HTMLLabelElement], ["iframe", HTMLIFrameElement],
  ["a", HTMLAnchorElement], ["canvas", HTMLCanvasElement], ["img", HTMLImageElement], ["script", HTMLScriptElement], ["style", HTMLStyleElement], ["link", HTMLLinkElement],
]);
const elementInterfaces = new Set<typeof HTMLElement>();

/** Makes `type` the interface of HTML elements with these local names and exposes it on windows. */
export function defineElementInterface(type: typeof HTMLElement, ...localNames: string[]): void {
  for (const name of localNames) htmlElementTypes.set(name, type);
  elementInterfaces.add(type);
  Object.defineProperty(type.prototype, Symbol.toStringTag, { value: type.name, configurable: true });
  builtinInterfaces = undefined;
}

function treeChanged(parent: Node, child: Node, inserted: boolean): void {
  for (let node: Node | null = parent; node !== null; node = node.parentNode) {
    if (node instanceof HTMLSelectElement) {
      node.updateSelection();
      break;
    }
  }
  if (inserted) {
    const connected = customElementsActive && child.isConnected;
    if (child instanceof HTMLInputElement) child.syncRadioGroup();
    if (child instanceof HTMLStyleElement) syncStyleText(child);
    if (child instanceof HTMLScriptElement && child.isConnected && !parserInsertedScripts.has(child)) {
      documents.get(child.ownerDocument)?.loadScript?.(child);
    }
    customElementInserted(child, connected);
    for (const node of descendants(child)) {
      if (node instanceof HTMLInputElement && node.type === "radio") node.syncRadioGroup();
      if (node instanceof HTMLStyleElement) syncStyleText(node);
      if (node instanceof HTMLScriptElement && node.isConnected && !parserInsertedScripts.has(node)) {
        documents.get(node.ownerDocument)?.loadScript?.(node);
      }
      customElementInserted(node, connected);
    }
  } else {
    const parentConnected = customElementsActive && parent.isConnected;
    if (isSheetOwner(child)) removeSheet(child);
    customElementRemoved(child, parentConnected);
    for (const node of descendants(child)) {
      if (isSheetOwner(node)) removeSheet(node);
      customElementRemoved(node, parentConnected);
    }
    const document = ownerOf(parent);
    const state = documents.get(document)!;
    state.inputDetached?.(child);
    const active = state.active;
    if (active !== null && child.contains(active)) {
      finishComposition(document);
      state.focusRevision++;
      state.active = null;
      stateOn(active, ElementState.Focus | ElementState.FocusVisible, false);
      changeStateChain(state.focusChain, [], ElementState.FocusWithin);
      state.focusChain = [];
    }
  }
}

const templateContents = new WeakMap<TemplateElement, DocumentFragment>();

export class HTMLTemplateElement extends HTMLElement {
  get content(): DocumentFragment | undefined { return templateContents.get(this); }
}
Object.defineProperty(HTMLTemplateElement.prototype, Symbol.toStringTag, { value: "HTMLTemplateElement", configurable: true });
type TemplateElement = HTMLTemplateElement;
const TemplateElement = HTMLTemplateElement;

export class SVGElement extends Element {}

export class SVGGraphicsElement extends SVGElement {
  getBBox(): DOMRect {
    return wrapRect(nativeRead(this, 7) as Rect);
  }
}

export class SVGSVGElement extends SVGGraphicsElement {}

export class SVGTextContentElement extends SVGGraphicsElement {
  getComputedTextLength(): number {
    return nativeRead(this, 8) as number;
  }
}

const svgGraphics = new Set("a circle defs ellipse foreignObject g image line path polygon polyline rect marker pattern switch symbol use".split(" "));
const svgText = new Set(["text", "textPath", "tspan"]);

function createElementObject(name: string, namespace: string | null, id: number, owner: Document, prefix: string | null = null,
                             override?: typeof HTMLElement): Element {
  if (namespace === SVG_NAMESPACE) {
    const type = name === "svg" ? SVGSVGElement : svgText.has(name) ? SVGTextContentElement :
      svgGraphics.has(name) ? SVGGraphicsElement : SVGElement;
    return new type(name, namespace, id, owner, prefix);
  }
  const Type = override ?? (namespace === HTML_NAMESPACE ? (name === "template" ? TemplateElement : htmlElementTypes.get(name) ??
    (ordinaryHtmlElements.has(name) || isValidCustomElementName(name) ? HTMLElement : HTMLUnknownElement)) : Element);
  internalElement = true;
  try {
    return new Type(name, namespace, id, owner, prefix);
  } finally {
    internalElement = false;
  }
}

function recordElement(document: Document, id: number, name: string, namespace: string | null): void {
  const { journal } = documents.get(document)!;
  if (journal === null) return;
  const tag = journal.intern(AtomKind.Tag, name);
  const ns = namespace === null ? 0 : journal.intern(AtomKind.Namespace, namespace);
  journal.record(Opcode.CreateElement, id, tag, ns);
}

/** Template contents belong to an inert document in browsers, which has no custom element registry. */
export function inTemplateContents(node: Node): boolean {
  let root = node;
  while (root.parentNode !== null) root = root.parentNode;
  return nodeState(root).templateContent === true;
}

/** "Create an element" with the synchronous custom elements flag set, for a defined autonomous element. */
function constructCustomElement(document: Document, definition: CustomElementDefinition, localName: string,
                                prefix: string | null): Element {
  try {
    const result: unknown = Reflect.construct(definition.constructor, []);
    if (!(result instanceof HTMLElement) || !nodes.has(result)) {
      throw new TypeError("The custom element constructor did not return an HTMLElement");
    }
    if (result.hasAttributes()) fail("NotSupportedError", "A newly constructed custom element must not have attributes");
    if (result.hasChildNodes()) fail("NotSupportedError", "A newly constructed custom element must not have children");
    if (result.parentNode !== null) fail("NotSupportedError", "A newly constructed custom element must not have a parent");
    if (result.ownerDocument !== document) fail("NotSupportedError", "A newly constructed custom element belongs to another document");
    if (result.localName !== localName) fail("NotSupportedError", "A newly constructed custom element has another local name");
    (result as { prefix: string | null }).prefix = prefix;
    return result;
  } catch (error) {
    reportException(error);
    const element = document[createElementInternal](localName, HTML_NAMESPACE, prefix, HTMLUnknownElement);
    markFailed(element);
    return element;
  }
}

const createElementInternal = Symbol("create element");

export class Document extends ParentNode {
  constructor(journal: Journal | null = realmDocument === undefined ? new Journal() : journalFor(realmDocument), id = 0,
              quirks: QuirksMode = 0, allocator?: Allocator | null, html = false, contentType?: string) {
    super(id, NodeType.Document, null);
    let slots: Allocator | null = null;
    if (journal !== null) {
      slots = allocator ?? allocators.get(journal) ?? { nextSlot: 1 };
      allocators.set(journal, slots);
    }
    documents.set(this, {
      journal, quirks, allocator: slots, html, contentType: contentType ?? (html ? "text/html" : "application/xml"),
      view: null, active: null, focusChain: [], focusRevision: 0, focusVisible: false,
      keyboardModality: true, url: "about:blank", ready: "complete", sheetOwners: new Map(),
    });
  }

  get compatMode(): "CSS1Compat" | "BackCompat" { return documents.get(this)!.quirks === 2 ? "BackCompat" : "CSS1Compat"; }
  get contentType(): string { return documents.get(this)!.contentType; }
  get URL(): string { return documents.get(this)!.url; }
  get documentURI(): string { return this.URL; }
  // document.cookie presents the same empty jar as a browser with cookies disabled.
  get cookie(): string { return ""; }
  set cookie(_value: string) {}
  get readyState(): ReadyState { return documents.get(this)!.ready; }
  getSelection(): Selection | null {
    const state = documents.get(this)!;
    return state.view === null ? null : state.selection ??= new Selection(selectionToken, this);
  }

  get scrollingElement(): Element | null {
    return documents.get(this)!.quirks === 2 ? this.body : this.documentElement;
  }

  get activeElement(): Element | null { return documents.get(this)!.active ?? this.body ?? this.documentElement; }
  get defaultView(): EventTarget | null { return documents.get(this)!.view; }
  get styleSheets(): StyleSheetList {
    const state = documents.get(this)!;
    return state.styleSheets ??= styleSheetList(() => {
      const list: CSSStyleSheet[] = [];
      for (const node of descendants(this)) if (isSheetOwner(node) && node.sheet !== null) list.push(node.sheet);
      return list;
    });
  }

  get documentElement(): Element | null { return this.firstElementChild; }
  get doctype(): DocumentType | null { return childrenOf(this).find((child) => child instanceof DocumentType) ?? null; }
  get head(): Element | null { return this.#htmlChild("head"); }
  get body(): Element | null { return this.#htmlChild("body") ?? this.#htmlChild("frameset"); }
  get scripts(): HTMLCollection<HTMLScriptElement> {
    return byTagNS(this, HTML_NAMESPACE, "script") as HTMLCollection<HTMLScriptElement>;
  }

  #titleElement(): Element | undefined {
    for (const node of descendants(this)) {
      if (node instanceof Element && node.namespaceURI === HTML_NAMESPACE && node.localName === "title") return node;
    }
  }
  get title(): string {
    return (this.#titleElement()?.textContent ?? "").replace(/[\t\n\f\r ]+/g, " ").replace(/^ | $/g, "");
  }
  set title(value: string) {
    let title = this.#titleElement();
    if (title === undefined) {
      const head = this.head;
      if (head === null) return;
      title = this.createElement("title");
      head.appendChild(title);
    }
    title.textContent = String(value);
  }

  #htmlChild(name: string): Element | null {
    const root = this.documentElement;
    if (root?.namespaceURI !== HTML_NAMESPACE || root.localName !== "html") return null;
    return [...root.children].find((child) => child.namespaceURI === HTML_NAMESPACE && child.localName === name) ?? null;
  }

  createElement(name: string, _options?: unknown): Element {
    if (arguments.length === 0) throw new TypeError("createElement requires a name");
    const { html } = documents.get(this)!;
    const localName = validateLocalName(html ? lowerAscii(String(name)) : String(name), true);
    const namespace = html ? HTML_NAMESPACE : null;
    const definition = lookupDefinition(this, namespace, localName);
    if (definition !== null) return constructCustomElement(this, definition, localName, null);
    return this[createElementInternal](localName, namespace, null);
  }

  createElementNS(namespace: string | null, name: string, _options?: unknown): Element {
    if (arguments.length < 2) throw new TypeError("createElementNS requires namespace and name");
    namespace = namespace == null || namespace === "" ? null : String(namespace);
    const { prefix, localName } = qualifiedName(namespace, String(name));
    const definition = lookupDefinition(this, namespace, localName);
    if (definition !== null) return constructCustomElement(this, definition, localName, prefix);
    return this[createElementInternal](localName, namespace, prefix);
  }

  [createElementInternal](name: string, namespace: string | null, prefix: string | null, override?: typeof HTMLElement): Element {
    const id = allocate(this);
    recordElement(this, id, name, namespace);
    const element = createElementObject(name, namespace, id, this, prefix, override);
    if (element instanceof TemplateElement) {
      const contents = new DocumentFragment(this, undefined, id);
      templateContents.set(element, contents);
      nodeState(contents).templateContent = true;
    }
    return element;
  }

  createTextNode(data: string): Text {
    if (arguments.length === 0) throw new TypeError("createTextNode requires data");
    return new Text(String(data), this);
  }

  createComment(data: string): Comment {
    if (arguments.length === 0) throw new TypeError("createComment requires data");
    return new Comment(String(data), this);
  }

  createDocumentFragment(): DocumentFragment { return new DocumentFragment(this); }
  createTreeWalker(root: Node, whatToShow = NodeFilter.SHOW_ALL, filter: Parameters<typeof treeWalker>[2] = null): TreeWalker {
    return treeWalker(root, whatToShow, filter);
  }
  createEvent(interfaceName: string): Event { return createLegacyEvent(interfaceName); }
  getElementById(id: string): Element | null { return byId(this, String(id)); }
  getElementsByTagName(name: string): HTMLCollection<Element> { return byTag(this, String(name)); }
  getElementsByTagNameNS(namespace: string | null, name: string): HTMLCollection<Element> { return byTagNS(this, namespace, String(name)); }
  getElementsByClassName(names: string): HTMLCollection<Element> { return byClass(this, String(names)); }

  importNode<T extends Node>(node: T, deep = false): T {
    expectNode(node);
    if (node instanceof Document) fail("NotSupportedError", "Cannot import a Document");
    return cloneNodeInto(node, this, Boolean(deep)) as T;
  }

  adoptNode<T extends Node>(node: T): T {
    expectNode(node);
    if (node instanceof Document) fail("NotSupportedError", "Cannot adopt a Document");
    if (node.parentNode !== null) unlink(node.parentNode, node);
    adoptSubtree(this, node);
    return node;
  }

  execCommand(commandId: string, _showUI = false, _value = ""): boolean {
    if (arguments.length === 0) throw new TypeError("execCommand requires a command");
    void String(commandId);
    return false;
  }

  queryCommandSupported(commandId: string): boolean {
    if (arguments.length === 0) throw new TypeError("queryCommandSupported requires a command");
    void String(commandId);
    return false;
  }

  queryCommandEnabled(commandId: string): boolean {
    if (arguments.length === 0) throw new TypeError("queryCommandEnabled requires a command");
    void String(commandId);
    return false;
  }
}

export class XMLDocument extends Document {}

const SUPPORTED_PARSER_TYPES = new Set([
  "text/html",
  "text/xml",
  "application/xml",
  "application/xhtml+xml",
  "image/svg+xml",
]);

export class DOMParser {
  parseFromString(string: string, type: string): Document {
    if (arguments.length < 2) {
      throw new TypeError("DOMParser.parseFromString requires 2 arguments");
    }
    const text = String(string);
    const mime = String(type);
    if (!SUPPORTED_PARSER_TYPES.has(mime)) {
      throw new TypeError(
        `Failed to execute 'parseFromString' on 'DOMParser': The provided value '${mime}' is not a valid enum value of type SupportedType.`,
      );
    }
    const bindings = realmDocument !== undefined ? documents.get(realmDocument)?.bindings : undefined;
    const url = realmDocument !== undefined ? documents.get(realmDocument)!.url : "about:blank";
    if (mime === "text/html") {
      if (bindings?.parseHtml === undefined) {
        fail("NotSupportedError", "HTML document parsing requires the document engine");
      }
      const parsed = bindings.parseHtml(text);
      const doc = new Document(null, 0, parsed.quirksMode, null, true, mime);
      documents.get(doc)!.url = url;
      materializeDocument(doc, parsed.nodes, true);
      return doc;
    }
    if (bindings?.parseXml === undefined) {
      fail("NotSupportedError", "XML document parsing requires the document engine");
    }
    const result = bindings.parseXml(text);
    const doc = new XMLDocument(null, 0, 0, null, false, mime);
    documents.get(doc)!.url = url;
    if (Array.isArray(result)) {
      materializeDocument(doc, result, false);
      return doc;
    }
    buildParserErrorDocument(doc, result.error, result.line, result.column);
    return doc;
  }
}

const voidElements = new Set("area base br col embed hr img input link meta param source track wbr".split(" "));
const rawText = new Set("iframe noembed noframes plaintext script style xmp".split(" "));
function htmlEscape(value: string, attribute: boolean): string {
  value = value.replaceAll("&", "&amp;").replaceAll("\u00a0", "&nbsp;");
  return attribute ? value.replaceAll('"', "&quot;") : value.replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}

function htmlChildren(node: Element): string {
  const parent = node instanceof TemplateElement ? node.content ?? node : node;
  return [...parent.childNodes].map(serializeHTML).join("");
}

function serializeHTML(node: Node): string {
  if (node instanceof Text) {
    const parent = node.parentElement;
    return parent?.namespaceURI === HTML_NAMESPACE && rawText.has(parent.localName) ? node.data : htmlEscape(node.data, false);
  }
  if (node instanceof Comment) return "<!--" + node.data + "-->";
  if (node instanceof DocumentType) return "<!DOCTYPE " + node.name + ">";
  if (!(node instanceof Element)) return "";
  const name = node.prefix === null ? node.localName : node.prefix + ":" + node.localName;
  let html = "<" + name;
  for (const attribute of nodeState(node).attributes?.values() ?? []) {
    html += " " + attribute.name + '="' + htmlEscape(attribute.value, true) + '"';
  }
  html += ">";
  return node.namespaceURI === HTML_NAMESPACE && voidElements.has(node.localName) ?
    html : html + htmlChildren(node) + "</" + name + ">";
}

function xmlEscape(value: string, attribute: boolean): string {
  value = value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  return attribute ? value.replaceAll('"', "&quot;") : value;
}

function xmlChildren(node: Element): string {
  const parent = node instanceof TemplateElement ? node.content ?? node : node;
  return [...parent.childNodes].map(serializeXML).join("");
}

function serializeXML(node: Node): string {
  if (node instanceof Text) return xmlEscape(node.data, false);
  if (node instanceof Comment) return "<!--" + node.data + "-->";
  if (node instanceof DocumentType) return "<!DOCTYPE " + node.name + ">";
  if (!(node instanceof Element)) return "";
  const name = node.prefix === null ? node.localName : node.prefix + ":" + node.localName;
  let xml = "<" + name;
  for (const attribute of nodeState(node).attributes?.values() ?? []) {
    xml += " " + attribute.name + '="' + xmlEscape(attribute.value, true) + '"';
  }
  const children = xmlChildren(node);
  return children === "" ? xml + "/>" : xml + ">" + children + "</" + name + ">";
}

for (const name of ["input", "change", "click", "focus", "blur", "copy", "cut", "paste"]) reflectedEvent(Document.prototype, name);

ceReactions(Node.prototype, ["nodeValue", "textContent", "appendChild", "insertBefore", "removeChild", "replaceChild", "normalize", "cloneNode"]);
ceReactions(ParentNode.prototype, ["append", "prepend", "replaceChildren"]);
ceReactions(CharacterData.prototype, ["before", "after", "replaceWith", "remove"]);
ceReactions(DocumentType.prototype, ["remove"]);
ceReactions(Element.prototype, ["before", "after", "replaceWith", "remove", "innerHTML", "insertAdjacentElement", "insertAdjacentText",
  "insertAdjacentHTML", "id", "className", "setAttribute", "setAttributeNS", "removeAttribute", "removeAttributeNS", "toggleAttribute"]);
ceReactions(HTMLElement.prototype, ["innerText", "tabIndex"]);
ceReactions(Document.prototype, ["createElement", "createElementNS", "importNode", "adoptNode", "title"]);
ceReactions(CSSStyleDeclaration.prototype, ["cssText", "setProperty", "removeProperty"]);

function attributeKey(namespace: string | null, localName: string): string { return `${namespace ?? ""}\0${localName}`; }

function attributeAtomFor(journal: Journal, attribute: Attribute): number {
  return attribute.namespaceURI === null ? journal.intern(AtomKind.Attribute, attribute.name) :
    journal.intern(AtomKind.QualifiedAttribute, attributeKey(attribute.namespaceURI, attribute.name));
}

function storeAttribute(element: Element, attribute: Attribute): void {
  (nodeState(element).attributes ??= new Map()).set(attributeKey(attribute.namespaceURI, attribute.localName), attribute);
}

function writeAttribute(element: Element, attribute: Attribute, initial = false): void {
  const oldValue = initial ? null : element.getAttributeNS(attribute.namespaceURI, attribute.localName);
  storeAttribute(element, attribute);
  const journal = journalFor(element);
  if (journal !== null) {
    journal.record(Opcode.SetAttr, nodeId(element), attributeAtomFor(journal, attribute), attribute.value);
  }
  if (!initial) {
    queueMutation(element, { type: "attributes", name: attribute.localName, namespace: attribute.namespaceURI, oldValue });
    customElementAttributeChanged(element, attribute.localName, oldValue, attribute.value, attribute.namespaceURI);
  }
  if (attribute.namespaceURI === null && element instanceof HTMLElement) element[attributeChanged](attribute.name, oldValue);
}

function eraseAttribute(element: Element, attribute: Attribute): void {
  nodeState(element).attributes!.delete(attributeKey(attribute.namespaceURI, attribute.localName));
  const journal = journalFor(element);
  if (journal !== null) {
    journal.record(Opcode.RemoveAttr, nodeId(element), attributeAtomFor(journal, attribute), 0);
  }
  queueMutation(element, { type: "attributes", name: attribute.localName, namespace: attribute.namespaceURI, oldValue: attribute.value });
  customElementAttributeChanged(element, attribute.localName, attribute.value, null, attribute.namespaceURI);
  if (attribute.namespaceURI === null && element instanceof HTMLElement) element[attributeChanged](attribute.name, attribute.value);
}

function unlink(parent: Node, child: Node, suppressObserver = false, syncText = true): void {
  const journal = journalFor(parent);
  const previous = suppressObserver ? null : child.previousSibling;
  const next = suppressObserver ? null : child.nextSibling;
  observeDetachedSubtree(child, parent);
  detach(parent, child);
  journal?.record(Opcode.Remove, nodeId(parent), nodeId(child), 0);
  if (!suppressObserver) queueMutation(parent, { type: "childList", added: [], removed: [child], previous, next });
  treeChanged(parent, child, false);
  if (syncText && parent instanceof HTMLStyleElement && child instanceof Text) syncStyleText(parent, true);
}

function cloneNodeInto(source: Node, targetDoc: Document, deep: boolean, upgrade = true): Node {
  let clone: Node;
  if (source instanceof Element) {
    clone = targetDoc[createElementInternal](source.localName, source.namespaceURI, source.prefix);
    const definition = upgrade ? lookupDefinition(targetDoc, source.namespaceURI, source.localName) : null;
    if (definition !== null) enqueueUpgrade(clone as Element, definition);
    for (const attribute of nodeState(source).attributes?.values() ?? []) writeAttribute(clone as Element, { ...attribute }, true);
    if (source instanceof HTMLInputElement && clone instanceof HTMLInputElement) source[cloneInput](clone);
  } else if (source instanceof Text) clone = targetDoc.createTextNode(source.data);
  else if (source instanceof Comment) clone = targetDoc.createComment(source.data);
  else if (source instanceof DocumentType) {
    const id = allocate(targetDoc);
    journalFor(targetDoc)?.record(Opcode.CreateNode, id, NodeType.DocumentType, source.name);
    clone = new DocumentType(id, targetDoc, source.name, source.publicId, source.systemId);
  } else if (source instanceof Document) {
    const { journal, quirks, allocator, html, contentType } = documents.get(source)!;
    clone = source instanceof XMLDocument
      ? new XMLDocument(journal, 0, quirks, allocator, html, contentType)
      : new Document(journal, 0, quirks, allocator, html, contentType);
  } else clone = targetDoc.createDocumentFragment();
  if (deep) {
    for (const child of source.childNodes) clone.appendChild(cloneNodeInto(child, targetDoc, true, upgrade));
    if (source instanceof TemplateElement && clone instanceof TemplateElement && source.content && clone.content) {
      for (const child of source.content.childNodes) clone.content.appendChild(cloneNodeInto(child, targetDoc, true, false));
    }
  }
  return clone;
}

function recordSubtree(journal: Journal, allocateId: () => number, node: Node,
                       onNode?: (node: Node, id: number, parentId: number) => void, parentId = 0): number {
  const id = allocateId();
  onNode?.(node, id, parentId);
  const state = nodeState(node);
  if (node instanceof Element) {
    const tag = journal.intern(AtomKind.Tag, node.localName);
    const ns = node.namespaceURI === null ? 0 : journal.intern(AtomKind.Namespace, node.namespaceURI);
    journal.record(Opcode.CreateElement, id, tag, ns);
    for (const attribute of state.attributes?.values() ?? []) {
      journal.record(Opcode.SetAttr, id, attributeAtomFor(journal, attribute), attribute.value);
    }
    if (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement) {
      const value = node.value;
      if (value !== "") journal.record(Opcode.SetValue, id, value, 0);
    }
    if (node instanceof HTMLInputElement) {
      if (node.checked) journal.record(Opcode.SetState, id, ElementState.Checked, ElementState.Checked);
      if (node.indeterminate) journal.record(Opcode.SetState, id, ElementState.Indeterminate, ElementState.Indeterminate);
    }
    if (node instanceof HTMLOptionElement && node.selected) {
      journal.record(Opcode.SetState, id, ElementState.Selected, ElementState.Selected);
    }
    if (node instanceof HTMLElement) {
      if (node.hasAttribute("disabled") && (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement ||
          node instanceof HTMLSelectElement || node instanceof HTMLOptionElement || node instanceof HTMLButtonElement ||
          node.localName === "fieldset" || node.localName === "optgroup")) {
        journal.record(Opcode.SetState, id, ElementState.Disabled, ElementState.Disabled);
      }
      if (node.hasAttribute("readonly") && (node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement)) {
        journal.record(Opcode.SetState, id, ElementState.ReadOnly, ElementState.ReadOnly);
      }
    }
    if (node instanceof TemplateElement && node.content !== undefined) {
      const contentId = allocateId();
      onNode?.(node.content, contentId, 0);
      journal.record(Opcode.CreateNode, contentId, NodeType.DocumentFragment, id);
      for (const child of childrenOf(node.content)) {
        const childId = recordSubtree(journal, allocateId, child, onNode, contentId);
        journal.record(Opcode.Insert, contentId, childId, 0);
      }
    }
  } else if (node instanceof Text) {
    journal.record(Opcode.CreateText, id, node.data, 0);
  } else if (node instanceof Comment) {
    journal.record(Opcode.CreateNode, id, NodeType.Comment, node.data);
  } else if (node instanceof DocumentType) {
    journal.record(Opcode.CreateNode, id, NodeType.DocumentType, node.name);
  } else if (node instanceof DocumentFragment) {
    journal.record(Opcode.CreateNode, id, NodeType.DocumentFragment, NO_STRING);
  }
  for (const child of childrenOf(node)) {
    const childId = recordSubtree(journal, allocateId, child, onNode, id);
    journal.record(Opcode.Insert, id, childId, 0);
  }
  return id;
}

function setOwnerSubtree(targetDoc: Document, root: Node): void {
  const pending: Node[] = [root];
  while (pending.length !== 0) {
    const current = pending.pop()!;
    const state = nodeState(current);
    state.owner = targetDoc;
    for (const child of state.children ?? noChildren) pending.push(child);
    if (current instanceof TemplateElement && current.content !== undefined) pending.push(current.content);
  }
}

function demoteSubtree(sourceJournal: Journal, targetDoc: Document, root: Node): void {
  const tracker = runtimeNodes.get(sourceJournal);
  const detachAndRelease = (node: Node) => {
    const state = nodeState(node);
    const id = state.id;
    for (const child of [...(state.children ?? noChildren)]) {
      const childId = nodeState(child).id;
      if (id !== 0 && childId !== 0) sourceJournal.record(Opcode.Remove, id, childId, 0);
      detachAndRelease(child);
    }
    if (node instanceof TemplateElement && node.content !== undefined) {
      detachAndRelease(node.content);
    }
    if (id !== 0 && !(node instanceof Document)) {
      if (state.tracked !== undefined) {
        cleanupNodes.unregister(node);
        tracker?.index.delete(id);
        state.tracked = undefined;
      }
      sourceJournal.release(id);
      state.id = 0;
    }
    state.owner = targetDoc;
  };
  detachAndRelease(root);
}

function adoptSubtree(targetDoc: Document, root: Node): void {
  const oldDocument = ownerOf(root);
  if (oldDocument === targetDoc) return;
  const sourceJournal = journalFor(root);
  const targetJournal = journalFor(targetDoc);
  if (sourceJournal === null && targetJournal !== null) {
    recordSubtree(targetJournal, () => allocate(targetDoc), root, (node, id, parentId) => {
      const state = nodeState(node);
      state.owner = targetDoc;
      state.id = id;
      rememberNode(targetJournal, state, node, true);
      if (state.tracked !== undefined) state.tracked.parent = parentId;
    });
  } else if (sourceJournal !== null && targetJournal === null) {
    demoteSubtree(sourceJournal, targetDoc, root);
  } else if (sourceJournal === targetJournal) {
    setOwnerSubtree(targetDoc, root);
  } else {
    fail("WrongDocumentError", "Nodes use different journal sessions");
  }
  if (customElementsActive) {
    customElementAdopted(root, oldDocument, targetDoc);
    for (const node of descendants(root)) customElementAdopted(node, oldDocument, targetDoc);
  }
}

function mutate(parent: Node, incoming: readonly Node[], before: Node | null, removed: readonly Node[] = []): void {
  if (!(parent instanceof ParentNode)) fail("HierarchyRequestError", "This node cannot have children");
  if (before !== null) {
    expectNode(before);
    if (before.parentNode !== parent) fail("NotFoundError", "The reference node is not a child");
  }
  const targetDoc = ownerOf(parent);
  const journal = journalFor(parent);
  const children = new Set<Node>();
  for (const child of incoming) {
    expectNode(child);
    if (child.contains(parent) || child instanceof Document ||
        child instanceof DocumentType && !(parent instanceof Document)) {
      fail("HierarchyRequestError", "Cannot insert an ancestor or this node kind into its descendant");
    }
    const childJournal = journalFor(child);
    if (childJournal !== journal && childJournal !== null && journal !== null) {
      fail("WrongDocumentError", "Nodes use different journal sessions");
    }
    for (const item of child instanceof DocumentFragment ? child.childNodes : [child]) {
      children.delete(item);
      children.add(item);
    }
  }

  const replacing = new Set([...removed, ...children]);
  let reference = before;
  while (reference !== null && replacing.has(reference)) reference = reference.nextSibling;
  if (parent instanceof Document) {
    const remaining = [...parent.childNodes].filter((child) => !replacing.has(child));
    remaining.splice(reference === null ? remaining.length : remaining.indexOf(reference), 0, ...children);
    const root = remaining.findIndex((child) => child instanceof Element);
    const doctype = remaining.findIndex((child) => child instanceof DocumentType);
    if (remaining.some((child, index) => child instanceof Text ||
        child instanceof Element && index !== root || child instanceof DocumentType && index !== doctype) ||
        root !== -1 && doctype > root) {
      fail("HierarchyRequestError", "Invalid document children");
    }
  }

  for (const child of removed) unlink(parent, child, true, false);
  for (const fragment of incoming) {
    if (fragment instanceof DocumentFragment && fragment.hasChildNodes()) {
      const detached = [...fragment.childNodes];
      for (const child of detached) unlink(fragment, child, true);
      queueMutation(fragment, { type: "childList", added: [], removed: detached, previous: null, next: null });
    }
  }
  const added = [...children];
  for (const child of children) {
    if (child.parentNode !== null) unlink(child.parentNode, child, false, child.parentNode !== parent);
    if (ownerOf(child) !== targetDoc) adoptSubtree(targetDoc, child);
    insert(parent, child, reference);
    journal?.record(Opcode.Insert, nodeId(parent), nodeId(child), reference === null ? 0 : nodeId(reference));
    treeChanged(parent, child, true);
  }
  if (parent instanceof HTMLStyleElement && (added.some(node => node instanceof Text) || removed.some(node => node instanceof Text))) {
    syncStyleText(parent, true);
  }
  if (added.length !== 0 || removed.length !== 0) {
    const previous = added.length > 0 ? added[0].previousSibling :
      reference !== null ? reference.previousSibling : parent.lastChild;
    queueMutation(parent, { type: "childList", added, removed: [...removed], previous, next: reference });
  }
}

function replaceParsedChildren(parent: ParentNode, added: Node[]): void {
  const state = nodeState(parent);
  const removed = (state.children ?? noChildren).slice();
  const journal = journalFor(parent);
  const id = nodeId(parent);
  for (const child of removed) unlink(parent, child, true, false);
  // The parser only produces fresh nodes owned by this document.
  for (const child of added) {
    const childId = linkFreshChild(parent, state, child);
    journal?.record(Opcode.Insert, id, childId, 0);
    treeChanged(parent, child, true);
  }
  if (parent instanceof HTMLStyleElement && (added.some(node => node instanceof Text) || removed.some(node => node instanceof Text))) {
    syncStyleText(parent, true);
  }
  if (added.length !== 0 || removed.length !== 0) {
    const previous = added.length > 0 ? added[0].previousSibling : parent.lastChild;
    queueMutation(parent, { type: "childList", added, removed, previous, next: null });
  }
}

function linkFreshChild(parent: Node, parentState: NodeState, child: Node): number {
  const state = nodeState(child);
  (parentState.children ??= []).push(child);
  state.parent = parent;
  if (state.tracked !== undefined) state.tracked.parent = parentState.id;
  return state.id;
}

function detach(parent: Node, child: Node): void {
  const state = nodeState(child);
  if (state.parent !== parent) {
    throw new Error("Cannot remove a node from a different parent");
  }
  const siblings = nodeState(parent).children!;
  siblings.splice(siblings.indexOf(child), 1);
  state.parent = null;
  if (state.tracked !== undefined) state.tracked.parent = 0;
}

function insert(parent: Node, child: Node, before: Node | null): void {
  if (parent.nodeType !== ELEMENT && parent.nodeType !== NodeType.Document &&
      parent.nodeType !== NodeType.DocumentFragment) {
    throw new Error("Cannot insert into a node with no children");
  }
  if (child.nodeType === NodeType.Document || nodeState(child).templateContent ||
      (before !== null && before.parentNode !== parent)) {
    throw new Error("Invalid journal insertion");
  }
  for (let ancestor: Node | null = parent; ancestor !== null; ancestor = ancestor.parentNode) {
    if (ancestor === child) {
      throw new Error("Cannot append an ancestor to its descendant");
    }
  }
  if (before === child) {
    return;
  }

  const oldParent = child.parentNode;
  if (oldParent !== null) {
    detach(oldParent, child);
  }
  const parentState = nodeState(parent);
  const childState = nodeState(child);
  const siblings = parentState.children ??= [];
  const index = before === null ? siblings.length : siblings.indexOf(before);
  siblings.splice(index, 0, child);
  childState.parent = parent;
  if (childState.tracked !== undefined) childState.tracked.parent = parentState.id;

  const owner = parent instanceof Document ? parent : parent.ownerDocument;
  if (owner !== child.ownerDocument) {
    const pending = [child];
    while (pending.length !== 0) {
      const state = nodeState(pending.pop()!);
      state.owner = owner;
      for (const descendant of state.children ?? noChildren) {
        pending.push(descendant);
      }
    }
  }
}

function applyAttribute(node: Node, atom: JournalAtom, value: string | null): void {
  if (!(node instanceof Element)) {
    throw new Error("Journal attributes require an element");
  }
  let name = atom.text;
  let namespace = "";
  let localName = name;
  if (atom.kind === AtomKind.QualifiedAttribute) {
    const separator = atom.text.indexOf("\0");
    namespace = atom.text.slice(0, separator);
    name = atom.text.slice(separator + 1);
    localName = name.slice(name.indexOf(":") + 1);
  }

  const key = attributeKey(namespace, localName);
  if (value === null) {
    nodeState(node).attributes?.delete(key);
  } else {
    const prefix = namespace !== "" && name.includes(":") ? name.slice(0, name.indexOf(":")) : null;
    storeAttribute(node, { name, localName, value, namespaceURI: namespace || null, prefix });
  }
  if (node instanceof HTMLImageElement && namespace === "" && localName === "src") node[imageSourceChanged]();
}

export function createDocument(journal: Journal): Document {
  return new Document(journal, 0, 0, undefined, true);
}

/** NodeId of a packed stylesheet is also its sheet id. Internal to squark-dom. */
export function packedSheetOwner(document: Document, sheetId: number): Element | undefined {
  return documents.get(document)!.sheetOwners.get(sheetId);
}

/** Called by squark-js with batches already validated by squark-journal. */
export function bootstrapDocument(
  journal: Journal,
  documentId: number,
  quirks: QuirksMode,
  batches: readonly BootstrapBatch[],
): Document {
  const allocator = { nextSlot: (documentId & MAX_SLOT) + 1 };
  const document = new Document(journal, documentId, quirks, allocator, true);
  runtimeNodes.set(journal, { journal, index: new Map() });
  rememberNode(journal, nodeState(document), document, false);
  const byId = new Map<number, Node>([[documentId, document]]);
  const atoms = new Map<number, JournalAtom>();
  const sheetOwners = documents.get(document)!.sheetOwners;
  const add = (id: number, node: Node) => {
    byId.set(id, node);
    if (node instanceof Document) rememberNode(journal, nodeState(node), node, false);
    allocator.nextSlot = Math.max(allocator.nextSlot, (id & MAX_SLOT) + 1);
  };
  const node = (id: number) => byId.get(id)!;

  for (const [records, strings] of batches) {
    const view = new DataView(records);
    for (let offset = 0; offset < view.byteLength; offset += RECORD_BYTES) {
      const a = view.getUint32(offset + 4, true);
      const b = view.getUint32(offset + 8, true);
      const c = view.getUint32(offset + 12, true);
      const opcode = view.getUint8(offset);
      switch (opcode) {
        case Opcode.DefineAtom:
          atoms.set(a, { kind: c as JournalAtom["kind"], text: strings[b] });
          break;
        case Opcode.CreateElement: {
          const name = atoms.get(b)!.text;
          const namespace = c === 0 ? null : atoms.get(c)!.text;
          add(a, createElementObject(name, namespace, a, document));
          break;
        }
        case Opcode.CreateText:
          add(a, new Text(strings[b], document, a));
          break;
        case Opcode.CreateNode:
          if (b === NodeType.Document) {
            add(a, new Document(journal, a, quirks, allocator));
          } else if (b === NodeType.DocumentType) {
            add(a, new DocumentType(a, document, strings[c]));
          } else if (b === NodeType.Comment) {
            add(a, new Comment(strings[c], document, a));
          } else {
            const created = new DocumentFragment(document, a);
            if (c !== NO_STRING) {
              const template = node(c);
              if (!(template instanceof TemplateElement) || templateContents.has(template)) {
                throw new Error("Invalid owner of a template content fragment");
              }
              templateContents.set(template, created);
              nodeState(created).templateContent = true;
            }
            add(a, created);
          }
          break;
        case Opcode.Insert:
          insert(node(a), node(b), c === 0 ? null : node(c));
          break;
        case Opcode.Remove:
          detach(node(a), node(b));
          break;
        case Opcode.SetAttr:
        case Opcode.RemoveAttr:
          applyAttribute(node(a), atoms.get(b)!, opcode === Opcode.SetAttr ? strings[c] : null);
          break;
        case Opcode.SetText:
          if (nodeState(node(a)).value === null) {
            throw new Error("Journal text update requires a text or comment node");
          }
          nodeState(node(a)).value = strings[b];
          break;
        case Opcode.SheetInsert:
        case Opcode.SheetDelete: {
          const owner = node(a);
          if (!isSheetOwner(owner)) throw new Error("Packed stylesheet requires an owning style or link element");
          sheetOwners.set(a, owner);
          const state = backing(owner);
          if (opcode === Opcode.SheetInsert ? b > state.count : b >= state.count) {
            throw new Error("Packed stylesheet rule index is out of bounds");
          }
          state.count += opcode === Opcode.SheetInsert ? 1 : -1;
          state.raw = true;
          break;
        }
        case Opcode.Release:
          if (node(a).parentNode !== null || node(a).firstChild !== null ||
              nodeState(node(a)).templateContent) {
            throw new Error("Cannot release a linked node during bootstrap");
          }
          byId.delete(a);
          runtimeNodes.get(journal)!.index.delete(a);
          sheetOwners.delete(a);
          break;
        default:
          throw new Error(`Unexpected opcode ${opcode} in a packed static document`);
      }
    }
  }

  for (const owner of descendants(document)) {
    if (owner instanceof HTMLStyleElement) {
      const state = backing(owner);
      state.text = styleText(owner);
      state.raw ||= state.text !== "";
    }
  }
  journal.restoreAtoms(atoms);
  return document;
}

/** Installs the Node-test realm around a supplied document or an empty HTML tree. */
export function installDOM(global: Record<PropertyKey, unknown>, supplied?: Document, options?: { url?: string }): Document {
  const document = supplied ?? createDocument(new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES));
  realmDocument = document;
  const state = documents.get(document)!;
  state.ready = "loading";
  state.view = installWindowEventTarget(global);
  observeListenerFlags(state.view, flags => state.journal?.record(Opcode.Listeners, 0, flags, 0));
  if (options?.url !== undefined) state.url = options.url;
  installCustomElements(global, document);
  Object.assign(global, {
    window: global, self: global, opener: null, Window, document, Node, Element, CharacterData, Text, Comment, DocumentFragment,
    Document, XMLDocument, DOMParser, DocumentType, EventTarget, Event, UIEvent, CompositionEvent, CustomEvent, FocusEvent, MouseEvent, KeyboardEvent,
    PointerEvent, WheelEvent, InputEvent, SubmitEvent, ErrorEvent, ProgressEvent,
    CSSStyleSheet, CSSRule, CSSRuleList, StyleSheetList, HTMLStyleElement, HTMLLinkElement,
    SVGElement, SVGGraphicsElement, SVGSVGElement, SVGTextContentElement, NodeFilter, TreeWalker, Selection,
    getSelection: () => document.getSelection(),
    AbortController, AbortSignal, DOMException, NodeList, HTMLCollection, DOMTokenList, MutationObserver, MutationRecord, CSSStyleDeclaration,
    HTMLElement, HTMLInputElement, HTMLTextAreaElement, HTMLSelectElement, HTMLOptionElement, HTMLBRElement,
    HTMLButtonElement, HTMLFormElement, HTMLFormControlsCollection, RadioNodeList, HTMLLabelElement, HTMLIFrameElement, HTMLAnchorElement, HTMLCanvasElement, HTMLImageElement, HTMLUnknownElement, HTMLScriptElement,
    HTMLTemplateElement,
  });
  for (const type of elementInterfaces) global[type.name] = type;
  function Image(width?: number, height?: number): HTMLImageElement {
    if (new.target === undefined) throw new TypeError("Image requires new");
    const image = document.createElement("img") as HTMLImageElement;
    if (width !== undefined) image.width = width;
    if (height !== undefined) image.height = height;
    return image;
  }
  Image.prototype = HTMLImageElement.prototype;
  global.Image = Image;
  Object.defineProperties(global, {
    parent: { get: () => global, configurable: true, enumerable: true },
    top: { get: () => global, configurable: true, enumerable: true },
  });
  if (global.reportError === undefined) {
    const host = global.__squark as {
      logError?(message: string): void;
      reportScriptError?(error: unknown, filename: string, lineno: number, colno: number, message: string): void;
    } | undefined;
    let reporting = false;
    const report = (error: unknown, filename = "", lineno = 0, colno = 0, reported?: string) => {
      const details = typeof error === "object" && error !== null ? error as Error : undefined;
      const message = reported ?? (typeof details?.message === "string" ? details.message : String(error));
      const console = global.console as { error(error: unknown): void } | undefined;
      const log = () => {
        host?.logError?.(typeof details?.stack === "string" ? details.stack : message);
        console?.error(error);
      };
      if (reporting) { log(); return; }
      reporting = true;
      try {
        const event = new ErrorEvent("error", { cancelable: true, error, message, filename, lineno, colno });
        if (dispatchTrustedEvent(documents.get(document)!.view!, event)) log();
      } finally { reporting = false; }
    };
    if (host !== undefined) host.reportScriptError = report;
    global.reportError = function reportError(error: unknown) {
      if (arguments.length === 0) throw new TypeError("reportError requires a value");
      report(error);
    };
  }
  if (supplied === undefined) {
    const id = allocate(document);
    journalFor(document)?.record(Opcode.CreateNode, id, NodeType.DocumentType, "html");
    document.appendChild(new DocumentType(id, document, "html"));
    const root = document.appendChild(document.createElement("html"));
    root.append(document.createElement("head"), document.createElement("body"));
  }
  return document;
}

export function setDocumentURL(document: Document, url: string): void {
  documents.get(document)!.url = url;
}

/** Advances startup readiness after the packed scripts have run. */
export function finishDocumentStartup(document: Document): void {
  const state = documents.get(document)!;
  if (state.ready !== "loading") return;
  state.ready = "interactive";
  dispatchTrustedEvent(document, new Event("readystatechange"));
  dispatchTrustedEvent(document, new Event("DOMContentLoaded", { bubbles: true }));
  state.ready = "complete";
  dispatchTrustedEvent(document, new Event("readystatechange"));
  if (state.view !== null) dispatchTrustedEvent(state.view, new Event("load"));
}


type ParsedName = { namespace: string | null; localName: string; prefix: string | null };
type ParsedNode = (ParsedName & { kind: "element"; attrs: (ParsedName & { value: string })[]; children: ParsedNode[] }) |
  { kind: "text" | "comment"; value: string } |
  { kind: "doctype"; name: string; publicId: string; systemId: string };
type ParsedHtmlDocument = { quirksMode: QuirksMode; nodes: ParsedNode[] };
type XmlParseError = { error: string; line: number; column: number };
type Rect = { x: number; y: number; width: number; height: number };
type DocumentBindings = {
  parseFragment(html: string, contextId: number): ParsedNode[];
  parseHtml?(html: string): ParsedHtmlDocument;
  parseXml?(xml: string): ParsedNode[] | XmlParseError;
  selectDetached?(
    batches: ArrayBuffer[],
    rootId: number,
    quirks: QuirksMode,
    isHtml: boolean,
    scopeId: number,
    selector: string,
    mode: string,
  ): unknown;
  query(kind: number, id: number, arg?: unknown): unknown;
  scrollOffset(id: number): { left: number; top: number };
  queryNumber(kind: number, id: number, field: number): number;
  postTask(callback: () => void): void;
};

function qualified({ prefix, localName }: ParsedName): string {
  return prefix === null ? localName : prefix + ":" + localName;
}

function adjacentPosition(element: Element, position: string): { parent: Node; before: Node | null } | null {
  const where = lowerAscii(String(position));
  switch (where) {
    case "beforebegin":
    case "afterend": {
      const { parentNode } = element;
      return parentNode === null ? null : {
        parent: parentNode, before: where === "beforebegin" ? element : element.nextSibling,
      };
    }
    case "afterbegin": return { parent: element, before: element.firstChild };
    case "beforeend": return { parent: element, before: null };
    default: fail("SyntaxError", "Invalid adjacent insertion position");
  }
}

function parseMarkup(context: Element, html: string): Node[] {
  if (html === "") return [];
  const document = context.ownerDocument;
  const bindings = documents.get(document)!.bindings;
  if (bindings === undefined) fail("NotSupportedError", "HTML fragment parsing requires the document engine");
  const id = nodeId(context);
  const journal = journalFor(document);
  journal?.flush();
  return bindings.parseFragment(html, id).map(node => materialize(document, journal, node, false));
}

function materialize(document: Document, journal: Journal | null, node: ParsedNode, markScripts: boolean): Node {
  if (node.kind === "doctype") {
    const id = allocate(document);
    journal?.record(Opcode.CreateNode, id, NodeType.DocumentType, node.name);
    return new DocumentType(id, document, node.name, node.publicId, node.systemId);
  }
  if (node.kind !== "element") {
    return node.kind === "text" ? new Text(node.value, document) : new Comment(node.value, document);
  }
  const element = document[createElementInternal](node.localName, node.namespace, node.prefix);
  if (markScripts && element instanceof HTMLScriptElement) parserInsertedScripts.add(element);
  for (const attr of node.attrs) {
    writeAttribute(element, {
      name: qualified(attr), localName: attr.localName, namespaceURI: attr.namespace, prefix: attr.prefix, value: attr.value,
    }, true);
  }
  const target = element instanceof TemplateElement ? element.content! : element;
  const parent = nodeState(target);
  // Each child is new, detached and already belongs to this document.
  for (const child of node.children) {
    const item = materialize(document, journal, child, markScripts);
    const childId = linkFreshChild(target, parent, item);
    journal?.record(Opcode.Insert, parent.id, childId, 0);
  }
  if (element instanceof HTMLSelectElement) element.updateSelection();
  return element;
}

function materializeDocument(document: Document, parsed: readonly ParsedNode[], markScripts: boolean): void {
  const parent = nodeState(document);
  for (const child of parsed) {
    const item = materialize(document, null, child, markScripts);
    linkFreshChild(document, parent, item);
  }
}

function buildParserErrorDocument(doc: Document, message: string, line: number, column: number): void {
  const html = doc[createElementInternal]("html", HTML_NAMESPACE, null);
  writeAttribute(html, { name: "xmlns", localName: "xmlns", prefix: null, namespaceURI: XMLNS_NAMESPACE, value: HTML_NAMESPACE }, true);
  const body = doc[createElementInternal]("body", HTML_NAMESPACE, null);
  const error = doc[createElementInternal]("parsererror", HTML_NAMESPACE, null);
  writeAttribute(error, {
    name: "style", localName: "style", prefix: null, namespaceURI: null,
    value: "display: block; white-space: pre; border: 2px solid #c77; padding: 0 1em 0 1em; margin: 1em; background-color: #fdd; color: black",
  }, true);
  const heading = doc[createElementInternal]("h3", HTML_NAMESPACE, null);
  linkFreshChild(heading, nodeState(heading), new Text("This page contains the following errors:", doc));
  const detail = doc[createElementInternal]("div", HTML_NAMESPACE, null);
  writeAttribute(detail, {
    name: "style", localName: "style", prefix: null, namespaceURI: null,
    value: "font-family:monospace;font-size:12px",
  }, true);
  linkFreshChild(detail, nodeState(detail), new Text(`error on line ${line} at column ${column}: ${message}`, doc));
  const footer = doc[createElementInternal]("h3", HTML_NAMESPACE, null);
  linkFreshChild(footer, nodeState(footer), new Text("Below is a rendering of the page up to the first error.", doc));
  const errorState = nodeState(error);
  linkFreshChild(error, errorState, heading);
  linkFreshChild(error, errorState, detail);
  linkFreshChild(error, errorState, footer);
  linkFreshChild(body, nodeState(body), error);
  linkFreshChild(html, nodeState(html), body);
  linkFreshChild(doc, nodeState(doc), html);
}

export function hasPendingRecords(document: Document): boolean {
  return journalFor(document)?.hasPendingRecords ?? false;
}

export function flushDocument(document: Document): void {
  journalFor(document)?.flush();
}

export function notifyWindowResize(global: EventTarget): void {
  dispatchTrustedEvent(global, new Event("resize"));
}

function readFromEngine<T>(node: Node, read: (bindings: DocumentBindings, id: number) => T): T {
  const document = ownerOf(node);
  const bindings = documents.get(document)!.bindings ?? documents.get(realmDocument!)?.bindings;
  if (bindings === undefined) fail("NotSupportedError", "Synchronous DOM reads require the document engine");
  const id = nodeId(node);
  flushDocument(document);
  return read(bindings, id);
}

function nativeRead(node: Node, kind: number, argument?: unknown): unknown {
  return readFromEngine(node, (bindings, id) => bindings.query(kind, kind === 6 ? 0 : id, argument));
}

function selectDetached(node: Node, selector: string, mode: string): { result: unknown; nodes: Map<number, Node> } {
  const document = ownerOf(node);
  const state = documents.get(document)!;
  const bindings = state.bindings ?? (realmDocument !== undefined ? documents.get(realmDocument)?.bindings : undefined);
  if (bindings?.selectDetached === undefined) {
    fail("NotSupportedError", "Synchronous DOM reads require the document engine");
  }
  const batches: ArrayBuffer[] = [];
  const tempJournal = new Journal((buffer, length) => {
    batches.push(buffer.slice(0, length));
    return new DataView(buffer).getUint32(0, true) / RECORD_BYTES;
  });
  let nextSlot = 2;
  const allocateTemp = () => nextSlot++;
  const idToNode = new Map<number, Node>();
  let scopeId = 0;
  const root = node.getRootNode();
  const track = (item: Node, id: number) => {
    idToNode.set(id, item);
    if (item === node) scopeId = id;
  };
  if (root instanceof Document) {
    track(root, 1);
    for (const child of childrenOf(root)) {
      const childId = recordSubtree(tempJournal, allocateTemp, child, track, 1);
      tempJournal.record(Opcode.Insert, 1, childId, 0);
    }
  } else {
    const childId = recordSubtree(tempJournal, allocateTemp, root, track, 1);
    tempJournal.record(Opcode.Insert, 1, childId, 0);
  }
  tempJournal.flush();
  try {
    const result = bindings.selectDetached(batches, 1, state.quirks, state.html, scopeId, String(selector), mode);
    return { result, nodes: idToNode };
  } catch (error) {
    if (error instanceof SyntaxError) fail("SyntaxError", error.message);
    throw error;
  }
}

function select(node: Node, selector: string, mode: string): unknown {
  if (journalFor(node) === null) return selectDetached(node, selector, mode).result;
  try {
    return nativeRead(node, 5, { selector: String(selector), mode });
  } catch (error) {
    if (error instanceof SyntaxError) fail("SyntaxError", error.message);
    throw error;
  }
}

function lookupElement(scope: Node, id: number | null, detachedNodes?: Map<number, Node>): Element | null {
  if (id === null) return null;
  if (detachedNodes !== undefined) {
    const node = detachedNodes.get(id);
    return node instanceof Element ? node : null;
  }
  const journal = journalFor(scope);
  if (journal === null) return null;
  const node = runtimeNodes.get(journal)?.index.get(id)?.ref.deref();
  return node instanceof Element ? node : null;
}

class DOMRect {
  constructor(public x = 0, public y = 0, public width = 0, public height = 0) {}
  get top(): number { return Math.min(this.y, this.y + this.height); }
  get left(): number { return Math.min(this.x, this.x + this.width); }
  get bottom(): number { return Math.max(this.y, this.y + this.height); }
  get right(): number { return Math.max(this.x, this.x + this.width); }
  toJSON(): Rect & { top: number; left: number; bottom: number; right: number } {
    const { x, y, width, height, top, left, bottom, right } = this;
    return { x, y, width, height, top, left, bottom, right };
  }
}

function wrapRect(rect: Rect): DOMRect {
  return new DOMRect(rect.x, rect.y, rect.width, rect.height);
}

function method(prototype: object, name: string, value: Function): void {
  Object.defineProperty(prototype, name, { value, configurable: true, writable: true });
}

/** Adds APIs that have a synchronous engine backing in the V8 realm. */
export function installDocumentBindings(global: Record<PropertyKey, unknown>, document: Document, bindings: DocumentBindings): void {
  documents.get(document)!.bindings = bindings;
  global.DOMRect = DOMRect;
  global.getComputedStyle = function getComputedStyle(element: Element, pseudo: string | null = null): CSSStyleDeclaration {
    if (!(element instanceof Element)) throw new TypeError("Expected an Element");
    const selector = pseudo === null || pseudo === "" ? null : String(pseudo);
    return computedStyle(property => nativeRead(element, 4, { property, pseudo: selector }) as string);
  };
  method(ParentNode.prototype, "querySelector", function querySelector(this: ParentNode, selector: string) {
    if (journalFor(this) === null) {
      const { result, nodes } = selectDetached(this, selector, "first");
      return lookupElement(this, result as number | null, nodes);
    }
    return lookupElement(this, select(this, selector, "first") as number | null);
  });
  method(ParentNode.prototype, "querySelectorAll", function querySelectorAll(this: ParentNode, selector: string) {
    if (journalFor(this) === null) {
      const { result, nodes } = selectDetached(this, selector, "all");
      const matches = (result as number[]).map(id => lookupElement(this, id, nodes)).filter((node): node is Element => node !== null);
      return new NodeList(() => matches);
    }
    const ids = select(this, selector, "all") as number[];
    const matches = ids.map(id => lookupElement(this, id)).filter((node): node is Element => node !== null);
    return new NodeList(() => matches);
  });
  method(Element.prototype, "matches", function matches(this: Element, selector: string) {
    return select(this, selector, "match");
  });
  method(Element.prototype, "closest", function closest(this: Element, selector: string) {
    if (journalFor(this) === null) {
      const { result, nodes } = selectDetached(this, selector, "closest");
      return lookupElement(this, result as number | null, nodes);
    }
    return lookupElement(this, select(this, selector, "closest") as number | null);
  });
  method(Element.prototype, "getBoundingClientRect", function getBoundingClientRect(this: Element) {
    return wrapRect(nativeRead(this, 1) as Rect);
  });
  method(Element.prototype, "getClientRects", function getClientRects(this: Element) {
    const rects = (nativeRead(this, 2) as Rect[]).map(wrapRect);
    return Object.assign(rects, { item: (index: number) => rects[index] ?? null });
  });
  method(Document.prototype, "elementFromPoint", function elementFromPoint(this: Document, x: number, y: number) {
    x = Number(x);
    y = Number(y);
    return Number.isFinite(x) && Number.isFinite(y) ? lookupElement(this, nativeRead(this, 6, { x, y }) as number | null) : null;
  });
  for (const [index, name] of [
    "offsetTop", "offsetLeft", "offsetWidth", "offsetHeight", "clientTop", "clientLeft",
    "clientWidth", "clientHeight", "scrollWidth", "scrollHeight",
  ].entries()) {
    Object.defineProperty(index < 4 ? HTMLElement.prototype : Element.prototype, name, {
      get(this: Element) { return readFromEngine(this, (bindings, id) => bindings.queryNumber(3, id, index + 1)); },
      configurable: true, enumerable: true,
    });
  }
  Object.defineProperty(HTMLElement.prototype, "offsetParent", {
    get(this: HTMLElement) {
      const { offsetParent } = nativeRead(this, 3) as { offsetParent: number | null };
      return lookupElement(this, offsetParent);
    },
    configurable: true, enumerable: true,
  });
}

/** Resolve the full wire ID against the live DOM rather than a recycled slot. */
export function connectedInputTarget(document: Document, id: number): Node | null {
  const journal = journalFor(document);
  if (journal === null) return null;
  const node = runtimeNodes.get(journal)?.index.get(id)?.ref.deref();
  return node?.isConnected && ownerOf(node) === document ? node : null;
}

export function onInputDetach(document: Document, handler: (node: Node) => void): void {
  documents.get(document)!.inputDetached = handler;
}

const scrollBits = new DataView(new ArrayBuffer(4));

function floatBits(value: number): number {
  scrollBits.setFloat32(0, value, true);
  return scrollBits.getUint32(0, true);
}

export function recordScroll(owner: Document | Element, x: number, y: number): void {
  journalFor(owner)?.record(Opcode.ScrollTo, nodeId(owner), floatBits(x), floatBits(y));
}

export function readScrollOffset(owner: Document | Element): { left: number; top: number } {
  return readFromEngine(owner, (bindings, id) => bindings.scrollOffset(id));
}
