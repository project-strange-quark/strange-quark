// SPDX-License-Identifier: MIT
// The Selection API: one selection per document, holding a live Range, and
// the selectionchange events for the document and for text controls.

import { childrenOf, DocumentType, expectNode, focusedElement, HTMLInputElement, queueDocumentTask, Text } from "./document.js";
import type { Document, Element, HTMLTextAreaElement, Node } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event } from "./events.js";
import { comparePoints, newRange, nodeIndex, nodeLength, nodeRoot, Range, StaticRange, watchRange } from "./range.js";

type TextControl = HTMLInputElement | HTMLTextAreaElement;
type ControlSelection = { start: number; end: number; direction: "forward" | "backward" };
type State = {
  document: Document;
  object?: Selection;
  range: Range | null;
  backward: boolean;
  // Chromium keeps a text control's selection inside the control; the
  // selection then shows a caret before the control in its parent.
  control: TextControl | null;
  inControl: ControlSelection;
  scheduled: boolean;
};

const states = new WeakMap<Document, State>();
const scheduledControls = new WeakSet<Element>();
const token = Symbol("Selection constructor");

function fail(name: string, message: string): never { throw new DOMException(message, name); }

function stateOf(document: Document): State {
  let state = states.get(document);
  if (state === undefined) {
    state = { document, range: null, backward: false, control: null, inControl: { start: 0, end: 0, direction: "forward" }, scheduled: false };
    states.set(document, state);
  }
  return state;
}

/** The document's selection object. Internal to squark-dom; Document.getSelection() checks the browsing context. */
export function documentSelection(document: Document): Selection {
  const state = stateOf(document);
  return state.object ??= new Selection(token, state);
}

/** The selection's range while it is in the document tree, after dropping a text control that moved away. */
function current(state: State): Range | null {
  const { range, control } = state;
  if (range === null || nodeRoot(range.startContainer) !== state.document) return null;
  if (control !== null) {
    const parent = control.parentNode;
    if (parent === null || nodeRoot(parent) !== state.document || range.startContainer !== parent ||
        range.endContainer !== parent || range.startOffset !== nodeIndex(control) || !range.collapsed) state.control = null;
  }
  return range;
}

function snapshot(state: State): readonly unknown[] {
  const range = current(state);
  if (range === null) return [];
  return [state.control, state.backward, range.startContainer, range.startOffset, range.endContainer, range.endOffset];
}

function scheduleDocument(state: State): void {
  if (state.scheduled) return;
  state.scheduled = true;
  queueDocumentTask(state.document, () => {
    state.scheduled = false;
    dispatchTrustedEvent(state.document, new Event("selectionchange"));
  });
}

function scheduleControl(control: TextControl): void {
  if (scheduledControls.has(control)) return;
  scheduledControls.add(control);
  queueDocumentTask(control.ownerDocument, () => {
    scheduledControls.delete(control);
    dispatchTrustedEvent(control, new Event("selectionchange", { bubbles: true }));
  });
}

function associate(state: State, range: Range | null, backward: boolean, control: TextControl | null = null): void {
  if (state.range !== null) watchRange(state.range, undefined);
  state.range = range;
  state.backward = backward;
  state.control = control;
  if (range !== null) watchRange(range, () => rangeChanged(state, range));
}

/** A script moved the selection's range with a Range method. */
function rangeChanged(state: State, range: Range): void {
  if (state.range !== range) return;
  // Chromium reselects the range from its start to its end.
  state.control = null;
  state.backward = false;
  // Chromium drops a selection whose range leaves the document.
  if (nodeRoot(range.startContainer) !== state.document) associate(state, null, false);
  scheduleDocument(state);
}

/** Sets the selection's range, firing selectionchange when the selection actually changed. */
function select(state: State, range: Range | null, backward = false): void {
  const before = snapshot(state);
  associate(state, range, backward);
  const after = snapshot(state);
  if (before.length !== after.length || before.some((value, index) => value !== after[index])) scheduleDocument(state);
}

function collapsedAt(state: State, node: Node, offset: number): Range {
  return newRange(state.document, node, offset, node, offset);
}

function checkPoint(node: Node, offset: number): void {
  if (node instanceof DocumentType) fail("InvalidNodeTypeError", "A selection boundary cannot be a doctype");
  if (offset > nodeLength(node)) fail("IndexSizeError", "The offset exceeds the node's length");
}

function controlText(control: TextControl, { start, end }: ControlSelection): string {
  const length = control.value.length;
  const from = Math.min(start, length);
  const to = Math.min(end, length);
  if (control instanceof HTMLInputElement && control.type === "password") return "•".repeat(to - from);
  return control.value.slice(from, to);
}

function enterControl(state: State, control: TextControl, selection: ControlSelection): void {
  const parent = control.parentNode;
  if (parent === null) return;
  associate(state, collapsedAt(state, parent, nodeIndex(control)), false, control);
  state.inControl = { ...selection };
}

/** Focus moved into a text control, so the document selection moves into it too. */
export function textControlFocused(control: TextControl, selection: ControlSelection): void {
  const document = control.ownerDocument;
  if (document.defaultView === null) return;
  enterControl(stateOf(document), control, selection);
  scheduleControl(control);
}

/** A text control's selection changed; a focused control also carries the document selection. */
export function textControlSelectionChanged(control: TextControl, selection: ControlSelection): void {
  scheduleControl(control);
  const document = control.ownerDocument;
  if (document.defaultView === null || focusedElement(document) !== control) return;
  enterControl(stateOf(document), control, selection);
}

export class Selection {
  #state: State;

  constructor(key: symbol, state: State) {
    if (key !== token) throw new TypeError("Illegal constructor");
    this.#state = state;
  }

  #anchor(): [Node, number] | [null, 0] {
    const range = current(this.#state);
    if (range === null) return [null, 0];
    return this.#state.backward ? [range.endContainer, range.endOffset] : [range.startContainer, range.startOffset];
  }

  #focus(): [Node, number] | [null, 0] {
    const range = current(this.#state);
    if (range === null) return [null, 0];
    return this.#state.backward ? [range.startContainer, range.startOffset] : [range.endContainer, range.endOffset];
  }

  get anchorNode(): Node | null { return this.#anchor()[0]; }
  get anchorOffset(): number { return this.#anchor()[1]; }
  get focusNode(): Node | null { return this.#focus()[0]; }
  get focusOffset(): number { return this.#focus()[1]; }
  get baseNode(): Node | null { return this.anchorNode; }
  get baseOffset(): number { return this.anchorOffset; }
  get extentNode(): Node | null { return this.focusNode; }
  get extentOffset(): number { return this.focusOffset; }

  get isCollapsed(): boolean {
    const range = current(this.#state);
    return range === null || range.collapsed;
  }

  get rangeCount(): number { return current(this.#state) === null ? 0 : 1; }

  get type(): string {
    const state = this.#state;
    const range = current(state);
    if (range === null) return "None";
    if (state.control !== null) return state.inControl.start === state.inControl.end ? "Caret" : "Range";
    return range.collapsed ? "Caret" : "Range";
  }

  get direction(): string {
    const state = this.#state;
    const range = current(state);
    if (range === null) return "none";
    if (state.control !== null) return state.inControl.start === state.inControl.end ? "none" : state.inControl.direction;
    // Chromium reports a collapsed selection as directionless.
    if (range.collapsed) return "none";
    return state.backward ? "backward" : "forward";
  }

  getRangeAt(index: number): Range {
    if (arguments.length < 1) throw new TypeError("getRangeAt requires an index");
    const range = current(this.#state);
    if (index >>> 0 !== 0 || range === null) fail("IndexSizeError", "The selection has no range at this index");
    return range;
  }

  addRange(range: Range): void {
    if (arguments.length < 1) throw new TypeError("addRange requires a range");
    if (!(range instanceof Range)) throw new TypeError("Expected a Range");
    const state = this.#state;
    if (nodeRoot(range.startContainer) !== state.document || current(state) !== null) return;
    select(state, range);
  }

  removeRange(range: Range): void {
    if (arguments.length < 1) throw new TypeError("removeRange requires a range");
    if (!(range instanceof Range)) throw new TypeError("Expected a Range");
    const state = this.#state;
    if (state.range !== range) fail("NotFoundError", "The range is not the selection's range");
    select(state, null);
  }

  removeAllRanges(): void {
    if (this.#state.range !== null) select(this.#state, null);
  }

  empty(): void { this.removeAllRanges(); }

  collapse(node: Node | null, offset = 0): void {
    if (arguments.length < 1) throw new TypeError("collapse requires a node");
    this.#collapse(node, offset);
  }

  setPosition(node: Node | null, offset = 0): void {
    if (arguments.length < 1) throw new TypeError("setPosition requires a node");
    this.#collapse(node, offset);
  }

  #collapse(node: Node | null, offset: number): void {
    if (node === null || node === undefined) {
      this.removeAllRanges();
      return;
    }
    expectNode(node);
    offset >>>= 0;
    checkPoint(node, offset);
    const state = this.#state;
    if (nodeRoot(node) !== state.document) return;
    select(state, collapsedAt(state, node, offset));
  }

  collapseToStart(): void {
    const state = this.#state;
    const range = current(state);
    if (range === null) fail("InvalidStateError", "The selection is empty");
    select(state, collapsedAt(state, range.startContainer, range.startOffset));
  }

  collapseToEnd(): void {
    const state = this.#state;
    const range = current(state);
    if (range === null) fail("InvalidStateError", "The selection is empty");
    select(state, collapsedAt(state, range.endContainer, range.endOffset));
  }

  extend(node: Node, offset = 0): void {
    if (arguments.length < 1) throw new TypeError("extend requires a node");
    expectNode(node);
    offset >>>= 0;
    const state = this.#state;
    if (nodeRoot(node) !== state.document) return;
    if (current(state) === null) fail("InvalidStateError", "The selection is empty");
    checkPoint(node, offset);
    const [anchorNode, anchorOffset] = this.#anchor() as [Node, number];
    const backward = comparePoints(node, offset, anchorNode, anchorOffset) < 0;
    select(state, backward ? newRange(state.document, node, offset, anchorNode, anchorOffset) :
      newRange(state.document, anchorNode, anchorOffset, node, offset), backward);
  }

  setBaseAndExtent(anchorNode: Node, anchorOffset: number, focusNode: Node, focusOffset: number): void {
    if (arguments.length < 4) throw new TypeError("setBaseAndExtent requires two nodes and two offsets");
    expectNode(anchorNode);
    anchorOffset >>>= 0;
    expectNode(focusNode);
    focusOffset >>>= 0;
    if (anchorOffset > nodeLength(anchorNode) || focusOffset > nodeLength(focusNode)) {
      fail("IndexSizeError", "The offset exceeds the node's length");
    }
    const state = this.#state;
    if (nodeRoot(anchorNode) !== state.document || nodeRoot(focusNode) !== state.document) return;
    checkPoint(anchorNode, anchorOffset);
    checkPoint(focusNode, focusOffset);
    const backward = comparePoints(focusNode, focusOffset, anchorNode, anchorOffset) < 0;
    select(state, backward ? newRange(state.document, focusNode, focusOffset, anchorNode, anchorOffset) :
      newRange(state.document, anchorNode, anchorOffset, focusNode, focusOffset), backward);
  }

  selectAllChildren(node: Node): void {
    if (arguments.length < 1) throw new TypeError("selectAllChildren requires a node");
    expectNode(node);
    if (node instanceof DocumentType) fail("InvalidNodeTypeError", "Cannot select the children of a doctype");
    const state = this.#state;
    if (nodeRoot(node) !== state.document) return;
    select(state, newRange(state.document, node, 0, node, childrenOf(node).length));
  }

  deleteFromDocument(): void {
    const state = this.#state;
    const range = current(state);
    // Like Chromium, a selection inside a text control leaves the control's value alone.
    if (range !== null && state.control === null) range.deleteContents();
  }

  containsNode(node: Node, allowPartialContainment = false): boolean {
    if (arguments.length < 1) throw new TypeError("containsNode requires a node");
    expectNode(node);
    const state = this.#state;
    const range = current(state);
    if (range === null || state.control !== null || nodeRoot(node) !== state.document) return false;
    const parent = node.parentNode;
    if (parent === null) return false;
    // Chromium's test: compare the node's place in its parent with the range,
    // and count a partially selected Text node as contained.
    const index = nodeIndex(node);
    const { startContainer, startOffset, endContainer, endOffset } = range;
    if (comparePoints(parent, index, startContainer, startOffset) >= 0 &&
        comparePoints(parent, index + 1, endContainer, endOffset) <= 0) return true;
    if (comparePoints(parent, index, endContainer, endOffset) > 0 ||
        comparePoints(parent, index + 1, startContainer, startOffset) < 0) return false;
    return Boolean(allowPartialContainment) || node instanceof Text;
  }

  getComposedRanges(..._options: unknown[]): StaticRange[] {
    const state = this.#state;
    const range = current(state);
    if (range === null) return [];
    // The selection stays in the document tree, so no shadow root rescopes it. A text
    // control's selection is rescoped to the control itself, as its own shadow tree.
    if (state.control !== null) {
      const index = range.startOffset;
      return [new StaticRange({ startContainer: range.startContainer, startOffset: index, endContainer: range.startContainer, endOffset: index + 1 })];
    }
    return [new StaticRange({
      startContainer: range.startContainer, startOffset: range.startOffset, endContainer: range.endContainer, endOffset: range.endOffset,
    })];
  }

  toString(): string {
    const state = this.#state;
    const range = current(state);
    if (range === null) return "";
    if (state.control !== null) return controlText(state.control, state.inControl);
    return range.toString();
  }
}
