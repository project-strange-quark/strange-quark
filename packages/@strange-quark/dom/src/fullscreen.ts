// SPDX-License-Identifier: MIT

// The Fullscreen API. Only the
// whole window can be fullscreen, so only the document element can be the
// fullscreen element in v1. Event order and rejection types follow the pinned
// Chromium: a request's promise settles first, then `fullscreenchange` (or
// `fullscreenerror`) fires at the element in a later task, bubbling to the
// document and the window.

import { consumeUserActivation } from "./activation.js";
import { Document, Element, reflectedEvent, ShadowRoot } from "./document.js";
import { dispatchTrustedEvent, Event } from "./events.js";

type FullscreenHost = {
  /** Sends `HostRequest::Fullscreen`; settles with the host's reply. */
  send(enter: boolean): Promise<unknown>;
  postTask(callback: () => void): void;
};

type FullscreenState = {
  host: FullscreenHost;
  /** The state the host last confirmed. */
  fullscreen: boolean;
  /** Requests sent to the host and not yet answered. */
  pending: number;
};

const states = new WeakMap<Document, FullscreenState>();

function fire(document: Document, type: "fullscreenchange" | "fullscreenerror", target: Element | null): void {
  dispatchTrustedEvent(target?.isConnected ? target : document,
    new Event(type, { bubbles: true, composed: true, cancelable: false }));
}

/** Records a confirmed state; fires `fullscreenchange` in a later task when it changed. */
function settle(document: Document, state: FullscreenState, fullscreen: boolean): void {
  if (state.fullscreen === fullscreen) return;
  // Exiting fires at the element that was fullscreen: the document element.
  const target = document.documentElement;
  state.fullscreen = fullscreen;
  state.host.postTask(() => fire(document, "fullscreenchange", target));
}

function send(document: Document, state: FullscreenState, enter: boolean, target: Element | null): Promise<void> {
  state.pending++;
  return state.host.send(enter).then(() => {
    state.pending--;
    settle(document, state, enter);
  }, (error: unknown) => {
    state.pending--;
    if (enter) state.host.postTask(() => fire(document, "fullscreenerror", target));
    throw new TypeError(error instanceof Error ? error.message : String(error));
  });
}

function requestFullscreen(this: Element, _options?: unknown): Promise<void> {
  const document = this.ownerDocument as Document | null;
  const state = document === null ? undefined : states.get(document);
  if (document === null || state === undefined) {
    return Promise.reject(new TypeError("Failed to execute 'requestFullscreen' on 'Element': Document not active"));
  }
  const reject = (message: string): Promise<void> => {
    state.host.postTask(() => fire(document, "fullscreenerror", this));
    return Promise.reject(new TypeError(message));
  };
  if (this !== document.documentElement || !this.isConnected) {
    return reject("Only the document element can be fullscreen in Strange Quark");
  }
  // Chromium requires transient user activation, and consumes it.
  if (!consumeUserActivation(document)) return reject("Permissions check failed");
  if (state.fullscreen && state.pending === 0) return Promise.resolve();
  return send(document, state, true, this);
}

function exitFullscreen(this: Document): Promise<void> {
  const state = states.get(this);
  if (state === undefined || (!state.fullscreen && state.pending === 0)) {
    return Promise.reject(new TypeError("Failed to execute 'exitFullscreen' on 'Document': Document not active"));
  }
  return send(this, state, false, this.documentElement);
}

/** Installs the Fullscreen API when the host can change the window's fullscreen state. */
export function installFullscreen(document: Document, host: FullscreenHost): void {
  states.set(document, { host, fullscreen: false, pending: 0 });
  const define = (target: object, name: string, descriptor: PropertyDescriptor) =>
    Object.defineProperty(target, name, { configurable: true, ...descriptor });
  define(Element.prototype, "requestFullscreen", { writable: true, value: requestFullscreen });
  define(Document.prototype, "exitFullscreen", { writable: true, value: exitFullscreen });
  define(Document.prototype, "fullscreenEnabled", {
    enumerable: true,
    get(this: Document): boolean { return states.has(this); },
  });
  define(Document.prototype, "fullscreenElement", {
    enumerable: true,
    get(this: Document): Element | null { return states.get(this)?.fullscreen ? this.documentElement : null; },
  });
  // Only the document element can be fullscreen, and it is in no shadow tree.
  define(ShadowRoot.prototype, "fullscreenElement", {
    enumerable: true,
    get(this: ShadowRoot): Element | null { return null; },
  });
  define(Document.prototype, "fullscreen", {
    enumerable: true,
    get(this: Document): boolean { return states.get(this)?.fullscreen ?? false; },
  });
  for (const type of ["fullscreenchange", "fullscreenerror"]) {
    reflectedEvent(Document.prototype, type);
    reflectedEvent(Element.prototype, type);
  }
}

/** Applies a host task: the window changed fullscreen state without a document request. */
export function dispatchFullscreenChange(document: Document, fullscreen: boolean): void {
  const state = states.get(document);
  if (state !== undefined) settle(document, state, Boolean(fullscreen));
}
