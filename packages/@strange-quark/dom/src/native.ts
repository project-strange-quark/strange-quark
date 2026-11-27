// SPDX-License-Identifier: MIT

// Native modules (architecture §13): app Rust code linked into an app runtime,
// called by name. This is not a web API, so it is not a global name: it sits
// under `Symbol.for("strange-quark.native")`, where `@strange-quark/native` and
// `@strange-quark/tauri` find it. It exists only in the native runtime.

import { DOMException } from "./events.js";

type NativeHost = {
  nativeModules?: readonly string[];
  nativeInvoke?: (id: number, module: string, command: string, json: string) => void;
};

type WindowRequest = (op: string, value?: unknown) => Promise<unknown>;
type Listener = (payload: unknown) => void;

export const NATIVE_API = Symbol.for("strange-quark.native");

const windowRequests = new WeakMap<object, WindowRequest>();

/** installPlatform hands over its host-request channel for window operations. */
export function registerWindowRequests(global: object, request: WindowRequest): void {
  windowRequests.set(global, request);
}

function parse(json: string): unknown {
  return json === "" ? undefined : JSON.parse(json);
}

/**
 * Install the native API when the runtime has native modules or window
 * operations. Returns the dispatcher squark-js calls with
 * `["reply", id, ok, json]` and `["event", name, json]` messages.
 */
export function installNative(global: Record<PropertyKey, unknown>, host: NativeHost): ((message: unknown[]) => void) | undefined {
  const windowRequest = windowRequests.get(global);
  const invokeBinding = host.nativeInvoke;
  if (windowRequest === undefined && invokeBinding === undefined) return undefined;
  const report = (error: unknown) => {
    const reportError = global.reportError as ((error: unknown) => void) | undefined;
    if (reportError !== undefined) reportError(error);
  };
  const modules = Object.freeze([...(host.nativeModules ?? [])]);
  const pending = new Map<number, { resolve: (value: unknown) => void; reject: (reason: unknown) => void }>();
  const listeners = new Map<string, Set<Listener>>();
  let next = 0;

  const windowOp = (op: string, value?: unknown): Promise<unknown> =>
    windowRequest === undefined
      ? Promise.reject(new DOMException("Window operations are unavailable", "NotSupportedError"))
      : windowRequest(op, value);

  const api = Object.freeze({
    modules,
    invoke(module: string, command: string, args: unknown = {}): Promise<unknown> {
      module = String(module);
      if (invokeBinding === undefined || !modules.includes(module)) {
        return Promise.reject(`native module "${module}" is not available`);
      }
      let json: string;
      try {
        json = JSON.stringify(args ?? {}) ?? "{}";
      } catch (error) {
        return Promise.reject(error);
      }
      return new Promise((resolve, reject) => {
        const id = ++next;
        if (id > 0xffffffff) throw new RangeError("Native call ID space exhausted");
        pending.set(id, { resolve, reject });
        try {
          invokeBinding(id, module, String(command), json);
        } catch (error) {
          pending.delete(id);
          reject(error);
        }
      });
    },
    listen(event: string, callback: Listener): () => void {
      event = String(event);
      let set = listeners.get(event);
      if (set === undefined) listeners.set(event, (set = new Set()));
      const entry: Listener = payload => callback(payload);
      set.add(entry);
      return () => {
        const current = listeners.get(event);
        current?.delete(entry);
        if (current?.size === 0) listeners.delete(event);
      };
    },
    window: Object.freeze({
      setTitle: (title: string) => windowOp("title", String(title)),
      setZoom: (factor: number) => windowOp("zoom", Number(factor)),
      close: () => windowOp("close"),
      relaunch: () => windowOp("relaunch"),
      exit: (code = 0) => windowOp("exit", Number(code) | 0),
    }),
  });
  Object.defineProperty(global, NATIVE_API, { value: api, enumerable: false, writable: false, configurable: false });

  return (message: unknown[]) => {
    const [kind, first, second, third] = message;
    if (kind === "reply") {
      const entry = pending.get(first as number);
      pending.delete(first as number);
      if (entry === undefined) return;
      let value: unknown;
      try {
        value = parse(third as string);
      } catch (error) {
        entry.reject(error);
        return;
      }
      if (second) entry.resolve(value);
      else entry.reject(value);
    } else if (kind === "event") {
      const set = listeners.get(first as string);
      if (set === undefined) return;
      let payload: unknown;
      try {
        payload = parse(second as string);
      } catch (error) {
        report(error);
        return;
      }
      for (const listener of [...set]) {
        try {
          listener(payload);
        } catch (error) {
          report(error);
        }
      }
    }
  };
}
