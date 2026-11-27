// SPDX-License-Identifier: MIT

// Event handler attributes (`onload`, `onmessage`, ...) for any EventTarget.
// It has no DOM dependency, so a worker realm can use it without a document.

import type { Event, EventTarget } from "./events.js";

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
