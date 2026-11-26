// SPDX-License-Identifier: MIT
// `@strange-quark/vite-plugin`: the Vite side of `squark dev`.
//
// Vite keeps doing the transforming. What this plugin adds is the hand-off the
// native runtime needs:
//   * stock React Fast Refresh (through `@vitejs/plugin-react`) when the app
//     does not already install a React plugin;
//   * `/__squark/dev-manifest.json`, the origin, entry module and HMR URL the
//     launcher copies into the pack manifest;
//   * `/__squark/health`, a readiness probe for `squark dev`.
//
// The pack manifest the runtime reads is written by `squark dev`; the two shapes
// stay in sync through `createDevManifest()` in ./manifest.ts and
// `DevManifest` in crates/squark-cli/src/dev.rs.

import fs from "node:fs";
import path from "node:path";

import { version as viteVersion } from "vite";
import type { Plugin, ResolvedConfig, ViteDevServer } from "vite";

import {
  createDevManifest,
  extractEntryModule,
  hasReactPlugin,
  type SquarkDevManifest,
} from "./manifest.js";

export {
  VITE_CLIENT_PATH,
  REACT_REFRESH_PATH,
  createDevManifest,
  extractEntryModule,
  hmrUrl,
  scriptAttributes,
  hasReactPlugin,
  type SquarkDevManifest,
} from "./manifest.js";

/** Path the plugin serves with the dev hand-off. */
export const DEV_MANIFEST_PATH = "/__squark/dev-manifest.json";
/** Readiness probe the launcher polls before it writes the dev pack. */
export const DEV_HEALTH_PATH = "/__squark/health";

export interface StrangeQuarkPluginOptions {
  /** Override the app name reported to the runtime (defaults to package.json). */
  name?: string;
}

function readPackageName(root: string): string | null {
  try {
    const name = (JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")) as {
      name?: unknown;
    }).name;
    return typeof name === "string" && name.trim() !== "" ? name.trim() : null;
  } catch {
    return null;
  }
}

function isReactApp(root: string): boolean {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")) as {
      dependencies?: Record<string, unknown>;
      devDependencies?: Record<string, unknown>;
    };
    return Boolean(pkg.dependencies?.react ?? pkg.devDependencies?.react);
  } catch {
    return false;
  }
}

function directoryName(root: string): string {
  const parts = root.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || "squark-app";
}

export function strangeQuark(options: StrangeQuarkPluginOptions = {}): Plugin {
  let config: ResolvedConfig | null = null;
  let entry: string | null = null;
  let appName: string | null = null;

  const manifestFor = (server: ViteDevServer): SquarkDevManifest => {
    const resolved = config ?? server.config;
    const origin =
      server.resolvedUrls?.local[0] ??
      `http://${resolved.server.host ?? "127.0.0.1"}:${resolved.server.port ?? 5173}`;
    const hmr = resolved.server.hmr;
    return createDevManifest({
      appName: appName ?? options.name ?? directoryName(resolved.root),
      origin,
      entry,
      root: resolved.root,
      vite: viteVersion,
      base: resolved.base,
      hmr:
        hmr && typeof hmr === "object"
          ? { host: hmr.host, port: hmr.port, path: hmr.path }
          : null,
    });
  };

  return {
    name: "strange-quark:vite-plugin",
    apply: "serve",
    configResolved(resolved) {
      config = resolved;
      appName = options.name ?? readPackageName(resolved.root);
      // React Fast Refresh is the stock `@vitejs/plugin-react` runtime. squark dev
      // cannot add it for the app without changing the app's config, so say so
      // once instead of silently falling back to a plain module update.
      if (isReactApp(resolved.root) && !hasReactPlugin(resolved.plugins)) {
        resolved.logger.warn(
          "squark dev: this React app has no @vitejs/plugin-react in its vite config; " +
            "HMR will update modules without React Fast Refresh state",
        );
      }
      // The dev entry is whatever `index.html` loads first, in the same order a
      // browser would. `transformIndexHtml` has not run yet, so the raw file is
      // all we need: Vite's client and the refresh preamble are injected.
      try {
        const html = fs.readFileSync(path.join(resolved.root, "index.html"), "utf8");
        entry = extractEntryModule(html);
      } catch {
        entry = null;
      }
    },
    configureServer(server) {
      // Registered before Vite's internal middlewares, so both probes answer
      // even while the HTML fallback is still warming up.
      server.middlewares.use((req, res, next) => {
        const pathname = (req.url ?? "/").split("?")[0];
        if (pathname === DEV_HEALTH_PATH) {
          res.statusCode = 200;
          res.setHeader("content-type", "text/plain; charset=utf-8");
          res.setHeader("cache-control", "no-store");
          res.end("ok");
          return;
        }
        if (pathname === DEV_MANIFEST_PATH) {
          res.statusCode = 200;
          res.setHeader("content-type", "application/json; charset=utf-8");
          res.setHeader("cache-control", "no-store");
          res.end(`${JSON.stringify(manifestFor(server), null, 2)}\n`);
          return;
        }
        next();
      });
    },
  };
}

export default strangeQuark;
