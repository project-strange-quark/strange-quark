// SPDX-License-Identifier: MIT

// Tauri 2 frontend compatibility. `@tauri-apps/api` and the official plugins
// reach their backend only through `window.__TAURI_INTERNALS__` (`invoke`,
// `transformCallback`, `convertFileSrc`, `metadata`) and
// `window.__TAURI_EVENT_PLUGIN_INTERNALS__`, the same surface the API's own
// `mocks` module replaces. This file provides that surface over a transport:
// Strange Quark native modules in a real app, or canned replies (a fixture) to
// run a frontend with no backend at all.
//
// The file has no imports, so `scripts/classic.mjs` can turn it into the
// classic script that `squark build` and `squark dev` run before the app.

/** Where app commands and backend events go. */
export interface TauriTransport {
  /** Run an app command. Rejections carry the command's error value. */
  invoke(command: string, args: Record<string, unknown>): Promise<unknown>;
  /** Receive backend events by name; returns an unsubscribe function. */
  listen(event: string, deliver: (payload: unknown) => void): () => void;
}

/** Window and process operations the plugins need from the host. */
export interface TauriHost {
  setZoom?(factor: number): void;
  relaunch?(): void;
  exit?(code: number): void;
  openExternal?(url: string): void;
}

export interface TauriShimOptions {
  transport: TauriTransport;
  host?: TauriHost;
  /** The window and webview label Tauri apps see; Tauri's default is "main". */
  label?: string;
}

/** The private native API squark-dom installs when the pack lists native modules. */
export interface NativeApi {
  readonly modules: readonly string[];
  invoke(module: string, command: string, args: unknown): Promise<unknown>;
  listen(event: string, callback: (payload: unknown) => void): () => void;
  readonly window: {
    setZoom(factor: number): void;
    relaunch(): void;
    exit(code?: number): void;
  };
}

export const NATIVE_API = Symbol.for("strange-quark.native");

type Callback = (data: unknown) => unknown;
type Clipboard = { readText(): Promise<string>; writeText(text: string): Promise<void> };
type ShimGlobal = typeof globalThis & {
  __TAURI_INTERNALS__?: Record<string, unknown>;
  __TAURI_EVENT_PLUGIN_INTERNALS__?: Record<string, unknown>;
  isTauri?: boolean;
  navigator?: { clipboard?: Clipboard; platform?: string; userAgent?: string };
  open?: (url: string, target?: string) => unknown;
  crypto?: { getRandomValues<T extends ArrayBufferView>(array: T): T };
};

function unavailable(command: string): Promise<never> {
  return Promise.reject(`${command} is not available in Strange Quark`);
}

function isWindows(global: ShimGlobal): boolean {
  const platform = global.navigator?.platform ?? "";
  return platform.startsWith("Win");
}

/**
 * Install `__TAURI_INTERNALS__` on `global`. Call it before any app module
 * loads, because the official plugins capture nothing but `invoke` itself.
 */
export function installTauri(global: ShimGlobal, options: TauriShimOptions): void {
  const { transport, host = {} } = options;
  const label = options.label ?? "main";
  const callbacks = new Map<number, Callback>();
  // eventId → [event, unsubscribe]; frontend-emitted events go to `local`.
  const listeners = new Map<number, [string, () => void]>();
  const local = new Map<string, Set<(payload: unknown) => void>>();
  let nextEvent = 1;

  // Capture the native clipboard before the app can replace navigator.clipboard
  // with calls to the clipboard plugin, which lands back here.
  const nativeClipboard = global.navigator?.clipboard;
  const clipboard: Clipboard | undefined = nativeClipboard
    ? {
        readText: nativeClipboard.readText.bind(nativeClipboard),
        writeText: nativeClipboard.writeText.bind(nativeClipboard),
      }
    : undefined;
  const openExternal =
    host.openExternal ?? ((url: string) => void global.open?.(url, "_blank"));

  const randomId = () => {
    let id = 0;
    do {
      id = global.crypto ? global.crypto.getRandomValues(new Uint32Array(1))[0]! : Math.floor(Math.random() * 0xffffffff);
    } while (id === 0 || callbacks.has(id));
    return id;
  };
  const transformCallback = (callback?: Callback, once = false): number => {
    const id = randomId();
    callbacks.set(id, data => {
      if (once) callbacks.delete(id);
      return callback?.(data);
    });
    return id;
  };
  const unregisterCallback = (id: number) => void callbacks.delete(id);
  const runCallback = (id: number, data: unknown) => {
    const callback = callbacks.get(id);
    if (callback) callback(data);
  };

  const unlisten = (eventId: number) => {
    const entry = listeners.get(eventId);
    if (!entry) return;
    listeners.delete(eventId);
    entry[1]();
  };
  const listen = (event: string, handler: number): number => {
    const eventId = nextEvent++;
    const deliver = (payload: unknown) => runCallback(handler, { event, id: eventId, payload });
    const unsubscribeBackend = transport.listen(event, deliver);
    let set = local.get(event);
    if (!set) local.set(event, (set = new Set()));
    set.add(deliver);
    listeners.set(eventId, [
      event,
      () => {
        unsubscribeBackend();
        local.get(event)?.delete(deliver);
      },
    ]);
    return eventId;
  };
  const emitLocal = (event: string, payload: unknown) => {
    for (const deliver of [...(local.get(event) ?? [])]) deliver(payload);
  };

  const plugin = (name: string, command: string, args: Record<string, unknown>): Promise<unknown> => {
    const full = `plugin:${name}|${command}`;
    switch (name) {
      case "event":
        switch (command) {
          case "listen":
            return Promise.resolve(listen(String(args.event), Number(args.handler)));
          case "unlisten":
            unlisten(Number(args.eventId));
            return Promise.resolve();
          case "emit":
          case "emit_to":
            emitLocal(String(args.event), args.payload);
            return Promise.resolve();
        }
        break;
      case "clipboard-manager":
        if (!clipboard) return unavailable(full);
        switch (command) {
          case "read_text":
            return clipboard.readText();
          case "write_text":
            return clipboard.writeText(String(args.text ?? ""));
          case "write_html":
            return clipboard.writeText(String(args.altText ?? args.html ?? ""));
          case "clear":
            return clipboard.writeText("");
        }
        break;
      case "opener":
        if (command === "open_url") {
          openExternal(String(args.url));
          return Promise.resolve();
        }
        break;
      case "webview":
        if (command === "set_webview_zoom" && host.setZoom) {
          host.setZoom(Number(args.value));
          return Promise.resolve();
        }
        break;
      case "process":
        if (command === "restart" && host.relaunch) {
          host.relaunch();
          return new Promise(() => {});
        }
        if (command === "exit" && host.exit) {
          host.exit(Number(args.code ?? 0));
          return new Promise(() => {});
        }
        break;
    }
    return unavailable(full);
  };

  const invoke = (command: string, args: unknown = {}, _options?: unknown): Promise<unknown> => {
    const record = (args ?? {}) as Record<string, unknown>;
    const match = /^plugin:([^|]+)\|(.+)$/.exec(command);
    if (match) return plugin(match[1]!, match[2]!, record);
    try {
      return transport.invoke(command, record);
    } catch (error) {
      return Promise.reject(error);
    }
  };

  const convertFileSrc = (filePath: string, protocol = "asset"): string => {
    const path = encodeURIComponent(filePath);
    return isWindows(global) ? `http://${protocol}.localhost/${path}` : `${protocol}://localhost/${path}`;
  };

  const windows = isWindows(global);
  global.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback,
    unregisterCallback,
    runCallback,
    callbacks,
    convertFileSrc,
    metadata: {
      currentWindow: { label },
      currentWebview: { windowLabel: label, label },
    },
    plugins: { path: { sep: windows ? "\\" : "/", delimiter: windows ? ";" : ":" } },
  };
  global.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event: string, eventId: number) => unlisten(eventId),
  };
  global.isTauri = true;
}

/** Commands go to one native module; its events arrive through the native API. */
export function nativeTransport(global: object, module: string): TauriTransport {
  const api = (global as Record<symbol, NativeApi | undefined>)[NATIVE_API];
  if (!api) {
    const missing = () => Promise.reject(`native module "${module}" is not available: this runtime has no native modules`);
    return { invoke: missing, listen: () => () => {} };
  }
  return {
    invoke: (command, args) => api.invoke(module, command, args),
    listen: (event, deliver) => api.listen(event, deliver),
  };
}

/** Host operations backed by the native API's window object, when present. */
export function nativeHost(global: object): TauriHost {
  const api = (global as Record<symbol, NativeApi | undefined>)[NATIVE_API];
  if (!api) return {};
  return {
    setZoom: factor => api.window.setZoom(factor),
    relaunch: () => api.window.relaunch(),
    exit: code => api.window.exit(code),
  };
}

export type FixtureHandler = (args: Record<string, unknown>) => unknown;

export interface FixtureTransport extends TauriTransport {
  /** Deliver a backend event to frontend listeners, as the backend would. */
  emit(event: string, payload: unknown): void;
}

/**
 * Canned replies for running a frontend with no backend. A command without a
 * handler resolves with `undefined`; a handler that throws rejects the call.
 */
export function fixtureTransport(handlers: Record<string, FixtureHandler | unknown> = {}): FixtureTransport {
  const events = new Map<string, Set<(payload: unknown) => void>>();
  return {
    invoke(command, args) {
      if (!Object.hasOwn(handlers, command)) return Promise.resolve(undefined);
      const handler = handlers[command];
      try {
        return Promise.resolve(typeof handler === "function" ? (handler as FixtureHandler)(args) : handler);
      } catch (error) {
        return Promise.reject(error);
      }
    },
    listen(event, deliver) {
      let set = events.get(event);
      if (!set) events.set(event, (set = new Set()));
      set.add(deliver);
      return () => void set.delete(deliver);
    },
    emit(event, payload) {
      for (const deliver of [...(events.get(event) ?? [])]) deliver(payload);
    },
  };
}

/** Configuration `squark build` and `squark dev` write before the classic script. */
export interface TauriConfig {
  /** The native module app commands go to. */
  module?: string;
  /** Use `globalThis.__STRANGE_QUARK_TAURI_FIXTURE__` replies instead of native modules. */
  fixture?: boolean;
  label?: string;
}

/** Entry point of the classic script: read the injected config and install. */
export function installTauriFromConfig(global: ShimGlobal & Record<string, unknown>): void {
  const config = (global.__STRANGE_QUARK_TAURI__ ?? {}) as TauriConfig;
  let transport: TauriTransport;
  if (config.fixture) {
    const fixture = fixtureTransport((global.__STRANGE_QUARK_TAURI_FIXTURE__ ?? {}) as Record<string, unknown>);
    global.__STRANGE_QUARK_TAURI_EMIT__ = fixture.emit;
    transport = fixture;
  } else {
    transport = nativeTransport(global, config.module ?? "app");
  }
  installTauri(global, { transport, host: nativeHost(global), label: config.label });
}
