// SPDX-License-Identifier: MIT

// HTML drag and drop for drags that start in the page, following Chromium:
// a non-cancelled mousedown on a draggable source arms a drag, which starts
// once the pointer has moved 4 CSS pixels. From then on the pointer drives
// drag events instead of mouse events: drag on the source, dragenter on a
// new element under the pointer before dragleave on the previous one,
// dragover each move, then drop (or dragleave) and dragend on release.
// Escape cancels. The drag image is recorded for the painter.

import { dragTransfer } from "./clipboard.js";
import type { DataTransfer, DropEffect, EffectAllowed } from "./clipboard.js";
import type { Document, Element } from "./document.js";
import { dispatchTrustedEvent, MouseEvent } from "./events.js";
import type { MouseInit } from "./events.js";

type DragEventInit = MouseInit & { dataTransfer?: DataTransfer | null };

export class DragEvent extends MouseEvent {
  readonly dataTransfer: DataTransfer | null;
  constructor(type: string, init: DragEventInit = {}) {
    super(type, init);
    this.dataTransfer = init.dataTransfer ?? null;
  }
}

/** Chromium's drag threshold (kDragThresholdX/Y), in CSS pixels. */
export const DRAG_THRESHOLD = 4;

type Armed = { source: Element; x: number; y: number; pointerId: number };
type Operation = {
  source: Element;
  transfer: DataTransfer;
  under: Element | null;
  effect: DropEffect;
};

export type DragHost = {
  postTask(callback: () => void): void;
  /** The drag image changed (null when the drag ends). */
  dragImage?(element: Element | null, x: number, y: number): void;
};

function attribute(element: Element, name: string): string | null {
  return element.getAttribute(name);
}

/** The element a drag starting at `target` carries, if any. */
export function dragSource(target: Element | null): Element | null {
  for (let node: Element | null = target; node !== null; node = node.parentElement) {
    const draggable = attribute(node, "draggable");
    if (draggable !== null) {
      const value = draggable.toLowerCase();
      if (value === "true") return node;
      if (value === "false") return null;
    }
    const name = node.localName;
    if (name === "img" && node.namespaceURI === "http://www.w3.org/1999/xhtml") return node;
    if (name === "a" && node.hasAttribute("href")) return node;
    // Form controls keep pointer gestures for their own editing.
    if (name === "input" || name === "textarea" || name === "select" || name === "button") return null;
  }
  return null;
}

function defaultEffect(allowed: EffectAllowed): DropEffect {
  switch (allowed) {
    case "none": return "none";
    case "link": case "linkMove": return "link";
    case "move": return "move";
    default: return "copy";
  }
}

function allows(allowed: EffectAllowed, effect: DropEffect): boolean {
  if (effect === "none") return false;
  switch (allowed) {
    case "all": case "uninitialized": return true;
    case "copyLink": return effect === "copy" || effect === "link";
    case "copyMove": return effect === "copy" || effect === "move";
    case "linkMove": return effect === "link" || effect === "move";
    default: return allowed === effect;
  }
}

function editable(element: Element | null): boolean {
  if (element === null) return false;
  const name = element.localName;
  if (name === "textarea") return !element.hasAttribute("disabled") && !element.hasAttribute("readonly");
  if (name !== "input") return false;
  const type = (attribute(element, "type") ?? "text").toLowerCase();
  return ["text", "search", "url", "tel", "email", "password", ""].includes(type) &&
    !element.hasAttribute("disabled") && !element.hasAttribute("readonly");
}

export class DragController {
  #armed: Armed | null = null;
  #operation: Operation | null = null;
  // A drag cancelled before its button was released (Escape) still owns
  // that release: the page sees neither mouseup nor click for it.
  #swallowRelease = false;
  readonly #document: Document;
  readonly #host: DragHost;

  constructor(document: Document, host: DragHost) {
    this.#document = document;
    this.#host = host;
  }

  get dragging(): boolean {
    return this.#operation !== null;
  }

  /** After a non-cancelled primary mousedown on `target`. */
  arm(target: Element | null, x: number, y: number, pointerId: number): void {
    const source = dragSource(target);
    this.#armed = source === null ? null : { source, x, y, pointerId };
  }

  disarm(): void {
    this.#armed = null;
  }

  #event(type: string, target: Element, init: MouseInit, cancelable: boolean): boolean {
    const operation = this.#operation!;
    const event = new DragEvent(type, {
      ...init,
      bubbles: true,
      cancelable,
      composed: true,
      button: 0,
      dataTransfer: operation.transfer,
    });
    return dispatchTrustedEvent(target, event);
  }

  /**
   * A pointer move with the primary button down. Returns true when the drag
   * consumed it, so no pointer or mouse events go to the page.
   */
  move(hit: Element | null, init: MouseInit & { clientX: number; clientY: number }, pointerId: number, cancelPointer: () => void): boolean {
    if (this.#operation === null) {
      const armed = this.#armed;
      if (armed === null || armed.pointerId !== pointerId) return false;
      if (Math.abs(init.clientX - armed.x) < DRAG_THRESHOLD && Math.abs(init.clientY - armed.y) < DRAG_THRESHOLD) return false;
      this.#armed = null;
      if (!armed.source.isConnected) return false;
      const transfer = dragTransfer.create(callback => this.#host.postTask(callback));
      dragTransfer.state(transfer).effectAllowed = "uninitialized";
      const source = armed.source;
      if (source.localName === "a") {
        const href = (source as unknown as { href: string }).href;
        transfer.setData("text/uri-list", href);
        transfer.setData("text/plain", href);
      } else if (source.localName === "img") {
        const src = (source as unknown as { src: string }).src;
        transfer.setData("text/uri-list", src);
      }
      this.#operation = { source, transfer, under: null, effect: "none" };
      dragTransfer.mode(transfer, "readwrite");
      const started = this.#event("dragstart", source, init, true);
      dragTransfer.mode(transfer, "protected");
      if (!started) {
        this.#operation = null;
        return false;
      }
      cancelPointer();
      const image = dragTransfer.state(transfer).image;
      this.#host.dragImage?.((image?.element as Element | undefined) ?? source, image?.x ?? 0, image?.y ?? 0);
    }
    this.#step(hit, init);
    return true;
  }

  #step(hit: Element | null, init: MouseInit): void {
    const operation = this.#operation!;
    if (!this.#event("drag", operation.source, init, true)) {
      this.cancel(init);
      return;
    }
    const state = dragTransfer.state(operation.transfer);
    if (hit !== operation.under) {
      const previous = operation.under;
      operation.under = hit;
      if (hit !== null) {
        state.dropEffect = defaultEffect(state.effectAllowed);
        this.#event("dragenter", hit, init, true);
      }
      if (previous?.isConnected) this.#event("dragleave", previous, init, false);
    }
    if (operation.under === null) {
      operation.effect = "none";
      return;
    }
    state.dropEffect = defaultEffect(state.effectAllowed);
    const accepted = !this.#event("dragover", operation.under, init, true);
    if (accepted) {
      operation.effect = allows(state.effectAllowed, state.dropEffect) ? state.dropEffect : "none";
    } else {
      // An editable text field accepts text drops by default.
      operation.effect = editable(operation.under) && operation.transfer.types.includes("text/plain")
        ? defaultEffect(state.effectAllowed)
        : "none";
    }
  }

  /** The primary button was released. Returns true when a drag consumed it. */
  release(init: MouseInit): boolean {
    this.#armed = null;
    const operation = this.#operation;
    if (operation === null) return false;
    const state = dragTransfer.state(operation.transfer);
    let effect: DropEffect = "none";
    if (operation.effect !== "none" && operation.under?.isConnected) {
      state.dropEffect = operation.effect;
      dragTransfer.mode(operation.transfer, "readonly");
      const handled = !this.#event("drop", operation.under, init, true);
      dragTransfer.mode(operation.transfer, "protected");
      if (handled) effect = state.dropEffect;
      else if (editable(operation.under)) effect = operation.effect;
    } else if (operation.under?.isConnected) {
      this.#event("dragleave", operation.under, init, false);
    }
    this.#finish(effect, init);
    return true;
  }

  /** The release that ends a cancelled drag's gesture, if it is still pending. */
  takeRelease(): boolean {
    const swallow = this.#swallowRelease;
    this.#swallowRelease = false;
    return swallow;
  }

  /** Escape, a window blur or a cancelled pointer ends the drag without a drop. */
  cancel(init: MouseInit = {}, buttonHeld = true): boolean {
    this.#armed = null;
    const operation = this.#operation;
    if (operation === null) return false;
    this.#swallowRelease = buttonHeld;
    if (operation.under?.isConnected) this.#event("dragleave", operation.under, init, false);
    this.#finish("none", init);
    return true;
  }

  #finish(effect: DropEffect, init: MouseInit): void {
    const operation = this.#operation!;
    dragTransfer.state(operation.transfer).dropEffect = effect;
    if (operation.source.isConnected) this.#event("dragend", operation.source, init, false);
    else this.#event("dragend", this.#document as unknown as Element, init, false);
    this.#operation = null;
    this.#host.dragImage?.(null, 0, 0);
  }
}
