// SPDX-License-Identifier: MIT

import { connectedInputTarget, Document, Element } from "./document.js";
import { dispatchTrustedEvent, Event, EventTarget } from "./events.js";

type Result = { media: string; matches: boolean };
type Host = { matchMedia(query: string): Result };
type Handler = ((event: MediaQueryListEvent) => void) | null;
const internal = Symbol("MediaQueryList constructor");

export class MediaQueryListEvent extends Event {
  readonly media: string;
  readonly matches: boolean;
  constructor(type: string, init?: { media?: string; matches?: boolean; bubbles?: boolean; cancelable?: boolean; composed?: boolean }) {
    super(type, init);
    this.media = String(init?.media ?? "");
    this.matches = Boolean(init?.matches);
  }
}

const states = new WeakMap<MediaQueryList, { query: string; value: Result }>();

export class MediaQueryList extends EventTarget {
  #handler: Handler = null;
  #notify = (event: Event) => this.#handler?.call(this, event as MediaQueryListEvent);
  constructor(token?: symbol, query?: string, value?: Result) {
    super();
    if (token !== internal || query === undefined || value === undefined) throw new TypeError("Illegal constructor");
    states.set(this, { query, value });
  }
  get matches(): boolean { return states.get(this)!.value.matches; }
  get media(): string { return states.get(this)!.value.media; }
  get onchange(): Handler { return this.#handler; }
  set onchange(handler: Handler) {
    const next = typeof handler === "function" ? handler : null;
    if (this.#handler === null && next !== null) this.addEventListener("change", this.#notify);
    if (this.#handler !== null && next === null) this.removeEventListener("change", this.#notify);
    this.#handler = next;
  }
  addListener(listener: Parameters<EventTarget["addEventListener"]>[1]): void { this.addEventListener("change", listener); }
  removeListener(listener: Parameters<EventTarget["removeEventListener"]>[1]): void { this.removeEventListener("change", listener); }
}

export function installMedia(global: Record<PropertyKey, unknown>, host: Host) {
  const queries = new Set<MediaQueryList>();
  Object.assign(global, {
    MediaQueryList, MediaQueryListEvent, TransitionEvent, AnimationEvent,
    matchMedia(query: string): MediaQueryList {
      if (arguments.length === 0) throw new TypeError("matchMedia requires a query");
      query = String(query);
      const list = new MediaQueryList(internal, query, host.matchMedia(query));
      queries.add(list);
      return list;
    },
  });
  return function viewport(): void {
    const changes: MediaQueryList[] = [];
    for (const list of queries) {
      const state = states.get(list)!;
      const result = host.matchMedia(state.query);
      if (result.matches !== state.value.matches) changes.push(list);
      state.value = result;
    }
    for (const list of changes) dispatchTrustedEvent(list, new MediaQueryListEvent("change", list));
  };
}

type TimedInit = { elapsedTime?: number; pseudoElement?: string; bubbles?: boolean; cancelable?: boolean; composed?: boolean };

export class TransitionEvent extends Event {
  readonly propertyName: string;
  readonly elapsedTime: number;
  readonly pseudoElement: string;
  constructor(type: string, init?: TimedInit & { propertyName?: string }) {
    super(type, init);
    this.propertyName = String(init?.propertyName ?? "");
    this.elapsedTime = Number(init?.elapsedTime ?? 0);
    this.pseudoElement = String(init?.pseudoElement ?? "");
  }
}

export class AnimationEvent extends Event {
  readonly animationName: string;
  readonly elapsedTime: number;
  readonly pseudoElement: string;
  constructor(type: string, init?: TimedInit & { animationName?: string }) {
    super(type, init);
    this.animationName = String(init?.animationName ?? "");
    this.elapsedTime = Number(init?.elapsedTime ?? 0);
    this.pseudoElement = String(init?.pseudoElement ?? "");
  }
}

export function dispatchAnimationEvent(document: Document, task: {
  target: number; pseudo: string | null; type: string; name: string; elapsedTime: number;
}): void {
  const target = connectedInputTarget(document, task.target);
  if (!(target instanceof Element)) return;
  const init = { bubbles: true, elapsedTime: task.elapsedTime, pseudoElement: task.pseudo ?? "" };
  const event = task.type.startsWith("transition")
    ? new TransitionEvent(task.type, { ...init, propertyName: task.name })
    : new AnimationEvent(task.type, { ...init, animationName: task.name });
  dispatchTrustedEvent(target, event);
}
