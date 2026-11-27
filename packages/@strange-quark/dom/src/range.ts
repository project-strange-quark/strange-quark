// SPDX-License-Identifier: MIT
// DOM Standard ranges: AbstractRange, NodeRange, StaticRange and live Range.

import {
  CharacterData, childrenOf, Comment, defaultDocument, Document, DocumentFragment, DocumentType, Element, expectNode, Node,
  parseMarkup, Text,
} from "./document.js";
import { DOMException } from "./events.js";

type Points = { startNode: Node; startOffset: number; endNode: Node; endOffset: number; watch?: () => void };
type StaticRangeInit = { startContainer: Node; startOffset: number; endContainer: Node; endOffset: number };

const HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";
const constructing = Symbol("range constructor");

function fail(name: string, message: string): never { throw new DOMException(message, name); }

export function nodeLength(node: Node): number {
  if (node instanceof DocumentType) return 0;
  if (node instanceof CharacterData) return node.length;
  return childrenOf(node).length;
}

export function nodeRoot(node: Node): Node {
  let root = node;
  for (let parent = root.parentNode; parent !== null; parent = parent.parentNode) root = parent;
  return root;
}

export function nodeIndex(node: Node): number {
  const parent = node.parentNode;
  return parent === null ? 0 : childrenOf(parent).indexOf(node);
}

function isInclusiveAncestor(ancestor: Node, node: Node): boolean {
  for (let current: Node | null = node; current !== null; current = current.parentNode) {
    if (current === ancestor) return true;
  }
  return false;
}

function documentOf(node: Node): Document {
  return node instanceof Document ? node : node.ownerDocument!;
}

/** The position of (nodeA, offsetA) relative to (nodeB, offsetB) in one tree: -1 before, 0 equal, 1 after. */
export function comparePoints(nodeA: Node, offsetA: number, nodeB: Node, offsetB: number): number {
  if (nodeA === nodeB) return offsetA === offsetB ? 0 : offsetA < offsetB ? -1 : 1;
  const pathA: Node[] = [];
  for (let node: Node | null = nodeA; node !== null; node = node.parentNode) pathA.push(node);
  const pathB: Node[] = [];
  for (let node: Node | null = nodeB; node !== null; node = node.parentNode) pathB.push(node);
  let a = pathA.length - 1;
  let b = pathB.length - 1;
  while (a > 0 && b > 0 && pathA[a - 1] === pathB[b - 1]) { a--; b--; }
  if (a === 0) return childrenOf(nodeA).indexOf(pathB[b - 1]) < offsetA ? 1 : -1;
  if (b === 0) return childrenOf(nodeB).indexOf(pathA[a - 1]) < offsetB ? -1 : 1;
  const siblings = childrenOf(pathA[a]);
  return siblings.indexOf(pathA[a - 1]) < siblings.indexOf(pathB[b - 1]) ? -1 : 1;
}

let pointsOf: (range: AbstractRange) => Points;

export abstract class AbstractRange {
  #points: Points;

  protected constructor(token: symbol, points: Points) {
    if (token !== constructing) throw new TypeError("Illegal constructor");
    this.#points = points;
  }

  static {
    pointsOf = (range) => range.#points;
  }

  get startOffset(): number { return this.#points.startOffset; }
  get endOffset(): number { return this.#points.endOffset; }
  get collapsed(): boolean {
    const points = this.#points;
    return points.startNode === points.endNode && points.startOffset === points.endOffset;
  }
}

export abstract class NodeRange extends AbstractRange {
  get startContainer(): Node { return pointsOf(this).startNode; }
  get endContainer(): Node { return pointsOf(this).endNode; }
}

function requiredMember(init: Record<string, unknown>, name: string): unknown {
  const value = init[name];
  if (value === undefined) throw new TypeError(`StaticRangeInit requires ${name}`);
  return value;
}

export class StaticRange extends NodeRange {
  constructor(init: StaticRangeInit) {
    if (arguments.length === 0) throw new TypeError("StaticRange requires an init dictionary");
    if (init !== null && init !== undefined && typeof init !== "object" && typeof init !== "function") {
      throw new TypeError("StaticRange init must be a dictionary");
    }
    const members = (init ?? {}) as unknown as Record<string, unknown>;
    // Dictionary members convert in lexicographic order.
    const endContainer = requiredMember(members, "endContainer");
    expectNode(endContainer);
    const endOffset = (requiredMember(members, "endOffset") as number) >>> 0;
    const startContainer = requiredMember(members, "startContainer");
    expectNode(startContainer);
    const startOffset = (requiredMember(members, "startOffset") as number) >>> 0;
    if (startContainer instanceof DocumentType || endContainer instanceof DocumentType) {
      fail("InvalidNodeTypeError", "A StaticRange cannot have a doctype boundary");
    }
    super(constructing, { startNode: startContainer, startOffset, endNode: endContainer, endOffset });
  }
}

// Live ranges are tracked weakly so short-lived ranges leave the set once collected.
const liveRanges = new Set<WeakRef<Points>>();
const releasedRanges = new FinalizationRegistry<WeakRef<Points>>((ref) => { liveRanges.delete(ref); });
let nextDocument: Document | null = null;

/** The number of live ranges still tracked, for tests. Internal to squark-dom. */
export function liveRangeCount(): number { return liveRanges.size; }

/** Calls `watch` after a Range method changes the range's boundary points; DOM mutations do not call it. */
export function watchRange(range: Range, watch: (() => void) | undefined): void {
  pointsOf(range).watch = watch;
}

export function createRange(document: Document): Range {
  nextDocument = document;
  return new Range();
}

/** A live range in `document` with the given boundary points, which the caller has validated. */
export function newRange(document: Document, startNode: Node, startOffset: number, endNode: Node, endOffset: number): Range {
  const range = createRange(document);
  Object.assign(pointsOf(range), { startNode, startOffset, endNode, endOffset });
  return range;
}

function update(range: AbstractRange, change: (points: Points) => void): void {
  const points = pointsOf(range);
  const watch = points.watch;
  if (watch === undefined) {
    change(points);
    return;
  }
  const { startNode, startOffset, endNode, endOffset } = points;
  change(points);
  if (points.startNode !== startNode || points.startOffset !== startOffset ||
      points.endNode !== endNode || points.endOffset !== endOffset) watch();
}

function setBoundary(range: Range, node: Node, offset: number, start: boolean): void {
  if (node instanceof DocumentType) fail("InvalidNodeTypeError", "A range boundary cannot be a doctype");
  if (offset > nodeLength(node)) fail("IndexSizeError", "The offset exceeds the node's length");
  update(range, (points) => {
    if (start) {
      if (nodeRoot(points.startNode) !== nodeRoot(node) || comparePoints(node, offset, points.endNode, points.endOffset) > 0) {
        points.endNode = node;
        points.endOffset = offset;
      }
      points.startNode = node;
      points.startOffset = offset;
    } else {
      if (nodeRoot(points.startNode) !== nodeRoot(node) || comparePoints(node, offset, points.startNode, points.startOffset) < 0) {
        points.startNode = node;
        points.startOffset = offset;
      }
      points.endNode = node;
      points.endOffset = offset;
    }
  });
}

function setBoth(range: AbstractRange, startNode: Node, startOffset: number, endNode: Node, endOffset: number): void {
  update(range, (points) => {
    points.startNode = startNode;
    points.startOffset = startOffset;
    points.endNode = endNode;
    points.endOffset = endOffset;
  });
}

function parentFor(node: Node): Node {
  const parent = node.parentNode;
  if (parent === null) fail("InvalidNodeTypeError", "The node has no parent");
  return parent;
}

function checkPoint(node: Node, offset: number): void {
  if (node instanceof DocumentType) fail("InvalidNodeTypeError", "A boundary point cannot be in a doctype");
  if (offset > nodeLength(node)) fail("IndexSizeError", "The offset exceeds the node's length");
}

function nextSkippingChildren(node: Node): Node | null {
  for (let current: Node | null = node; current !== null; current = current.parentNode) {
    const next = current.nextSibling;
    if (next !== null) return next;
  }
  return null;
}

function firstNode(points: Points): Node | null {
  const { startNode, startOffset } = points;
  if (startNode instanceof CharacterData) return startNode;
  return childrenOf(startNode)[startOffset] ?? nextSkippingChildren(startNode);
}

function pastLastNode(points: Points): Node | null {
  const { endNode, endOffset } = points;
  if (endNode instanceof CharacterData) return nextSkippingChildren(endNode);
  return childrenOf(endNode)[endOffset] ?? nextSkippingChildren(endNode);
}

/** Every node from the range's first node up to its past-last node, in tree order. */
function* rangeNodes(points: Points): Generator<Node> {
  const first = firstNode(points);
  const stop = pastLastNode(points);
  if (first === null || first === stop) return;
  // Sibling cursors from the root down avoid an indexOf for every step.
  const frames: { siblings: readonly Node[]; index: number }[] = [];
  for (let child: Node = first, parent = child.parentNode; parent !== null;) {
    const siblings = childrenOf(parent);
    frames.unshift({ siblings, index: siblings.indexOf(child) });
    child = parent;
    parent = child.parentNode;
  }
  let node: Node | undefined = first;
  while (node !== undefined && node !== stop) {
    yield node;
    const children = childrenOf(node);
    if (children.length !== 0) {
      frames.push({ siblings: children, index: 0 });
      node = children[0];
      continue;
    }
    node = undefined;
    while (frames.length !== 0) {
      const frame = frames[frames.length - 1];
      if (++frame.index < frame.siblings.length) {
        node = frame.siblings[frame.index];
        break;
      }
      frames.pop();
    }
  }
}

function commonAncestor(startNode: Node, endNode: Node): Node {
  const ancestors = new Set<Node>();
  for (let node: Node | null = endNode; node !== null; node = node.parentNode) ancestors.add(node);
  let container = startNode;
  while (!ancestors.has(container)) container = container.parentNode!;
  return container;
}

/** The common ancestor's partially contained children and the children between them. */
function partition(startNode: Node, startOffset: number, endNode: Node, endOffset: number) {
  const common = commonAncestor(startNode, endNode);
  const childOf = (node: Node): Node => {
    let child = node;
    while (child.parentNode !== common) child = child.parentNode!;
    return child;
  };
  const firstPartial = common === startNode ? null : childOf(startNode);
  const lastPartial = common === endNode ? null : childOf(endNode);
  const children = childrenOf(common);
  const from = firstPartial === null ? startOffset : children.indexOf(firstPartial) + 1;
  const to = lastPartial === null ? endOffset : children.indexOf(lastPartial);
  const contained = children.slice(from, Math.max(from, to));
  if (contained.some((child) => child instanceof DocumentType)) {
    fail("HierarchyRequestError", "A range containing a doctype cannot be extracted or cloned");
  }
  return { firstPartial, lastPartial, contained };
}

/** Where a range's boundary points collapse after its contents leave the tree. */
function collapsePoint(startNode: Node, startOffset: number, endNode: Node): [Node, number] {
  if (isInclusiveAncestor(startNode, endNode)) return [startNode, startOffset];
  let reference = startNode;
  while (reference.parentNode !== null && !isInclusiveAncestor(reference.parentNode, endNode)) reference = reference.parentNode;
  return [reference.parentNode!, nodeIndex(reference) + 1];
}

function textClone(node: CharacterData, offset: number, count: number): CharacterData {
  const clone = node.cloneNode(false) as CharacterData;
  clone.data = node.substringData(offset, count);
  return clone;
}

function extract(range: Range | null, startNode: Node, startOffset: number, endNode: Node, endOffset: number): DocumentFragment {
  const fragment = documentOf(startNode).createDocumentFragment();
  if (startNode === endNode && startOffset === endOffset) return fragment;
  if (startNode === endNode && startNode instanceof CharacterData) {
    fragment.appendChild(textClone(startNode, startOffset, endOffset - startOffset));
    startNode.replaceData(startOffset, endOffset - startOffset, "");
    return fragment;
  }
  const { firstPartial, lastPartial, contained } = partition(startNode, startOffset, endNode, endOffset);
  const [newNode, newOffset] = collapsePoint(startNode, startOffset, endNode);
  if (firstPartial instanceof CharacterData) {
    const node = startNode as CharacterData;
    fragment.appendChild(textClone(node, startOffset, node.length - startOffset));
    node.replaceData(startOffset, node.length - startOffset, "");
  } else if (firstPartial !== null) {
    const clone = fragment.appendChild(firstPartial.cloneNode(false));
    clone.appendChild(extract(null, startNode, startOffset, firstPartial, nodeLength(firstPartial)));
  }
  for (const child of contained) fragment.appendChild(child);
  if (lastPartial instanceof CharacterData) {
    const node = endNode as CharacterData;
    fragment.appendChild(textClone(node, 0, endOffset));
    node.replaceData(0, endOffset, "");
  } else if (lastPartial !== null) {
    const clone = fragment.appendChild(lastPartial.cloneNode(false));
    clone.appendChild(extract(null, lastPartial, 0, endNode, endOffset));
  }
  if (range !== null) setBoth(range, newNode, newOffset, newNode, newOffset);
  return fragment;
}

function cloneContents(startNode: Node, startOffset: number, endNode: Node, endOffset: number): DocumentFragment {
  const fragment = documentOf(startNode).createDocumentFragment();
  if (startNode === endNode && startOffset === endOffset) return fragment;
  if (startNode === endNode && startNode instanceof CharacterData) {
    fragment.appendChild(textClone(startNode, startOffset, endOffset - startOffset));
    return fragment;
  }
  const { firstPartial, lastPartial, contained } = partition(startNode, startOffset, endNode, endOffset);
  if (firstPartial instanceof CharacterData) {
    const node = startNode as CharacterData;
    fragment.appendChild(textClone(node, startOffset, node.length - startOffset));
  } else if (firstPartial !== null) {
    const clone = fragment.appendChild(firstPartial.cloneNode(false));
    clone.appendChild(cloneContents(startNode, startOffset, firstPartial, nodeLength(firstPartial)));
  }
  for (const child of contained) fragment.appendChild(child.cloneNode(true));
  if (lastPartial instanceof CharacterData) {
    fragment.appendChild(textClone(endNode as CharacterData, 0, endOffset));
  } else if (lastPartial !== null) {
    const clone = fragment.appendChild(lastPartial.cloneNode(false));
    clone.appendChild(cloneContents(lastPartial, 0, endNode, endOffset));
  }
  return fragment;
}

/** Contained nodes whose parent is not contained, in tree order. */
function topContained(points: Points): Node[] {
  const { startNode, startOffset, endNode, endOffset } = points;
  const common = commonAncestor(startNode, endNode);
  const nodes: Node[] = [];
  let from = startOffset;
  if (startNode !== common) {
    nodes.push(...childrenOf(startNode).slice(startOffset));
    let node = startNode;
    for (; node.parentNode !== common; node = node.parentNode!) {
      const siblings = childrenOf(node.parentNode!);
      nodes.push(...siblings.slice(siblings.indexOf(node) + 1));
    }
    from = nodeIndex(node) + 1;
  }
  let to = endOffset;
  const tail: Node[][] = [];
  if (endNode !== common) {
    tail.push(childrenOf(endNode).slice(0, endOffset));
    let node = endNode;
    for (; node.parentNode !== common; node = node.parentNode!) {
      const siblings = childrenOf(node.parentNode!);
      tail.push(siblings.slice(0, siblings.indexOf(node)));
    }
    to = nodeIndex(node);
  }
  nodes.push(...childrenOf(common).slice(from, Math.max(from, to)));
  for (let level = tail.length - 1; level >= 0; level--) nodes.push(...tail[level]);
  return nodes;
}

function ensurePreInsertValidity(node: Node, parent: Node, child: Node | null): void {
  if (!(parent instanceof Document || parent instanceof DocumentFragment || parent instanceof Element)) {
    fail("HierarchyRequestError", "The parent cannot have children");
  }
  if (isInclusiveAncestor(node, parent)) fail("HierarchyRequestError", "The node is an inclusive ancestor of the parent");
  if (child !== null && child.parentNode !== parent) fail("NotFoundError", "The reference node is not a child of the parent");
  if (!(node instanceof DocumentFragment || node instanceof DocumentType || node instanceof Element || node instanceof CharacterData)) {
    fail("HierarchyRequestError", "The node cannot be inserted");
  }
  if (node instanceof Text && parent instanceof Document || node instanceof DocumentType && !(parent instanceof Document)) {
    fail("HierarchyRequestError", "The node cannot be inserted into this parent");
  }
  if (!(parent instanceof Document)) return;
  const children = childrenOf(parent);
  const at = child === null ? children.length : children.indexOf(child);
  const hasElement = children.some((item) => item instanceof Element);
  const doctypeAfter = child instanceof DocumentType || children.slice(at).some((item) => item instanceof DocumentType);
  let invalid = false;
  if (node instanceof DocumentFragment) {
    const nodes = childrenOf(node);
    const elements = nodes.filter((item) => item instanceof Element).length;
    invalid = elements > 1 || nodes.some((item) => item instanceof Text) || elements === 1 && (hasElement || doctypeAfter);
  } else if (node instanceof Element) {
    invalid = hasElement || doctypeAfter;
  } else if (node instanceof DocumentType) {
    invalid = children.some((item) => item instanceof DocumentType) || children.slice(0, at).some((item) => item instanceof Element);
  }
  if (invalid) fail("HierarchyRequestError", "The document cannot have this child");
}

function textOf(points: Points): string {
  const { startNode, startOffset, endNode, endOffset } = points;
  if (startNode === endNode && startNode instanceof Text) return startNode.data.slice(startOffset, endOffset);
  let text = startNode instanceof Text ? startNode.data.slice(startOffset) : "";
  for (const node of rangeNodes(points)) {
    if (node instanceof Text && node !== startNode && node !== endNode) text += node.data;
  }
  if (endNode instanceof Text) text += endNode.data.slice(0, endOffset);
  return text;
}

export class Range extends NodeRange {
  static readonly START_TO_START = 0;
  static readonly START_TO_END = 1;
  static readonly END_TO_END = 2;
  static readonly END_TO_START = 3;

  constructor() {
    const document = nextDocument ?? defaultDocument();
    nextDocument = null;
    const points: Points = { startNode: document, startOffset: 0, endNode: document, endOffset: 0 };
    super(constructing, points);
    const ref = new WeakRef(points);
    liveRanges.add(ref);
    releasedRanges.register(this, ref);
  }

  get commonAncestorContainer(): Node {
    const { startNode, endNode } = pointsOf(this);
    return commonAncestor(startNode, endNode);
  }

  setStart(node: Node, offset: number): void {
    if (arguments.length < 2) throw new TypeError("setStart requires a node and an offset");
    expectNode(node);
    setBoundary(this, node, offset >>> 0, true);
  }

  setEnd(node: Node, offset: number): void {
    if (arguments.length < 2) throw new TypeError("setEnd requires a node and an offset");
    expectNode(node);
    setBoundary(this, node, offset >>> 0, false);
  }

  setStartBefore(node: Node): void {
    expectNode(node);
    setBoundary(this, parentFor(node), nodeIndex(node), true);
  }

  setStartAfter(node: Node): void {
    expectNode(node);
    setBoundary(this, parentFor(node), nodeIndex(node) + 1, true);
  }

  setEndBefore(node: Node): void {
    expectNode(node);
    setBoundary(this, parentFor(node), nodeIndex(node), false);
  }

  setEndAfter(node: Node): void {
    expectNode(node);
    setBoundary(this, parentFor(node), nodeIndex(node) + 1, false);
  }

  collapse(toStart = false): void {
    const { startNode, startOffset, endNode, endOffset } = pointsOf(this);
    if (toStart) setBoth(this, startNode, startOffset, startNode, startOffset);
    else setBoth(this, endNode, endOffset, endNode, endOffset);
  }

  selectNode(node: Node): void {
    expectNode(node);
    const parent = parentFor(node);
    const index = nodeIndex(node);
    setBoth(this, parent, index, parent, index + 1);
  }

  selectNodeContents(node: Node): void {
    expectNode(node);
    if (node instanceof DocumentType) fail("InvalidNodeTypeError", "Cannot select the contents of a doctype");
    setBoth(this, node, 0, node, nodeLength(node));
  }

  compareBoundaryPoints(how: number, sourceRange: Range): number {
    if (arguments.length < 2) throw new TypeError("compareBoundaryPoints requires a comparison and a range");
    const kind = (how >>> 0) & 0xffff;
    if (!(sourceRange instanceof Range)) throw new TypeError("Expected a Range");
    if (kind > 3) fail("NotSupportedError", "Unknown boundary point comparison");
    const own = pointsOf(this);
    const source = pointsOf(sourceRange);
    if (nodeRoot(own.startNode) !== nodeRoot(source.startNode)) fail("WrongDocumentError", "The ranges are in different trees");
    const ownStart = kind === 0 || kind === 3;
    const sourceStart = kind === 0 || kind === 1;
    return comparePoints(
      ownStart ? own.startNode : own.endNode, ownStart ? own.startOffset : own.endOffset,
      sourceStart ? source.startNode : source.endNode, sourceStart ? source.startOffset : source.endOffset,
    );
  }

  deleteContents(): void {
    const points = pointsOf(this);
    if (this.collapsed) return;
    const { startNode, startOffset, endNode, endOffset } = points;
    if (startNode === endNode && startNode instanceof CharacterData) {
      startNode.replaceData(startOffset, endOffset - startOffset, "");
      return;
    }
    const removed = topContained(points);
    const [newNode, newOffset] = collapsePoint(startNode, startOffset, endNode);
    if (startNode instanceof CharacterData) startNode.replaceData(startOffset, startNode.length - startOffset, "");
    for (const node of removed) node.parentNode?.removeChild(node);
    if (endNode instanceof CharacterData) endNode.replaceData(0, endOffset, "");
    setBoth(this, newNode, newOffset, newNode, newOffset);
  }

  extractContents(): DocumentFragment {
    const { startNode, startOffset, endNode, endOffset } = pointsOf(this);
    return extract(this, startNode, startOffset, endNode, endOffset);
  }

  cloneContents(): DocumentFragment {
    const { startNode, startOffset, endNode, endOffset } = pointsOf(this);
    return cloneContents(startNode, startOffset, endNode, endOffset);
  }

  insertNode(node: Node): void {
    if (arguments.length < 1) throw new TypeError("insertNode requires a node");
    expectNode(node);
    const points = pointsOf(this);
    const start = points.startNode;
    if (start instanceof Comment || start instanceof Text && start.parentNode === null || start === node) {
      fail("HierarchyRequestError", "Cannot insert a node at this range's start");
    }
    let reference: Node | null = start instanceof Text ? start : childrenOf(start)[points.startOffset] ?? null;
    const parent = reference === null ? start : reference.parentNode!;
    ensurePreInsertValidity(node, parent, reference);
    if (start instanceof Text) reference = start.splitText(points.startOffset);
    if (node === reference) reference = reference.nextSibling;
    node.parentNode?.removeChild(node);
    let newOffset = reference === null ? nodeLength(parent) : nodeIndex(reference);
    newOffset += node instanceof DocumentFragment ? nodeLength(node) : 1;
    parent.insertBefore(node, reference);
    if (this.collapsed) update(this, (current) => { current.endNode = parent; current.endOffset = newOffset; });
  }

  surroundContents(newParent: Node): void {
    if (arguments.length < 1) throw new TypeError("surroundContents requires a node");
    expectNode(newParent);
    const { startNode, endNode } = pointsOf(this);
    for (let node = startNode; !isInclusiveAncestor(node, endNode); node = node.parentNode!) {
      if (!(node instanceof Text)) fail("InvalidStateError", "The range partially contains a non-Text node");
    }
    for (let node = endNode; !isInclusiveAncestor(node, startNode); node = node.parentNode!) {
      if (!(node instanceof Text)) fail("InvalidStateError", "The range partially contains a non-Text node");
    }
    if (newParent instanceof Document || newParent instanceof DocumentType || newParent instanceof DocumentFragment) {
      fail("InvalidNodeTypeError", "The new parent cannot surround a range");
    }
    const fragment = this.extractContents();
    if (newParent instanceof Element && newParent.hasChildNodes()) newParent.replaceChildren();
    this.insertNode(newParent);
    newParent.appendChild(fragment);
    this.selectNode(newParent);
  }

  cloneRange(): Range {
    const { startNode, startOffset, endNode, endOffset } = pointsOf(this);
    return newRange(documentOf(startNode), startNode, startOffset, endNode, endOffset);
  }

  detach(): void {}

  isPointInRange(node: Node, offset: number): boolean {
    if (arguments.length < 2) throw new TypeError("isPointInRange requires a node and an offset");
    expectNode(node);
    offset >>>= 0;
    const points = pointsOf(this);
    if (nodeRoot(node) !== nodeRoot(points.startNode)) return false;
    checkPoint(node, offset);
    return comparePoints(node, offset, points.startNode, points.startOffset) >= 0 &&
      comparePoints(node, offset, points.endNode, points.endOffset) <= 0;
  }

  comparePoint(node: Node, offset: number): number {
    if (arguments.length < 2) throw new TypeError("comparePoint requires a node and an offset");
    expectNode(node);
    offset >>>= 0;
    const points = pointsOf(this);
    if (nodeRoot(node) !== nodeRoot(points.startNode)) fail("WrongDocumentError", "The point is in a different tree");
    checkPoint(node, offset);
    if (comparePoints(node, offset, points.startNode, points.startOffset) < 0) return -1;
    if (comparePoints(node, offset, points.endNode, points.endOffset) > 0) return 1;
    return 0;
  }

  intersectsNode(node: Node): boolean {
    if (arguments.length < 1) throw new TypeError("intersectsNode requires a node");
    expectNode(node);
    const points = pointsOf(this);
    if (nodeRoot(node) !== nodeRoot(points.startNode)) return false;
    const parent = node.parentNode;
    if (parent === null) return true;
    const offset = nodeIndex(node);
    return comparePoints(parent, offset, points.endNode, points.endOffset) < 0 &&
      comparePoints(parent, offset + 1, points.startNode, points.startOffset) > 0;
  }

  createContextualFragment(fragment: string): DocumentFragment {
    if (arguments.length < 1) throw new TypeError("createContextualFragment requires markup");
    const markup = String(fragment);
    const start = pointsOf(this).startNode;
    const document = documentOf(start);
    let element = start instanceof Element ? start : start instanceof Text || start instanceof Comment ? start.parentElement : null;
    if (element === null || document.contentType === "text/html" && element.localName === "html" && element.namespaceURI === HTML_NAMESPACE) {
      element = document.createElementNS(HTML_NAMESPACE, "body");
    }
    const result = document.createDocumentFragment();
    const nodes = parseMarkup(element, markup);
    if (nodes.length !== 0) result.append(...nodes);
    return result;
  }

  toString(): string { return textOf(pointsOf(this)); }
}

for (const name of ["START_TO_START", "START_TO_END", "END_TO_END", "END_TO_START"] as const) {
  Object.defineProperty(Range.prototype, name, { value: Range[name], enumerable: true });
}

function eachLiveRange(visit: (points: Points) => void): void {
  for (const ref of liveRanges) {
    const points = ref.deref();
    if (points === undefined) liveRanges.delete(ref);
    else visit(points);
  }
}

/** The live range pre-remove steps, run before `node` leaves `parent` at `index`. */
export function rangesBeforeRemove(node: Node, parent: Node, index: number): void {
  if (liveRanges.size === 0) return;
  eachLiveRange((points) => {
    if (points.startNode === parent) {
      if (points.startOffset > index) points.startOffset--;
    } else if (isInclusiveAncestor(node, points.startNode)) {
      points.startNode = parent;
      points.startOffset = index;
    }
    if (points.endNode === parent) {
      if (points.endOffset > index) points.endOffset--;
    } else if (isInclusiveAncestor(node, points.endNode)) {
      points.endNode = parent;
      points.endOffset = index;
    }
  });
}

/** The live range insertion steps for one node inserted into `parent` at `index`, before an existing child. */
export function rangesAfterInsert(parent: Node, index: number): void {
  if (liveRanges.size === 0) return;
  eachLiveRange((points) => {
    if (points.startNode === parent && points.startOffset > index) points.startOffset++;
    if (points.endNode === parent && points.endOffset > index) points.endOffset++;
  });
}

/** The live range steps of replace data: `count` code units at `offset` became `length` code units. */
export function rangesReplaceData(node: Node, offset: number, count: number, length: number): void {
  if (liveRanges.size === 0) return;
  eachLiveRange((points) => {
    if (points.startNode === node) {
      if (points.startOffset > offset && points.startOffset <= offset + count) points.startOffset = offset;
      else if (points.startOffset > offset + count) points.startOffset += length - count;
    }
    if (points.endNode === node) {
      if (points.endOffset > offset && points.endOffset <= offset + count) points.endOffset = offset;
      else if (points.endOffset > offset + count) points.endOffset += length - count;
    }
  });
}

/** The live range steps of splitting `node` at `offset` into `split`, after `split` was inserted. */
export function rangesSplitText(node: Text, split: Text, offset: number, parent: Node, index: number): void {
  if (liveRanges.size === 0) return;
  eachLiveRange((points) => {
    if (points.startNode === node && points.startOffset > offset) {
      points.startNode = split;
      points.startOffset -= offset;
    }
    if (points.endNode === node && points.endOffset > offset) {
      points.endNode = split;
      points.endOffset -= offset;
    }
    if (points.startNode === parent && points.startOffset === index + 1) points.startOffset++;
    if (points.endNode === parent && points.endOffset === index + 1) points.endOffset++;
  });
}

/** normalize() moving boundary points from `merged`, at `index` in `parent`, to `length` in `node`. */
export function rangesMergeText(node: Text, merged: Text, length: number, parent: Node, index: number): void {
  if (liveRanges.size === 0) return;
  eachLiveRange((points) => {
    if (points.startNode === merged) {
      points.startNode = node;
      points.startOffset += length;
    }
    if (points.endNode === merged) {
      points.endNode = node;
      points.endOffset += length;
    }
    if (points.startNode === parent && points.startOffset === index) {
      points.startNode = node;
      points.startOffset = length;
    }
    if (points.endNode === parent && points.endOffset === index) {
      points.endNode = node;
      points.endOffset = length;
    }
  });
}

