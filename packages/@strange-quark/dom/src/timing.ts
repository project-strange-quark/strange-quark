// SPDX-License-Identifier: MIT

import { DOMException, Event, EventTarget } from "./events.js";
import { installPerformance } from "./performance.js";

type Callback = (...args: unknown[]) => void;
type FrameCallback = (timestamp: number) => void;
type IdleCallback = (deadline: object) => void;
type TimingHost = {
  now(): number;
  armTimer(deadline: number | null): void;
  queueMicrotask(callback: () => void): void;
  postTask(callback: () => void): void;
  cloneMessage(data: unknown, transfers?: unknown[]): unknown;
};

type Timer = {
  id: number;
  deadline: number;
  order: number;
  nesting: number;
  delay: number;
  repeat: boolean;
  callback: () => void;
};

function before(left: Timer, right: Timer): boolean {
  return left.deadline < right.deadline || left.deadline === right.deadline && left.order < right.order;
}

class TimerHeap {
  #timers: Timer[] = [];

  peek(): Timer | undefined { return this.#timers[0]; }

  push(timer: Timer): void {
    const timers = this.#timers;
    let index = timers.length;
    timers.push(timer);
    while (index > 0) {
      const parent = (index - 1) >> 1;
      if (!before(timer, timers[parent])) break;
      timers[index] = timers[parent];
      index = parent;
    }
    timers[index] = timer;
  }

  pop(): Timer | undefined {
    const timers = this.#timers;
    const first = timers[0];
    const last = timers.pop();
    if (timers.length === 0) return first;

    let index = 0;
    while (index * 2 + 1 < timers.length) {
      let child = index * 2 + 1;
      if (child + 1 < timers.length && before(timers[child + 1], timers[child])) child++;
      if (!before(timers[child], last!)) break;
      timers[index] = timers[child];
      index = child;
    }
    timers[index] = last!;
    return first;
  }
}

type TimingCallbacks = {
  fireTimer(): void;
  beginFrame(): void;
  hasAnimationCallbacks(): boolean;
  nextAnimation(): FrameCallback | undefined;
  idleWakeup(): number | null;
  beginIdle(deadline: number): void;
  nextIdle(): (() => void) | undefined;
};


/** Installs timing globals, returning the callbacks driven by squark-js. */
export class MessageEvent extends Event {
  readonly data: unknown;
  readonly origin: string;
  readonly lastEventId: string;
  readonly source: unknown;
  readonly ports: readonly unknown[];

  constructor(type: string, init: {
    data?: unknown; origin?: string; lastEventId?: string; source?: unknown; ports?: Iterable<unknown>;
  } | null = {}) {
    init ??= {};
    super(type);
    this.data = "data" in init ? init.data : null;
    this.origin = String(init.origin ?? "");
    this.lastEventId = String(init.lastEventId ?? "");
    this.source = init.source ?? null;
    this.ports = Object.freeze([...(init.ports ?? [])]);
  }
}

export function installTiming(global: Record<PropertyKey, unknown>, host: TimingHost): TimingCallbacks {
  let nextTimer = 1;
  let order = 0;
  let nesting = 0;
  const timers = new Map<number, Timer>();
  const heap = new TimerHeap();

  function head(): Timer | undefined {
    let timer = heap.peek();
    while (timer !== undefined && timers.get(timer.id) !== timer) {
      heap.pop();
      timer = heap.peek();
    }
    return timer;
  }

  function arm(): void { host.armTimer(head()?.deadline ?? null); }

  function schedule(timer: Timer): void {
    const delay = timer.nesting > 6 ? Math.max(timer.delay, 4) : timer.delay;
    timer.deadline = host.now() + delay;
    timer.order = order++;
    timers.set(timer.id, timer);
    heap.push(timer);
    arm();
  }

  function createTimer(handler: Callback | string, delay: number, repeat: boolean, args: unknown[]): number {
    const action = typeof handler === "function"
      ? () => handler.apply(global, args)
      : () => globalEval(String(handler));
    const id = nextTimer++;
    schedule({ id, callback: action, delay: Math.max(delay | 0, 0), repeat,
               nesting: nesting + 1, deadline: 0, order: 0 });
    return id;
  }

  function globalEval(code: string): void {
    const evaluate = eval;
    evaluate(code);
  }

  function clearTimer(id: number): void {
    timers.delete(id >>> 0);
    arm();
  }

  // Internal timers take negative IDs, which clearTimeout cannot name.
  let nextInternalTimer = -1;
  function internalTimer(callback: () => void, delay: number): number {
    const id = nextInternalTimer--;
    schedule({ id, callback, delay, repeat: false, nesting: 0, deadline: 0, order: 0 });
    return id;
  }

  function fireTimer(): void {
    const timer = head();
    if (timer === undefined || timer.deadline > host.now()) {
      arm();
      return;
    }
    heap.pop();
    if (!timer.repeat) timers.delete(timer.id);
    nesting = timer.nesting;
    try {
      timer.callback();
    } finally {
      if (timer.repeat && timers.get(timer.id) === timer) {
        timer.nesting++;
        schedule(timer);
      }
      nesting = 0;
      arm();
    }
  }

  let nextFrame = 1;
  const frames = new Map<number, FrameCallback>();
  let frameHandles: number[] = [];
  let frameIndex = 0;

  function nextAnimation(): FrameCallback | undefined {
    while (frameIndex < frameHandles.length) {
      const id = frameHandles[frameIndex++];
      const callback = frames.get(id);
      if (callback !== undefined) {
        frames.delete(id);
        return callback;
      }
    }
    frameHandles = [];
    frameIndex = 0;
  }

  // requestIdleCallback: callbacks requested during an idle period wait for
  // the next one, which starts no earlier than the current one's deadline.
  type IdleRequest = { callback: IdleCallback; timeout: number | null };
  let nextIdle = 1;
  const idleRequests = new Map<number, IdleRequest>();
  const runnableIdle = new Map<number, IdleRequest>();
  let idleDeadline = 0;
  const deadlineToken = Symbol("IdleDeadline constructor");

  class IdleDeadline {
    readonly #deadline: number;
    readonly #didTimeout: boolean;
    constructor(token: symbol, deadline: number, didTimeout: boolean) {
      if (token !== deadlineToken) throw new TypeError("Illegal constructor");
      this.#deadline = deadline;
      this.#didTimeout = didTimeout;
    }
    timeRemaining(): number { return Math.max(0, this.#deadline - host.now()); }
    get didTimeout(): boolean { return this.#didTimeout; }
  }
  Object.defineProperty(IdleDeadline.prototype, Symbol.toStringTag, { value: "IdleDeadline", configurable: true });

  function takeIdle(id: number): IdleRequest | undefined {
    const request = idleRequests.get(id) ?? runnableIdle.get(id);
    idleRequests.delete(id);
    runnableIdle.delete(id);
    if (request?.timeout != null) {
      timers.delete(request.timeout);
      arm();
    }
    return request;
  }

  function requestIdleCallback(callback: IdleCallback, options?: { timeout?: number } | null): number {
    if (typeof callback !== "function") throw new TypeError("requestIdleCallback requires a callback");
    const id = nextIdle++;
    const timeout = Number(options?.timeout ?? 0) >>> 0;
    const request: IdleRequest = { callback, timeout: null };
    idleRequests.set(id, request);
    if (timeout > 0) {
      request.timeout = internalTimer(() => {
        if (takeIdle(id) !== undefined) callback.call(global, new IdleDeadline(deadlineToken, host.now(), true));
      }, timeout);
    }
    return id;
  }

  function cancelIdleCallback(id: number): void { takeIdle(id >>> 0); }

  function idleWakeup(): number | null {
    return idleRequests.size === 0 && runnableIdle.size === 0 ? null : idleDeadline;
  }

  // Start an idle period: callbacks left from the last one keep their place.
  function beginIdle(deadline: number): void {
    idleDeadline = deadline;
    for (const [id, request] of idleRequests) runnableIdle.set(id, request);
    idleRequests.clear();
  }

  function nextIdleCallback(): (() => void) | undefined {
    const first = runnableIdle.keys().next();
    if (first.done || host.now() >= idleDeadline) return undefined;
    const request = takeIdle(first.value)!;
    const deadline = new IdleDeadline(deadlineToken, idleDeadline, false);
    return () => request.callback.call(global, deadline);
  }

  type MessageHandler = ((event: MessageEvent) => void) | null;

  class MessagePort extends EventTarget {
    #peer: MessagePort | undefined;
    #started = false;
    #closed = false;
    #pending: unknown[] = [];
    #onmessage: MessageHandler = null;
    #handler = (event: Event) => this.#onmessage?.call(this, event as MessageEvent);

    private constructor() { super(); }

    static pair(): [MessagePort, MessagePort] {
      const first = new MessagePort();
      const second = new MessagePort();
      first.#peer = second;
      second.#peer = first;
      return [first, second];
    }

    get onmessage(): MessageHandler { return this.#onmessage; }
    set onmessage(value: MessageHandler) {
      const callback = typeof value === "function" ? value : null;
      if (this.#onmessage === null && callback !== null) this.addEventListener("message", this.#handler);
      if (this.#onmessage !== null && callback === null) this.removeEventListener("message", this.#handler);
      this.#onmessage = callback;
      this.start();
    }

    postMessage(data: unknown, options?: unknown[] | { transfer?: unknown[] }): void {
      if (arguments.length === 0) throw new TypeError("postMessage requires data");
      const transfer = Array.isArray(options) ? options : options?.transfer;
      if (transfer?.length) throw new DOMException("Message transfers are unavailable", "DataCloneError");
      const peer = this.#peer;
      if (peer === undefined) return;
      let message;
      try {
        message = host.cloneMessage(data);
      } catch (error) {
        if (error instanceof Error && error.name === "DataCloneError") {
          throw new DOMException(error.message, error.name);
        }
        throw error;
      }
      if (peer.#started) peer.#enqueue(message);
      else peer.#pending.push(message);
    }

    start(): void {
      if (this.#closed || this.#started) return;
      this.#started = true;
      for (const data of this.#pending) this.#enqueue(data);
      this.#pending = [];
    }

    close(): void {
      this.#closed = true;
      if (this.#peer !== undefined) this.#peer.#peer = undefined;
      this.#peer = undefined;
      this.#pending = [];
    }

    #enqueue(data: unknown): void {
      host.postTask(() => {
        if (!this.#closed) this.dispatchEvent(new MessageEvent("message", { data }));
      });
    }
  }

  class MessageChannel {
    readonly port1: MessagePort;
    readonly port2: MessagePort;
    constructor() { [this.port1, this.port2] = MessagePort.pair(); }
  }

  installPerformance(global, host);
  Object.assign(global, {
    setTimeout(handler: Callback | string, delay = 0, ...args: unknown[]) {
      if (arguments.length === 0) throw new TypeError("setTimeout requires a handler");
      return createTimer(handler, delay, false, args);
    },
    setInterval(handler: Callback | string, delay = 0, ...args: unknown[]) {
      if (arguments.length === 0) throw new TypeError("setInterval requires a handler");
      return createTimer(handler, delay, true, args);
    },
    clearTimeout: clearTimer,
    clearInterval: clearTimer,
    queueMicrotask: (callback: () => void) => host.queueMicrotask(callback),
    requestAnimationFrame(callback: FrameCallback) {
      if (typeof callback !== "function") throw new TypeError("requestAnimationFrame requires a callback");
      const id = nextFrame++;
      frames.set(id, (timestamp) => callback.call(global, timestamp));
      return id;
    },
    cancelAnimationFrame(id: number) { frames.delete(id >>> 0); },
    requestIdleCallback, cancelIdleCallback, IdleDeadline,
    structuredClone(value: unknown, options?: { transfer?: Iterable<unknown> }) {
      if (arguments.length === 0) throw new TypeError("structuredClone requires a value");
      const transfers = options?.transfer === undefined ? undefined : [...options.transfer];
      try { return host.cloneMessage(value, transfers); }
      catch (error) {
        if ((error as Error).name === "DataCloneError") throw new DOMException((error as Error).message, "DataCloneError");
        throw error;
      }
    },
    MessageChannel, MessagePort, MessageEvent,
  });

  return {
    fireTimer,
    hasAnimationCallbacks() { return frames.size !== 0; },
    beginFrame() {
      frameHandles = [...frames.keys()];
      frameIndex = 0;
    },
    nextAnimation,
    idleWakeup,
    beginIdle,
    nextIdle: nextIdleCallback,
  };
}
