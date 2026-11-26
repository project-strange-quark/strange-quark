// SPDX-License-Identifier: MIT

// Transient user activation (HTML "user activation"), as far as Strange Quark
// needs it: APIs that Chromium gates on a user gesture consume it. Chromium
// keeps a transient activation for 5 seconds.
const TRANSIENT_ACTIVATION_MS = 5000;

const activations = new WeakMap<object, number>();

function now(): number {
  return (globalThis as { performance?: { now(): number } }).performance?.now() ?? Date.now();
}

/** Records an activation-triggering trusted input event (keydown, mouse/pen pointerdown, touch pointerup). */
export function noteUserActivation(document: object): void {
  activations.set(document, now());
}

/** True while the document has transient activation. */
export function hasTransientActivation(document: object): boolean {
  const at = activations.get(document);
  return at !== undefined && now() - at <= TRANSIENT_ACTIVATION_MS;
}

/** Consumes transient activation, returning whether it was present. */
export function consumeUserActivation(document: object): boolean {
  const active = hasTransientActivation(document);
  activations.delete(document);
  return active;
}
