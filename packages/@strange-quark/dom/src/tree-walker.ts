// SPDX-License-Identifier: MIT

import { Node } from "./document.js";
import { DOMException } from "./events.js";

const token = Symbol("TreeWalker constructor");

export const NodeFilter = Object.freeze({
  FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
  SHOW_ALL: 0xffff_ffff, SHOW_ELEMENT: 1, SHOW_ATTRIBUTE: 2, SHOW_TEXT: 4, SHOW_CDATA_SECTION: 8,
  SHOW_ENTITY_REFERENCE: 16, SHOW_ENTITY: 32, SHOW_PROCESSING_INSTRUCTION: 64, SHOW_COMMENT: 128,
  SHOW_DOCUMENT: 256, SHOW_DOCUMENT_TYPE: 512, SHOW_DOCUMENT_FRAGMENT: 1024, SHOW_NOTATION: 2048,
});

type Filter = ((node: Node) => number) | { acceptNode(node: Node): number };

export class TreeWalker {
  #root: Node;
  #current: Node;
  #whatToShow: number;
  #filter: Filter | null;
  #filtering = false;

  constructor(check: symbol, root: Node, whatToShow: number, filter: Filter | null) {
    if (check !== token) throw new TypeError("Illegal TreeWalker constructor");
    this.#root = this.#current = root;
    this.#whatToShow = whatToShow;
    this.#filter = filter;
  }

  get root(): Node { return this.#root; }
  get whatToShow(): number { return this.#whatToShow; }
  get filter(): Filter | null { return this.#filter; }
  get currentNode(): Node { return this.#current; }
  set currentNode(node: Node) {
    if (!(node instanceof Node)) throw new TypeError("Expected a Node");
    this.#current = node;
  }

  #accept(node: Node): number {
    if ((this.#whatToShow & (1 << (node.nodeType - 1))) === 0) return NodeFilter.FILTER_SKIP;
    const filter = this.#filter;
    if (filter === null) return NodeFilter.FILTER_ACCEPT;
    if (this.#filtering) throw new DOMException("The filter is already running", "InvalidStateError");
    this.#filtering = true;
    try {
      return (typeof filter === "function" ? filter(node) : filter.acceptNode(node)) & 0xffff;
    } finally {
      this.#filtering = false;
    }
  }

  parentNode(): Node | null {
    let node = this.#current;
    while (node !== this.#root && node.parentNode !== null) {
      node = node.parentNode;
      if (this.#accept(node) === NodeFilter.FILTER_ACCEPT) return this.#current = node;
    }
    return null;
  }

  #branch(start: Node | null, top: Node, forward: boolean): Node | null {
    let node = start;
    while (node !== null) {
      const result = this.#accept(node);
      if (result === NodeFilter.FILTER_ACCEPT) return node;
      const child = forward ? node.firstChild : node.lastChild;
      if (result === NodeFilter.FILTER_SKIP && child !== null) {
        node = child;
      } else {
        while (node !== null && node !== top) {
          const sibling: Node | null = forward ? node.nextSibling : node.previousSibling;
          if (sibling !== null) { node = sibling; break; }
          node = node.parentNode;
        }
        if (node === top) return null;
      }
    }
    return null;
  }

  firstChild(): Node | null {
    const node = this.#branch(this.#current.firstChild, this.#current, true);
    return node === null ? null : this.#current = node;
  }

  lastChild(): Node | null {
    const node = this.#branch(this.#current.lastChild, this.#current, false);
    return node === null ? null : this.#current = node;
  }

  #sibling(forward: boolean): Node | null {
    for (let node = this.#current; node !== this.#root && node.parentNode !== null; node = node.parentNode) {
      const candidate = this.#branch(forward ? node.nextSibling : node.previousSibling, node.parentNode, forward);
      if (candidate !== null) return this.#current = candidate;
      if (node.parentNode === this.#root || this.#accept(node.parentNode) === NodeFilter.FILTER_ACCEPT) break;
    }
    return null;
  }

  nextSibling(): Node | null { return this.#sibling(true); }
  previousSibling(): Node | null { return this.#sibling(false); }

  nextNode(): Node | null {
    let node = this.#current;
    let result: number = NodeFilter.FILTER_ACCEPT;
    for (;;) {
      if (result !== NodeFilter.FILTER_REJECT && node.firstChild !== null) {
        node = node.firstChild;
      } else {
        while (node !== this.#root && node.nextSibling === null && node.parentNode !== null) node = node.parentNode;
        if (node === this.#root || node.nextSibling === null) return null;
        node = node.nextSibling;
      }
      result = this.#accept(node);
      if (result === NodeFilter.FILTER_ACCEPT) return this.#current = node;
    }
  }

  previousNode(): Node | null {
    let node = this.#current;
    while (node !== this.#root) {
      let sibling = node.previousSibling;
      while (sibling !== null) {
        node = sibling;
        let result = this.#accept(node);
        while (result !== NodeFilter.FILTER_REJECT && node.lastChild !== null) {
          node = node.lastChild;
          result = this.#accept(node);
        }
        if (result === NodeFilter.FILTER_ACCEPT) return this.#current = node;
        sibling = node.previousSibling;
      }
      if (node === this.#root || node.parentNode === null) return null;
      node = node.parentNode;
      if (this.#accept(node) === NodeFilter.FILTER_ACCEPT) return this.#current = node;
    }
    return null;
  }
}

export function treeWalker(root: Node, whatToShow: number, filter: Filter | null): TreeWalker {
  if (!(root instanceof Node)) throw new TypeError("Expected a root Node");
  if (filter !== null && typeof filter !== "object" && typeof filter !== "function") throw new TypeError("Expected a NodeFilter");
  return new TreeWalker(token, root, whatToShow >>> 0, filter);
}
