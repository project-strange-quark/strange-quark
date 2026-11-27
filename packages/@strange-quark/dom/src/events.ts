// SPDX-License-Identifier: MIT

import { ListenerFlags } from "./journal.js";

const legacyCodes: Record<string, number> = {
  IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, NoModificationAllowedError: 7,
  InvalidCharacterError: 5, NotFoundError: 8, NotSupportedError: 9,
  InvalidStateError: 11, SyntaxError: 12, NamespaceError: 14, InvalidNodeTypeError: 24,
  AbortError: 20, TimeoutError: 23, DataCloneError: 25,
};

export class DOMException extends Error {
  constructor(message = "", name = "Error") {
    super(String(message));
    this.name = String(name);
  }

  get code(): number {
    return Object.hasOwn(legacyCodes, this.name) ? legacyCodes[this.name] : 0;
  }
}

export const eventParent = Symbol("event parent");
export const eventActivation = Symbol("event activation");
export const defaultPassive = Symbol("default passive listener");
const dispatch = Symbol("dispatch event");

type EventInit = { bubbles?: boolean; cancelable?: boolean; composed?: boolean };
type EventState = {
  type: string;
  bubbles: boolean;
  cancelable: boolean;
  composed: boolean;
  target: EventTarget | null;
  currentTarget: EventTarget | null;
  phase: number;
  path: EventTarget[];
  dispatching: boolean;
  initialized: boolean;
  trusted: boolean;
  stopped: boolean;
  immediate: boolean;
  canceled: boolean;
  passive: boolean;
  time: number;
};
const events = new WeakMap<Event, EventState>();

export class Event {
  static readonly NONE = 0;
  static readonly CAPTURING_PHASE = 1;
  static readonly AT_TARGET = 2;
  static readonly BUBBLING_PHASE = 3;

  constructor(type: string, init?: EventInit | null) {
    if (arguments.length === 0) {
      throw new TypeError("Event requires a type");
    }
    const clock = globalThis as { performance?: { now(): number } };
    events.set(this, {
      type: String(type), bubbles: Boolean(init?.bubbles), cancelable: Boolean(init?.cancelable),
      composed: Boolean(init?.composed), target: null, currentTarget: null, phase: Event.NONE,
      path: [], dispatching: false, initialized: true, trusted: false, stopped: false, immediate: false, canceled: false, passive: false, time: clock.performance?.now() ?? 0,
    });
  }

  get NONE(): number { return Event.NONE; }
  get CAPTURING_PHASE(): number { return Event.CAPTURING_PHASE; }
  get AT_TARGET(): number { return Event.AT_TARGET; }
  get BUBBLING_PHASE(): number { return Event.BUBBLING_PHASE; }
  get timeStamp(): number { return events.get(this)!.time; }
  get type(): string { return events.get(this)!.type; }
  get bubbles(): boolean { return events.get(this)!.bubbles; }
  get cancelable(): boolean { return events.get(this)!.cancelable; }
  get composed(): boolean { return events.get(this)!.composed; }
  get eventPhase(): number { return events.get(this)!.phase; }
  get isTrusted(): boolean { return events.get(this)!.trusted; }
  get target(): EventTarget | null { return events.get(this)!.target; }
  get srcElement(): EventTarget | null { return this.target; }
  get currentTarget(): EventTarget | null { return events.get(this)!.currentTarget; }
  get defaultPrevented(): boolean { return events.get(this)!.canceled; }
  get cancelBubble(): boolean { return events.get(this)!.stopped; }
  set cancelBubble(value: boolean) { if (value) this.stopPropagation(); }
  get returnValue(): boolean { return !this.defaultPrevented; }
  set returnValue(value: boolean) { if (!value) this.preventDefault(); }

  composedPath(): EventTarget[] {
    return events.get(this)!.path.slice();
  }

  preventDefault(): void {
    const state = events.get(this)!;
    if (state.cancelable && !state.passive) {
      state.canceled = true;
    }
  }

  stopPropagation(): void {
    events.get(this)!.stopped = true;
  }

  stopImmediatePropagation(): void {
    const state = events.get(this)!;
    state.stopped = true;
    state.immediate = true;
  }

  initEvent(type: string, bubbles = false, cancelable = false): void {
    const state = events.get(this)!;
    if (state.dispatching) return;
    state.type = String(type);
    state.initialized = true;
    state.trusted = false;
    state.bubbles = Boolean(bubbles);
    state.cancelable = Boolean(cancelable);
    state.target = null;
    state.stopped = false;
    state.immediate = false;
    state.canceled = false;
  }
}

export class CustomEvent extends Event {
  readonly detail: unknown;

  constructor(type: string, init?: EventInit & { detail?: unknown }) {
    if (arguments.length === 0) throw new TypeError("CustomEvent requires a type");
    super(type, init);
    this.detail = init?.detail ?? null;
  }
}

type Callback = ((event: Event) => void) | { handleEvent(event: Event): void };
type Signal = {
  readonly aborted: boolean;
  addEventListener(type: "abort", callback: () => void, options: { once: boolean }): void;
  removeEventListener(type: "abort", callback: () => void): void;
};
type ListenerOptions = boolean | { capture?: boolean; once?: boolean; passive?: boolean; signal?: Signal | null };
type Listener = {
  callback: Callback;
  capture: boolean;
  passive: boolean;
  once: boolean;
  signal: Signal | null;
  abort?: () => void;
  removed: boolean;
};
// Most DOM nodes never receive a listener. This marks their EventTarget
// identity until the first registration needs a private map.
const emptyListeners = new Map<string, Listener[]>();
const listeners = new WeakMap<EventTarget, Map<string, Listener[]>>();

export const listenerFlagsChanged = Symbol("listener flags changed");
const listenerReports = new WeakMap<EventTarget, (flags: number) => void>();
const listenerFlags = new WeakMap<EventTarget, number>();

export function observeListenerFlags(target: EventTarget, report: (flags: number) => void): void {
  listenerReports.set(target, report);
  const flags = listenerFlags.get(target);
  if (flags !== undefined) report(flags);
}

function updateListenerFlags(target: EventTarget, type: string): void {
  if (type !== "wheel" && type !== "touchmove") return;
  const groups = listeners.get(target)!;
  const blocking = (type: string) => groups.get(type)?.some(listener => !listener.passive) ?? false;
  const flags = (blocking("wheel") ? ListenerFlags.NonPassiveWheel : 0) |
    (blocking("touchmove") ? ListenerFlags.NonPassiveTouchmove : 0);
  if (flags === (listenerFlags.get(target) ?? 0)) return;
  if (flags === 0) listenerFlags.delete(target);
  else listenerFlags.set(target, flags);
  target[listenerFlagsChanged](flags);
}

function remove(target: EventTarget, type: string, list: Listener[], listener: Listener): void {
  if (listener.removed) return;
  listener.removed = true;
  list.splice(list.indexOf(listener), 1);
  if (listener.abort !== undefined) {
    listener.signal!.removeEventListener("abort", listener.abort);
  }
  updateListenerFlags(target, type);
}

function report(error: unknown): void {
  const host = globalThis as { reportError?: (error: unknown) => void; console?: { error(error: unknown): void } };
  if (host.reportError !== undefined) host.reportError(error);
  else host.console?.error(error);
}

function invoke(target: EventTarget, event: Event, state: EventState, capture: boolean): void {
  if (state.stopped) return;
  state.currentTarget = target;
  const list = listeners.get(target)!.get(state.type);
  for (const listener of list?.slice() ?? []) {
    if (listener.removed || listener.capture !== capture || listener.signal?.aborted) continue;
    if (listener.once) remove(target, state.type, list!, listener);
    state.passive = listener.passive;
    const view = (globalThis as { window?: { event?: Event } }).window;
    const previous = view?.event;
    if (view !== undefined) view.event = event;
    try {
      const callback = listener.callback;
      if (typeof callback === "function") callback.call(target, event);
      else callback.handleEvent(event);
    } catch (error) {
      report(error);
    } finally {
      if (view !== undefined) view.event = previous;
      state.passive = false;
    }
    if (state.immediate) break;
  }
}

/** Share the listener storage with a Node's named-property wrapper. */
export function aliasEventTarget(target: EventTarget, wrapper: EventTarget): void {
  listeners.set(wrapper, listeners.get(target)!);
}

export class EventTarget {
  constructor() {
    listeners.set(this, emptyListeners);
  }

  [listenerFlagsChanged](flags: number): void { listenerReports.get(this)?.(flags); }

  [eventParent](_event: Event): EventTarget | null {
    return null;
  }

  [eventActivation](_event: MouseEvent): ((canceled: boolean) => void) | undefined {
    return undefined;
  }

  [defaultPassive](): boolean { return false; }

  addEventListener(type: string, callback: Callback | null, options?: ListenerOptions | null): void {
    if (arguments.length < 2) throw new TypeError("addEventListener requires a type and callback");
    type = String(type);
    const dictionary = typeof options === "object" ? options : null;
    const capture = typeof options === "boolean" ? options : Boolean(dictionary?.capture);
    const once = Boolean(dictionary?.once);
    const passive = dictionary?.passive === undefined
      ? (type === "wheel" || type === "touchstart" || type === "touchmove") && this[defaultPassive]()
      : Boolean(dictionary.passive);
    const signal = dictionary?.signal;
    if (signal !== undefined && (signal === null || typeof signal.aborted !== "boolean" || typeof signal.addEventListener !== "function")) {
      throw new TypeError("Expected an AbortSignal");
    }
    if (callback == null || signal?.aborted) return;
    if (typeof callback !== "function" && typeof callback !== "object") throw new TypeError("Invalid event listener");
    let groups = listeners.get(this)!;
    if (groups === emptyListeners) {
      groups = new Map();
      listeners.set(this, groups);
    }
    let list = groups.get(type);
    if (list === undefined) {
      list = [];
      groups.set(type, list);
    }
    if (list.some((entry) => entry.callback === callback && entry.capture === capture)) return;

    const listener: Listener = { callback, capture, passive, once, signal: signal ?? null, removed: false };
    list.push(listener);
    if (signal !== undefined) {
      const owned = list;
      listener.abort = () => remove(this, type, owned, listener);
      signal.addEventListener("abort", listener.abort, { once: true });
    }
    updateListenerFlags(this, type);
  }

  removeEventListener(type: string, callback: Callback | null, options?: ListenerOptions | null): void {
    if (arguments.length < 2) throw new TypeError("removeEventListener requires a type and callback");
    const capture = typeof options === "boolean" ? options : Boolean(options?.capture);
    type = String(type);
    const list = listeners.get(this)!.get(type);
    const listener = list?.find((entry) => entry.callback === callback && entry.capture === capture);
    if (listener !== undefined) remove(this, type, list!, listener);
  }

  dispatchEvent(event: Event): boolean {
    return this[dispatch](event, false);
  }

  [dispatch](event: Event, trusted: boolean): boolean {
    const state = events.get(event);
    if (state === undefined) throw new TypeError("Expected an Event");
    if (state.dispatching || !state.initialized) throw new DOMException("The event cannot be dispatched", "InvalidStateError");

    const path = [this as EventTarget];
    for (let parent = this[eventParent](event); parent !== null; parent = parent[eventParent](event)) {
      path.push(parent);
    }
    state.dispatching = true;
    state.trusted = trusted;
    state.target = this;
    state.path = path;
    let activate: ((canceled: boolean) => void) | undefined;
    try {
      if (state.type === "click" && event instanceof MouseEvent) {
        for (let index = 0; index < (state.bubbles ? path.length : 1); index++) {
          activate = path[index][eventActivation](event);
          if (activate !== undefined) break;
        }
      }
      for (let index = path.length - 1; index >= 0; index--) {
        state.phase = index === 0 ? Event.AT_TARGET : Event.CAPTURING_PHASE;
        invoke(path[index], event, state, true);
      }
      for (let index = 0; index < (state.bubbles ? path.length : 1); index++) {
        state.phase = index === 0 ? Event.AT_TARGET : Event.BUBBLING_PHASE;
        invoke(path[index], event, state, false);
      }
    } finally {
      state.phase = Event.NONE;
      state.currentTarget = null;
      state.path = [];
      state.dispatching = false;
      state.stopped = false;
      state.immediate = false;
    }
    activate?.(state.canceled);
    return !state.canceled;
  }
}

export function dispatchTrustedEvent(target: EventTarget, event: Event, time?: number): boolean {
  if (time !== undefined) events.get(event)!.time = time;
  return target[dispatch](event, true);
}

type WindowErrorHandler = (event: Event | string, source?: string, line?: number, column?: number, error?: unknown) => unknown;
const windowErrorHandlers = new WeakMap<Window, { callback: WindowErrorHandler; listener: (event: Event) => void }>();

export class Window extends EventTarget {
  private constructor() {
    super();
    throw new TypeError("Illegal Window constructor");
  }

  get onerror(): WindowErrorHandler | null { return windowErrorHandlers.get(this)?.callback ?? null; }
  set onerror(callback: WindowErrorHandler | null) {
    const handler = windowErrorHandlers.get(this);
    if (typeof callback !== "function") {
      if (handler !== undefined) {
        this.removeEventListener("error", handler.listener);
        windowErrorHandlers.delete(this);
      }
      return;
    }
    if (handler !== undefined) {
      handler.callback = callback;
      return;
    }
    const listener = (event: Event) => {
      const handle = windowErrorHandlers.get(this)!.callback;
      if (event instanceof ErrorEvent) {
        if (handle.call(this, event.message, event.filename, event.lineno, event.colno, event.error) === true) {
          event.preventDefault();
        }
      } else if (handle.call(this, event) === false) {
        event.preventDefault();
      }
    };
    windowErrorHandlers.set(this, { callback, listener });
    this.addEventListener("error", listener);
  }

  override [defaultPassive](): boolean { return true; }
}

/** Applies EventTarget behavior to the realm's existing global object. */
export function installWindowEventTarget(global: object): EventTarget {
  const target = global as EventTarget;
  Object.setPrototypeOf(target, Window.prototype);
  listeners.set(target, emptyListeners);
  listenerFlags.delete(target);
  for (const key of ["addEventListener", "removeEventListener", "dispatchEvent"] as const) {
    Object.defineProperty(target, key, {
      value: EventTarget.prototype[key].bind(target), configurable: true, writable: true,
    });
  }
  return target;
}

type FocusInit = EventInit & { relatedTarget?: EventTarget | null };
export type MouseInit = FocusInit & {
  detail?: number; button?: number; buttons?: number; clientX?: number; clientY?: number;
  screenX?: number; screenY?: number; ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean; metaKey?: boolean;
  view?: EventTarget | null;
};

export class FocusEvent extends Event {
  readonly relatedTarget: EventTarget | null;
  constructor(type: string, init?: FocusInit) {
    super(type, init);
    this.relatedTarget = init?.relatedTarget ?? null;
  }
}

export class MouseEvent extends Event {
  detail: number;
  button: number;
  buttons: number;
  clientX: number;
  clientY: number;
  screenX: number;
  screenY: number;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  relatedTarget: EventTarget | null;
  view: EventTarget | null;

  constructor(type: string, init: MouseInit = {}) {
    super(type, init);
    this.detail = Number(init.detail) | 0;
    this.button = Number(init.button) | 0;
    this.buttons = Number(init.buttons) | 0;
    this.clientX = Number(init.clientX) || 0;
    this.clientY = Number(init.clientY) || 0;
    this.screenX = Number(init.screenX) || 0;
    this.screenY = Number(init.screenY) || 0;
    this.ctrlKey = Boolean(init.ctrlKey);
    this.altKey = Boolean(init.altKey);
    this.shiftKey = Boolean(init.shiftKey);
    this.metaKey = Boolean(init.metaKey);
    this.relatedTarget = init.relatedTarget ?? null;
    this.view = init.view ?? null;
  }

  get x(): number { return this.clientX; }
  get y(): number { return this.clientY; }
  get which(): number { return this.button + 1; }
  getModifierState(key: string): boolean { return modifier(this, key); }

  initMouseEvent(type: string, bubbles = false, cancelable = false, view: EventTarget | null = null,
    detail = 0, screenX = 0, screenY = 0, clientX = 0, clientY = 0, ctrlKey = false,
    altKey = false, shiftKey = false, metaKey = false, button = 0, relatedTarget: EventTarget | null = null): void {
    if (events.get(this)!.dispatching) return;
    this.initEvent(type, bubbles, cancelable);
    Object.assign(this, { view, detail, screenX, screenY, clientX, clientY, ctrlKey, altKey, shiftKey, metaKey, button, relatedTarget });
  }
}

type PointerInit = MouseInit & {
  pointerId?: number; width?: number; height?: number; pressure?: number; pointerType?: string; isPrimary?: boolean;
};
const coalescedEvents = new WeakMap<PointerEvent, PointerEvent[]>();

export class PointerEvent extends MouseEvent {
  readonly pointerId: number;
  readonly width: number;
  readonly height: number;
  readonly pressure: number;
  readonly pointerType: string;
  readonly isPrimary: boolean;

  constructor(type: string, init: PointerInit = {}) {
    super(type, init);
    this.pointerId = Number(init.pointerId) | 0;
    this.width = Number(init.width ?? 1);
    this.height = Number(init.height ?? 1);
    this.pressure = Number(init.pressure ?? 0);
    this.pointerType = String(init.pointerType ?? "");
    this.isPrimary = Boolean(init.isPrimary);
  }

  getCoalescedEvents(): PointerEvent[] { return coalescedEvents.get(this)?.slice() ?? []; }
}

export function setPointerSamples(event: PointerEvent, samples: PointerEvent[], times: readonly number[], target: EventTarget): void {
  for (const [index, sample] of samples.entries()) {
    const state = events.get(sample)!;
    state.trusted = true;
    state.time = times[index];
    state.target = target;
  }
  coalescedEvents.set(event, samples);
}

export class WheelEvent extends MouseEvent {
  static readonly DOM_DELTA_PIXEL = 0;
  static readonly DOM_DELTA_LINE = 1;
  static readonly DOM_DELTA_PAGE = 2;

  readonly deltaX: number;
  readonly deltaY: number;
  readonly deltaZ: number;
  readonly deltaMode: number;

  constructor(type: string, init: MouseInit & { deltaX?: number; deltaY?: number; deltaZ?: number; deltaMode?: number } = {}) {
    super(type, init);
    this.deltaX = Number(init.deltaX ?? 0);
    this.deltaY = Number(init.deltaY ?? 0);
    this.deltaZ = Number(init.deltaZ ?? 0);
    this.deltaMode = Number(init.deltaMode) >>> 0;
  }
}

function modifier(event: MouseEvent | KeyboardEvent, key: string): boolean {
  switch (key) {
    case "Alt": return event.altKey;
    case "Control": return event.ctrlKey;
    case "Meta": return event.metaKey;
    case "Shift": return event.shiftKey;
    default: return false;
  }
}

export type KeyboardInit = EventInit & {
  key?: string; code?: string; location?: number; repeat?: boolean; isComposing?: boolean;
  ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean; metaKey?: boolean; keyCode?: number; charCode?: number;
};

export class KeyboardEvent extends Event {
  readonly key: string;
  readonly code: string;
  readonly location: number;
  readonly repeat: boolean;
  readonly isComposing: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
  readonly metaKey: boolean;
  readonly keyCode: number;
  readonly charCode: number;
  constructor(type: string, init: KeyboardInit = {}) {
    super(type, init);
    this.key = String(init.key ?? "");
    this.code = String(init.code ?? "");
    this.location = Number(init.location) >>> 0;
    this.repeat = Boolean(init.repeat);
    this.isComposing = Boolean(init.isComposing);
    this.ctrlKey = Boolean(init.ctrlKey);
    this.altKey = Boolean(init.altKey);
    this.shiftKey = Boolean(init.shiftKey);
    this.metaKey = Boolean(init.metaKey);
    this.keyCode = Number(init.keyCode) >>> 0;
    this.charCode = Number(init.charCode) >>> 0;
  }

  get which(): number { return this.type === "keypress" ? this.charCode : this.keyCode; }
  getModifierState(key: string): boolean { return modifier(this, key); }
}

export class InputEvent extends Event {
  readonly data: string | null;
  readonly inputType: string;
  readonly isComposing: boolean;

  constructor(type: string, init: EventInit & { data?: string | null; inputType?: string; isComposing?: boolean } = {}) {
    super(type, init);
    this.data = init.data == null ? null : String(init.data);
    this.inputType = String(init.inputType ?? "");
    this.isComposing = Boolean(init.isComposing);
  }
}

export class SubmitEvent extends Event {
  readonly submitter: EventTarget | null;

  constructor(type: string, init: EventInit & { submitter?: EventTarget | null } = {}) {
    super(type, init);
    this.submitter = init.submitter ?? null;
  }
}

export class ErrorEvent extends Event {
  readonly error: unknown;
  readonly message: string;
  readonly filename: string;
  readonly lineno: number;
  readonly colno: number;
  constructor(type: string, init: EventInit & {
    error?: unknown; message?: string; filename?: string; lineno?: number; colno?: number;
  } = {}) {
    super(type, init);
    this.error = init.error ?? null;
    this.message = String(init.message ?? "");
    this.filename = String(init.filename ?? "");
    this.lineno = Number(init.lineno) >>> 0;
    this.colno = Number(init.colno) >>> 0;
  }
}

export class ProgressEvent extends Event {
  readonly lengthComputable: boolean;
  readonly loaded: number;
  readonly total: number;
  constructor(type: string, init: EventInit & { lengthComputable?: boolean; loaded?: number; total?: number } = {}) {
    super(type, init);
    this.lengthComputable = Boolean(init.lengthComputable);
    this.loaded = Number(init.loaded ?? 0);
    this.total = Number(init.total ?? 0);
  }
}

export class CompositionEvent extends Event {
  readonly data: string;
  readonly view: EventTarget | null;
  constructor(type: string, init: EventInit & { data?: string; view?: EventTarget | null } = {}) {
    super(type, init);
    this.data = String(init.data ?? "");
    this.view = init.view ?? null;
  }
}

export function createLegacyEvent(interfaceName: string): Event {
  const name = String(interfaceName).toLowerCase();
  const constructors: Record<string, typeof Event> = {
    event: Event, events: Event, htmlevents: Event, mouseevent: MouseEvent, mouseevents: MouseEvent,
    customevent: CustomEvent, focusevent: FocusEvent, keyboardevent: KeyboardEvent, compositionevent: CompositionEvent,
  };
  if (!Object.hasOwn(constructors, name)) throw new DOMException("Unknown event interface", "NotSupportedError");
  const event = new constructors[name]("");
  events.get(event)!.initialized = false;
  return event;
}
