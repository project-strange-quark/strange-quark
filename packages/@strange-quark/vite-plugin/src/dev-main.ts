#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Long-running dev-server process spawned by `squark dev`.
//
// Usage: node dev-main.js <app-dir> <work-dir>
// Environment:
//   SQUARK_DEV_HOST     bind address (default 127.0.0.1)
//   SQUARK_DEV_PORT     port (default: Vite's configured port)
//   SQUARK_DEV_CONFIG   explicit vite config file
//   SQUARK_DEV_NAME     app name override
//
// Prints `SQUARK_DEV_READY <origin>` after `squark-dev.json` and `index.html` exist,
// then keeps running until SIGTERM or SIGINT.

import { startDevServer } from "./launcher.js";

const [appDir, workDir] = process.argv.slice(2);
if (!appDir || !workDir) {
  console.error("usage: dev-main <app-dir> <work-dir>");
  process.exit(2);
}

const portText = process.env.SQUARK_DEV_PORT;
const port = portText === undefined || portText === "" ? undefined : Number.parseInt(portText, 10);

const handle = await startDevServer({
  appDir,
  workDir,
  host: process.env.SQUARK_DEV_HOST || undefined,
  port: Number.isFinite(port) ? port : undefined,
  configFile: process.env.SQUARK_DEV_CONFIG || null,
  name: process.env.SQUARK_DEV_NAME || undefined,
});

process.stdout.write(`SQUARK_DEV_READY ${handle.origin}\n`);

let closing = false;
// A pipe from the CLI can be at EOF (stdin /dev/null), and then nothing else
// would hold the event loop once Vite's handles settle. Keep it alive until a
// signal arrives or the server closes.
const keepAlive = setInterval(() => undefined, 1 << 30);
const shutdown = (code: number): void => {
  if (closing) return;
  closing = true;
  clearInterval(keepAlive);
  handle
    .close()
    .catch(() => undefined)
    .finally(() => process.exit(code));
};

process.on("SIGTERM", () => shutdown(0));
process.on("SIGINT", () => shutdown(0));
process.stdin.on("end", () => undefined);
process.stdin.resume();
