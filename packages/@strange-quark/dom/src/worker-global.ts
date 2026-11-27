// SPDX-License-Identifier: MIT

// The global scope of a dedicated worker. squark-js loads this module, not
// index.js, as a worker realm's runtime entry, so a worker never loads the
// DOM, the journal encoder or the engine bindings: only the APIs a
// DedicatedWorkerGlobalScope has.

import { AbortController, AbortSignal } from "./abort.js";
import { installConsole } from "./console.js";
import {
  CustomEvent, dispatchTrustedEvent, DOMException, ErrorEvent, Event, EventTarget, installWindowEventTarget, ProgressEvent,
} from "./events.js";
import { installFiles } from "./file-reader.js";
import { reflectedEvent } from "./handlers.js";
import { navigatorIdentity } from "./navigator-identity.js";
import type { HostArch, HostOS } from "./navigator-identity.js";
import { installTiming, MessageEvent } from "./timing.js";
import { installTransport } from "./transport.js";
import type { TransportHost } from "./transport.js";
import { makeURL, URLSearchParams } from "./url.js";
import type { URLHost } from "./url.js";
import { installWasm } from "./wasm.js";
import { installCrypto } from "./web-crypto.js";
import { cloneError, transferList } from "./worker.js";

type WorkerHost = TransportHost & URLHost & Parameters<typeof installTiming>[1] & Parameters<typeof installFiles>[1] &
  Parameters<typeof installCrypto>[1] & Parameters<typeof installWasm>[1] & Parameters<typeof installConsole>[1] & {
    os?: HostOS;
    arch?: HostArch;
    hardwareConcurrency: number;
    workerPost(message: unknown, transfer: ArrayBuffer[]): void;
    workerClose(): void;
    workerReport(error: unknown): void;
    loadScript(url: string): [url: string, source: string] | undefined;
    runClassicScript(source: string, url: string): void;
  };

type WorkerOptions = { url: string; name: string; module: boolean };
type ErrorHandler = (event: Event | string, source?: string, line?: number, column?: number, error?: unknown) => unknown;

/** Turns the realm's global into a DedicatedWorkerGlobalScope and returns the callbacks squark-js drives. */
export function installWorkerGlobal(global: Record<PropertyKey, unknown>, host: WorkerHost, options: WorkerOptions) {
  const location = host.parseURL(options.url);
  if (location === undefined) throw new TypeError("Invalid worker URL");
  const secret = Symbol("worker constructor");

  class WorkerGlobalScope extends EventTarget {
    protected constructor() {
      super();
      throw new TypeError("Illegal constructor");
    }
  }
  class DedicatedWorkerGlobalScope extends WorkerGlobalScope {
    private constructor() { super(); }
    get name(): string { return options.name; }
    get [Symbol.toStringTag](): string { return "DedicatedWorkerGlobalScope"; }
  }

  let onerror: { callback: ErrorHandler; listener: (event: Event) => void } | undefined;
  Object.defineProperty(WorkerGlobalScope.prototype, "onerror", {
    configurable: true, enumerable: true,
    get: () => onerror?.callback ?? null,
    set(callback: ErrorHandler | null) {
      if (typeof callback !== "function") {
        if (onerror !== undefined) target.removeEventListener("error", onerror.listener);
        onerror = undefined;
      } else if (onerror !== undefined) {
        onerror.callback = callback;
      } else {
        const listener = (event: Event) => {
          const handle = onerror!.callback;
          if (event instanceof ErrorEvent) {
            if (handle.call(global, event.message, event.filename, event.lineno, event.colno, event.error) === true) event.preventDefault();
          } else if (handle.call(global, event) === false) {
            event.preventDefault();
          }
        };
        onerror = { callback, listener };
        target.addEventListener("error", listener);
      }
    },
  });
  for (const type of ["message", "messageerror"]) reflectedEvent(DedicatedWorkerGlobalScope.prototype, type);
  const target = installWindowEventTarget(global, DedicatedWorkerGlobalScope.prototype);

  class WorkerLocation {
    constructor(token: symbol) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get href(): string { return location!.href; }
    get origin(): string { return location!.origin; }
    get protocol(): string { return location!.protocol; }
    get host(): string { return location!.host; }
    get hostname(): string { return location!.hostname; }
    get port(): string { return location!.port; }
    get pathname(): string { return location!.pathname; }
    get search(): string { return location!.search; }
    get hash(): string { return location!.hash; }
    toString(): string { return location!.href; }
    get [Symbol.toStringTag](): string { return "WorkerLocation"; }
  }

  const identity = navigatorIdentity(host.os, host.arch);
  class WorkerNavigator {
    readonly userAgent = identity.userAgent;
    readonly platform = identity.platform;
    readonly language = "en-US";
    readonly languages = Object.freeze([this.language]);
    readonly hardwareConcurrency = host.hardwareConcurrency;
    readonly onLine = true;
    constructor(token: symbol) { if (token !== secret) throw new TypeError("Illegal constructor"); }
    get [Symbol.toStringTag](): string { return "WorkerNavigator"; }
  }

  const timing = installTiming(global, host);
  installConsole(global as Record<string, unknown>, host);
  Object.assign(global, { URL: makeURL(host), URLSearchParams });
  const network = installTransport(global, host, () => location.href);
  installFiles(global, host);
  installCrypto(global, host);
  installWasm(global, host);

  function postMessage(message: unknown, transfer?: unknown): void {
    if (arguments.length === 0) {
      throw new TypeError("Failed to execute 'postMessage' on 'DedicatedWorkerGlobalScope': 1 argument required, but only 0 present.");
    }
    const buffers = transferList(transfer, "DedicatedWorkerGlobalScope", global.MessagePort);
    try { host.workerPost(message, buffers); }
    catch (error) { throw cloneError(error, "DedicatedWorkerGlobalScope"); }
  }

  function importScripts(...urls: unknown[]): void {
    const failure = (message: string, name: string) =>
      new DOMException(`Failed to execute 'importScripts' on 'WorkerGlobalScope': ${message}`, name);
    if (options.module) {
      throw new TypeError("Failed to execute 'importScripts' on 'WorkerGlobalScope': Module scripts don't support importScripts().");
    }
    const resolved = urls.map(url => {
      const parsed = host.parseURL(String(url), location!.href);
      if (parsed === undefined) throw failure(`The URL '${String(url)}' is invalid.`, "SyntaxError");
      return parsed.href;
    });
    for (const url of resolved) {
      const script = host.loadScript(url);
      if (script === undefined) throw failure(`The script at '${url}' failed to load.`, "NetworkError");
      host.runClassicScript(script[1], script[0]);
    }
  }

  function reportError(error: unknown): void {
    if (arguments.length === 0) {
      throw new TypeError("Failed to execute 'reportError' on 'WorkerGlobalScope': 1 argument required, but only 0 present.");
    }
    host.workerReport(error);
  }

  Object.assign(global, {
    self: global,
    location: new WorkerLocation(secret),
    navigator: new WorkerNavigator(secret),
    postMessage,
    close: () => host.workerClose(),
    importScripts,
    reportError,
    WorkerGlobalScope, DedicatedWorkerGlobalScope, WorkerLocation, WorkerNavigator,
    EventTarget, Event, CustomEvent, ErrorEvent, ProgressEvent, DOMException, AbortController, AbortSignal,
  });

  let reporting = false;
  return {
    timing,
    network: network.dispatch,
    fetchModule: network.fetchModule,
    message(data: unknown): void { dispatchTrustedEvent(target, new MessageEvent("message", { data })); },
    messageError(): void { dispatchTrustedEvent(target, new MessageEvent("messageerror")); },
    /** Fires `error` at the worker global; true when no handler canceled it. */
    report(error: unknown, filename: string, lineno: number, colno: number, message: string): boolean {
      if (reporting) return true;
      reporting = true;
      try {
        return dispatchTrustedEvent(target, new ErrorEvent("error", { cancelable: true, error, message, filename, lineno, colno }));
      } finally {
        reporting = false;
      }
    },
  };
}
