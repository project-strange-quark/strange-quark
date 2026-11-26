// SPDX-License-Identifier: MIT
// Starts a Vite dev server with the Strange Quark hand-off. Used by the `squark dev`
// launcher and by the dev-loop measurement tests.

import fs from "node:fs";
import path from "node:path";

import { createServer, version as viteVersion, type ViteDevServer } from "vite";

import { strangeQuark } from "./index.js";
import { extractEntryModule } from "./manifest.js";

export interface DevServerOptions {
  /** Application directory (the Vite root). */
  appDir: string;
  /** Directory the launcher writes `index.html` and `squark-dev.json` into. */
  workDir: string;
  /** Bind address; defaults to 127.0.0.1. */
  host?: string | undefined;
  /** Port; omit to keep the app's configured port. */
  port?: number | undefined;
  /** Explicit Vite config file; omit to auto-detect. */
  configFile?: string | null | undefined;
  /** Override the app name written to `squark-dev.json`. */
  name?: string | undefined;
}

export interface DevServerHandle {
  server: ViteDevServer;
  /** Origin without a trailing slash, for the pack manifest. */
  origin: string;
  /** Origin with a trailing slash, for URL joins. */
  url: string;
  /** WebSocket URL for Vite's HMR client. */
  hmr: string;
  /** First application module script in index.html, if any. */
  entry: string | null;
  /** HTML that `squark dev` compiles into `document.bin`. */
  indexHtmlPath: string;
  /** Machine-readable hand-off consumed by `squark dev`. */
  devJsonPath: string;
  close(): Promise<void>;
}

function stripTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

interface PendingDep {
  processing?: Promise<unknown>[];
}

/**
 * Waits for Vite's initial dependency scan and pre-bundling to settle.
 *
 * The scan starts in the background when the server starts listening; closing
 * during it makes Vite's `close()` hang, and the native runtime's first module
 * request would otherwise queue behind the optimiser. Vite's public API has no
 * promise for this, so the internal `depsOptimizer` is read defensively.
 */
async function waitForOptimizer(server: { environments?: Record<string, unknown> }): Promise<void> {
  const client = server.environments?.client as
    | { depsOptimizer?: { scanProcessing?: Promise<void>; metadata?: { discovered?: Record<string, PendingDep> } } }
    | undefined;
  const optimizer = client?.depsOptimizer;
  if (!optimizer) return;
  const deadline = Date.now() + 10_000;
  try {
    await optimizer.scanProcessing;
    while (Date.now() < deadline) {
      const discovered = Object.values(optimizer.metadata?.discovered ?? {});
      const pending = discovered.flatMap((dep) => dep.processing ?? []);
      if (pending.length === 0) return;
      await Promise.allSettled(pending);
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  } catch {
    // Optimisation failures surface on the first request instead; never block startup.
  }
}

/** Starts Vite, transforms `index.html`, and writes the dev hand-off files. */
export async function startDevServer(options: DevServerOptions): Promise<DevServerHandle> {
  const appDir = path.resolve(options.appDir);
  const workDir = path.resolve(options.workDir);
  fs.mkdirSync(workDir, { recursive: true });

  const server = await createServer({
    root: appDir,
    configFile: options.configFile ?? undefined,
    server: {
      host: options.host ?? "127.0.0.1",
      ...(options.port === undefined ? {} : { port: options.port }),
      // `squark dev` never opens a browser; the native runtime is the client.
      open: false,
      strictPort: false,
    },
    plugins: [strangeQuark({ name: options.name })],
  });

  try {
    await server.listen();
    await waitForOptimizer(server);
    const url = server.resolvedUrls?.local[0] ?? `http://${options.host ?? "127.0.0.1"}:${server.config.server.port}`;
    const origin = stripTrailingSlash(url);

    const sourceHtml = fs.readFileSync(path.join(server.config.root, "index.html"), "utf8");
    // Exactly what Vite serves for `/index.html`: the client script, the React
    // Fast Refresh preamble and the app entry, in browser order.
    const html = await server.transformIndexHtml("/index.html", sourceHtml);
    const indexHtmlPath = path.join(workDir, "index.html");
    fs.writeFileSync(indexHtmlPath, html);

    const entry = extractEntryModule(html) ?? extractEntryModule(sourceHtml);
    const hmr = new URL(origin);
    hmr.protocol = hmr.protocol === "https:" ? "wss:" : "ws:";
    hmr.pathname = server.config.base;

    const appName =
      options.name ??
      (() => {
        try {
          const name = (JSON.parse(fs.readFileSync(path.join(server.config.root, "package.json"), "utf8")) as {
            name?: unknown;
          }).name;
          if (typeof name === "string" && name.trim() !== "") return name.trim();
        } catch {
          // Fall back to the directory name.
        }
        return path.basename(server.config.root) || "squark-app";
      })();

    const devJsonPath = path.join(workDir, "squark-dev.json");
    fs.writeFileSync(
      devJsonPath,
      `${JSON.stringify(
        {
          version: 1,
          appName,
          origin,
          hmr: hmr.toString(),
          entry,
          client: "/@vite/client",
          refresh: "/@react-refresh",
          root: server.config.root,
          vite: viteVersion,
          indexHtml: "index.html",
        },
        null,
        2,
      )}\n`,
    );

    return {
      server,
      origin,
      url: `${origin}/`,
      hmr: hmr.toString(),
      entry,
      indexHtmlPath,
      devJsonPath,
      close: async () => {
        // Vite can hang `close()` forever when it is cancelled mid dependency
        // optimisation ("The build was canceled"). The HTTP socket and watcher
        // are torn down first, so a cap only keeps a long-lived `squark dev`
        // process from wedging on shutdown.
        await Promise.race([
          server.close(),
          new Promise<void>((resolve) => setTimeout(resolve, 5_000).unref()),
        ]);
      },
    };
  } catch (error) {
    await server.close().catch(() => undefined);
    throw error;
  }
}
