// SPDX-License-Identifier: MIT
// Pure helpers for the squark dev hand-off: the `/__squark/dev-manifest.json` served
// by the plugin and the `dev` block `squark dev` copies into the pack manifest.

/** Where the runtime loads the Vite client and the React Fast Refresh runtime. */
export const VITE_CLIENT_PATH = "/@vite/client";
export const REACT_REFRESH_PATH = "/@react-refresh";

/** The dev-server description served at `/__squark/dev-manifest.json`. */
export interface SquarkDevManifest {
  /** Format version, currently 1. */
  version: 1;
  /** App name, from `squark.config.ts` or `package.json`. */
  appName: string;
  /** HTTP origin of the running dev server, without a trailing slash. */
  origin: string;
  /** WebSocket URL Vite's HMR client connects to. */
  hmr: string;
  /** First application module script in `index.html` (e.g. `/src/main.tsx`), if any. */
  entry: string | null;
  /** Vite client module that owns HMR. */
  client: string;
  /** React Fast Refresh runtime module injected by `@vitejs/plugin-react`. */
  refresh: string;
  /** Vite root (absolute), for diagnostics. */
  root: string;
  /** Vite version, for diagnostics. */
  vite: string;
}

export interface CreateDevManifestOptions {
  appName: string;
  origin: string;
  entry: string | null;
  root: string;
  vite: string;
  /** Set when `server.hmr` pins host, port or path. */
  hmr?: { host?: string; port?: number; path?: string } | null;
  /** Vite's `base`; defaults to `/`. */
  base?: string;
}

function stripTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

/** Builds the WebSocket URL Vite's HMR client will use. */
export function hmrUrl(
  origin: string,
  hmr: { host?: string; port?: number; path?: string } | null | undefined,
  base = "/",
): string {
  const url = new URL(origin);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (hmr?.path) {
    url.pathname = hmr.path;
    url.search = "";
  } else {
    url.pathname = base.startsWith("/") ? base : `/${base}`;
  }
  return url.toString();
}

/** Creates the manifest the plugin serves; kept separate so tests can pin its shape. */
export function createDevManifest(options: CreateDevManifestOptions): SquarkDevManifest {
  const origin = stripTrailingSlash(options.origin);
  return {
    version: 1,
    appName: options.appName,
    origin,
    hmr: hmrUrl(origin, options.hmr, options.base),
    entry: options.entry,
    client: VITE_CLIENT_PATH,
    refresh: REACT_REFRESH_PATH,
    root: options.root,
    vite: options.vite,
  };
}

const SCRIPT_TAG = /<script\b([^>]*)>/gi;
const ATTRIBUTE = /([a-zA-Z_:][-a-zA-Z0-9_:.]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g;

/** Attributes of every `<script>` tag, lower-cased and unquoted. */
export function scriptAttributes(html: string): Record<string, string>[] {
  const tags: Record<string, string>[] = [];
  for (const tag of html.matchAll(SCRIPT_TAG)) {
    const attrs: Record<string, string> = {};
    for (const attr of tag[1].matchAll(ATTRIBUTE)) {
      attrs[attr[1].toLowerCase()] = attr[2] ?? attr[3] ?? attr[4] ?? "";
    }
    tags.push(attrs);
  }
  return tags;
}

/**
 * Picks the first application module script from `index.html`, skipping the
 * Vite client and the React Fast Refresh preamble.
 */
export function extractEntryModule(html: string): string | null {
  for (const attrs of scriptAttributes(html)) {
    if ((attrs.type ?? "").trim().toLowerCase() !== "module") continue;
    const src = (attrs.src ?? "").trim();
    if (!src) continue;
    if (src === VITE_CLIENT_PATH || src === REACT_REFRESH_PATH) continue;
    return src;
  }
  return null;
}

/** True when the resolved plugin list already carries a React plugin. */
export function hasReactPlugin(plugins: unknown): boolean {
  const stack: unknown[] = [plugins];
  while (stack.length > 0) {
    const item = stack.pop();
    if (Array.isArray(item)) {
      stack.push(...item);
      continue;
    }
    if (item === null || typeof item !== "object") continue;
    const name = (item as { name?: unknown }).name;
    if (typeof name === "string" && /^vite:react(-|$)/.test(name)) return true;
  }
  return false;
}
