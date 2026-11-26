// SPDX-License-Identifier: MIT

import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";

const constructSignal = Symbol("construct AbortSignal");
type SignalState = {
  aborted: boolean;
  reason: unknown;
  dependents: Set<WeakRef<AbortSignal>>;
  sources: Set<AbortSignal>;
  handler: ((event: Event) => unknown) | null;
  listener?: (event: Event) => void;
};
const states = new WeakMap<AbortSignal, SignalState>();

function defaultReason(reason: unknown): unknown {
  return reason === undefined ? new DOMException("The operation was aborted", "AbortError") : reason;
}

function cancel(signal: AbortSignal, reason: unknown): void {
  if (signal.aborted) return;
  const marked: AbortSignal[] = [];
  const visit = (candidate: AbortSignal) => {
    const state = states.get(candidate)!;
    if (state.aborted) return;
    state.aborted = true;
    state.reason = reason;
    state.sources.clear();
    marked.push(candidate);
    for (const dependent of state.dependents) {
      const target = dependent.deref();
      if (target !== undefined) visit(target);
    }
    state.dependents.clear();
  };
  visit(signal);
  for (const target of marked) dispatchTrustedEvent(target, new Event("abort"));
}

export class AbortSignal extends EventTarget {
  constructor(token: typeof constructSignal) {
    if (token !== constructSignal) throw new TypeError("Illegal AbortSignal constructor");
    super();
    states.set(this, { aborted: false, reason: undefined, dependents: new Set(), sources: new Set(), handler: null });
  }

  get aborted(): boolean { return states.get(this)!.aborted; }
  get reason(): unknown { return states.get(this)!.reason; }
  throwIfAborted(): void { if (this.aborted) throw this.reason; }

  get onabort(): ((event: Event) => unknown) | null { return states.get(this)!.handler; }
  set onabort(callback: ((event: Event) => unknown) | null) {
    const state = states.get(this)!;
    if (typeof callback !== "function") {
      if (state.listener !== undefined) this.removeEventListener("abort", state.listener);
      state.listener = undefined;
      state.handler = null;
    } else {
      state.handler = callback;
      if (state.listener === undefined) {
        state.listener = event => state.handler?.call(this, event);
        this.addEventListener("abort", state.listener);
      }
    }
  }

  static abort(reason?: unknown): AbortSignal {
    const signal = new AbortSignal(constructSignal);
    cancel(signal, defaultReason(reason));
    return signal;
  }

  static any(signals: Iterable<AbortSignal>): AbortSignal {
    const inputs = [...signals];
    if (inputs.some(signal => !states.has(signal))) throw new TypeError("Expected AbortSignals");
    const aborted = inputs.find(signal => signal.aborted);
    if (aborted !== undefined) return AbortSignal.abort(aborted.reason);
    const signal = new AbortSignal(constructSignal);
    const state = states.get(signal)!;
    for (const input of inputs) {
      const source = states.get(input)!;
      for (const original of source.sources.size === 0 ? [input] : source.sources) {
        if (!state.sources.has(original)) {
          state.sources.add(original);
          states.get(original)!.dependents.add(new WeakRef(signal));
        }
      }
    }
    return signal;
  }

  static timeout(ms: number): AbortSignal {
    ms = Number(ms);
    if (!Number.isFinite(ms) || Math.trunc(ms) < 0 || Math.trunc(ms) > Number.MAX_SAFE_INTEGER) {
      throw new TypeError("Invalid AbortSignal timeout");
    }
    const signal = new AbortSignal(constructSignal);
    const timers = globalThis as unknown as { setTimeout(callback: () => void, delay: number): unknown };
    const reference = new WeakRef(signal);
    const end = performanceNow() + Math.trunc(ms);
    const wake = () => {
      const pending = reference.deref();
      if (pending === undefined) return;
      const remaining = end - performanceNow();
      if (remaining <= 0) cancel(pending, new DOMException("The operation timed out", "TimeoutError"));
      else timers.setTimeout(wake, Math.min(0x7fff_ffff, Math.ceil(remaining)));
    };
    timers.setTimeout(wake, Math.min(Math.trunc(ms), 0x7fff_ffff));
    return signal;
  }
}

function performanceNow(): number {
  return (globalThis as { performance?: { now(): number } }).performance?.now() ?? Date.now();
}

export class AbortController {
  #signal = new AbortSignal(constructSignal);
  get signal(): AbortSignal { return this.#signal; }
  abort(reason?: unknown): void { cancel(this.#signal, defaultReason(reason)); }
}
