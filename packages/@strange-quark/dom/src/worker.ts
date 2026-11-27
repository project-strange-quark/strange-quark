// SPDX-License-Identifier: MIT

// The document's dedicated Worker. squark-js runs each worker in its own V8
// isolate on its own thread, carries messages between the isolates as V8
// structured-clone bytes and returns the worker's events as packets through
// the function `installWorkers` returns.

import { dispatchTrustedEvent, DOMException, ErrorEvent, Event, EventTarget } from "./events.js";
import { reflectedEvent } from "./handlers.js";
import { MessageEvent } from "./timing.js";
import type { ParsedURL } from "./url.js";

type WorkerHost = {
  workerStart?(id: number, url: string, module: boolean, name: string): void;
  workerPost(id: number, message: unknown, transfer: ArrayBuffer[]): void;
  workerTerminate(id: number): void;
  parseURL(input: string, base?: string): ParsedURL | undefined;
  logError?(message: string): void;
};

type ErrorDetails = [message: string, filename: string, lineno: number, colno: number];
export type WorkerPacket =
  | [id: number, kind: "message", data: unknown]
  | [id: number, kind: "error", details: ErrorDetails]
  | [id: number, kind: "messageerror" | "failed" | "closed"];

type WorkerOptions = { type?: unknown; name?: unknown };

/**
 * Validates postMessage's transfer argument as Chromium does. ArrayBuffers
 * move to the other thread; MessagePorts stay in their realm.
 */
export function transferList(options: unknown, owner: string, MessagePort: unknown): ArrayBuffer[] {
  const fail = (message: string) => new DOMException(`Failed to execute 'postMessage' on '${owner}': ${message}`, "DataCloneError");
  let items: unknown[] = [];
  if (typeof options === "object" && options !== null) {
    if (Symbol.iterator in options) items = [...options as Iterable<unknown>];
    else {
      const transfer = (options as { transfer?: Iterable<unknown> }).transfer;
      if (transfer !== undefined) items = [...transfer];
    }
  } else if (options !== undefined && options !== null) {
    throw new TypeError(`Failed to execute 'postMessage' on '${owner}': The provided value is not of type '(sequence<object> or StructuredSerializeOptions)'.`);
  }
  const buffers: ArrayBuffer[] = [];
  items.forEach((item, index) => {
    if (item instanceof ArrayBuffer) {
      if (buffers.includes(item)) throw fail(`ArrayBuffer at index ${index} is a duplicate of an earlier ArrayBuffer.`);
      if (item.detached) throw fail(`ArrayBuffer at index ${index} is already detached.`);
      buffers.push(item);
    } else if (typeof MessagePort === "function" && item instanceof MessagePort) {
      throw fail(`MessagePort at index ${index} cannot be transferred to another thread.`);
    } else {
      throw fail(`Value at index ${index} does not have a transferable type.`);
    }
  });
  return buffers;
}

/** V8's serializer throws a plain Error named DataCloneError. */
export function cloneError(error: unknown, owner: string): unknown {
  if (error instanceof Error && error.name === "DataCloneError") {
    return new DOMException(`Failed to execute 'postMessage' on '${owner}': ${error.message}`, "DataCloneError");
  }
  return error;
}

/**
 * Defines `Worker` when the host can start workers, and returns the function
 * that delivers their packets. Without a host there is no Worker, so feature
 * detection sees a browser without workers.
 */
export function installWorkers(global: Record<PropertyKey, unknown>, document: { readonly URL: string }, host: WorkerHost): ((packet: WorkerPacket) => void) | undefined {
  if (typeof host.workerStart !== "function") return undefined;
  const workers = new Map<number, Worker>();
  let nextId = 0;

  class Worker extends EventTarget {
    #id: number;

    constructor(url: string | { toString(): string }, options?: WorkerOptions | null) {
      if (arguments.length === 0) throw new TypeError("Failed to construct 'Worker': 1 argument required, but only 0 present.");
      const type = options?.type === undefined ? "classic" : String(options.type);
      if (type !== "classic" && type !== "module") {
        throw new TypeError(`Failed to construct 'Worker': Failed to read the 'type' property from 'WorkerOptions': The provided value '${type}' is not a valid enum value of type WorkerType.`);
      }
      const name = options?.name === undefined ? "" : String(options.name);
      const parsed = host.parseURL(String(url), document.URL);
      if (parsed === undefined) throw new DOMException(`Failed to construct 'Worker': '${String(url)}' is not a valid URL.`, "SyntaxError");
      const origin = host.parseURL(document.URL)?.origin;
      const sameOrigin = ["http:", "https:", "blob:"].includes(parsed.protocol) && parsed.origin === origin;
      if (!sameOrigin && parsed.protocol !== "data:") {
        throw new DOMException(`Failed to construct 'Worker': Script at '${parsed.href}' cannot be accessed from origin '${origin}'.`, "SecurityError");
      }
      super();
      if (nextId === 0xffffffff) throw new RangeError("Worker ID space exhausted");
      this.#id = ++nextId;
      workers.set(this.#id, this);
      host.workerStart!(this.#id, parsed.href, type === "module", name);
    }

    postMessage(message: unknown, options?: unknown): void {
      if (arguments.length === 0) throw new TypeError("Failed to execute 'postMessage' on 'Worker': 1 argument required, but only 0 present.");
      const transfer = transferList(options, "Worker", global.MessagePort);
      try { host.workerPost(this.#id, message, transfer); }
      catch (error) { throw cloneError(error, "Worker"); }
    }

    terminate(): void {
      if (workers.delete(this.#id)) host.workerTerminate(this.#id);
    }
  }
  // Chromium's Worker has onmessage and onerror, but no onmessageerror.
  for (const type of ["message", "error"]) reflectedEvent(Worker.prototype, type);
  Object.assign(global, { Worker });

  return function dispatch(packet: WorkerPacket): void {
    const worker = workers.get(packet[0]);
    if (worker === undefined) return;
    switch (packet[1]) {
      case "message":
        dispatchTrustedEvent(worker, new MessageEvent("message", { data: packet[2] }));
        break;
      case "messageerror":
        dispatchTrustedEvent(worker, new MessageEvent("messageerror"));
        break;
      case "error": {
        // An error the worker did not handle. If the page does not handle it
        // on the Worker either, it reaches the window, as in Chromium.
        const [message, filename, lineno, colno] = packet[2];
        const init = { cancelable: true, message, filename, lineno, colno };
        if (dispatchTrustedEvent(worker, new ErrorEvent("error", init)) &&
            dispatchTrustedEvent(global as unknown as EventTarget, new ErrorEvent("error", init))) {
          host.logError?.(message);
        }
        break;
      }
      case "failed":
        // The script did not load or did not parse as a module.
        dispatchTrustedEvent(worker, new Event("error", { cancelable: true }));
        break;
      case "closed":
        workers.delete(packet[0]);
        break;
    }
  };
}
