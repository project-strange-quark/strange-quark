// SPDX-License-Identifier: MIT
// Measures the save-to-update leg of `squark dev` against a real Vite server:
//
//   save -> Vite watcher -> transform -> HMR update on the WebSocket
//   save -> the updated module fetched over HTTP (what the dev loader does)
//
// The final leg, the presented frame in the native runtime, is measured by
// measure-native.mjs.

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { startDevServer } from "@strange-quark/vite-plugin/launcher";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(__dirname, "../../..");

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

function percentiles(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const at = (q) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  return {
    samples: sorted.map((value) => Number(value.toFixed(2))),
    min: Number(sorted[0].toFixed(2)),
    p50: Number(at(0.5).toFixed(2)),
    p95: Number(at(0.95).toFixed(2)),
    max: Number(sorted[sorted.length - 1].toFixed(2)),
  };
}

/**
 * Buffers HMR messages so an update that lands before the next `next()` call is
 * never missed: a save can be picked up in the same tick as the write.
 */
function hmrQueue(socket) {
  const messages = [];
  const waiters = [];
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    const index = waiters.findIndex((waiter) => waiter.predicate(message));
    if (index >= 0) {
      const [waiter] = waiters.splice(index, 1);
      clearTimeout(waiter.timer);
      waiter.resolve(message);
      return;
    }
    messages.push(message);
  });

  return (predicate, timeoutMs) =>
    new Promise((resolve, reject) => {
      const index = messages.findIndex(predicate);
      if (index >= 0) {
        const [message] = messages.splice(index, 1);
        resolve(message);
        return;
      }
      const waiter = {
        predicate,
        resolve,
        timer: setTimeout(() => {
          const at = waiters.indexOf(waiter);
          if (at >= 0) waiters.splice(at, 1);
          reject(new Error("timed out waiting for an HMR update"));
        }, timeoutMs),
      };
      waiters.push(waiter);
    });
}

async function openHmrSocket(hmr) {
  const socket = new WebSocket(hmr, "vite-hmr");
  const next = hmrQueue(socket);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", () => reject(new Error(`cannot connect to ${hmr}`)), {
      once: true,
    });
  });
  await next((message) => message.type === "connected", 15_000);
  return { socket, next };
}

/**
 * Runs `iterations` save-measure cycles on a copy of `appDir`.
 * Returns the report object; writes it to `reportPath` when given.
 */
export async function measureDevLoop({
  appName = "counter",
  iterations = 5,
  workRoot = path.join(repoRoot, "target/squark-dev/measure"),
  reportPath = null,
} = {}) {
  const appDir = path.join(workRoot, `app-${process.pid}`);
  const workDir = path.join(workRoot, `work-${process.pid}`);
  fs.rmSync(appDir, { recursive: true, force: true });
  fs.rmSync(workDir, { recursive: true, force: true });
  fs.mkdirSync(appDir, { recursive: true });
  fs.cpSync(path.join(repoRoot, "apps", appName), appDir, {
    recursive: true,
    filter: (src) => !["node_modules", "dist", "target"].includes(path.basename(src)),
  });

  const handle = await startDevServer({
    appDir,
    workDir,
    host: "127.0.0.1",
    port: await freePort(),
  });
  let socket;
  try {
    const sourcePath = path.join(appDir, "src/App.tsx");
    const pristine = fs.readFileSync(sourcePath, "utf8");
    if (!pristine.includes("Counter")) {
      throw new Error(`${sourcePath} does not contain the edit marker "Counter"`);
    }

    // Warm the graph the way a running app has already done.
    await (await fetch(new URL("/index.html", handle.url))).text();
    for (const module of ["/src/main.tsx", "/src/App.tsx"]) {
      await (await fetch(new URL(module, handle.url))).text();
    }

    const opened = await openHmrSocket(handle.hmr);
    socket = opened.socket;
    const next = opened.next;

    const hmrSamples = [];
    const moduleSamples = [];
    for (let i = 0; i < iterations; i += 1) {
      const marker = `SQUARK_HMR_${process.pid}_${i}`;
      const started = performance.now();
      fs.writeFileSync(sourcePath, pristine.replace("Counter", `Counter ${marker}`));

      const update = await next(
        (message) =>
          message.type === "update" &&
          (message.updates ?? []).some((entry) => entry.path.endsWith("/src/App.tsx")),
        15_000,
      );
      const notified = performance.now();
      hmrSamples.push(notified - started);

      const timestamp = update.updates[0].timestamp;
      const updated = await (
        await fetch(new URL(`/src/App.tsx?t=${timestamp}&squark=${i}`, handle.url))
      ).text();
      const fetched = performance.now();
      if (!updated.includes(marker)) {
        throw new Error(`updated module did not contain ${marker}`);
      }
      moduleSamples.push(fetched - started);
      // Let Vite's watcher and transform queue settle between saves, the way a
      // human editing a file does; the settle time is outside the measurement.
      if (i + 1 < iterations) await new Promise((resolve) => setTimeout(resolve, 200));
    }

    const report = {
      requirement: "live reload: save to presented frame under 1000 ms",
      app: appName,
      vite: handle.server.config.version ?? null,
      origin: handle.origin,
      iterations,
      saveToHmrUpdateMs: percentiles(hmrSamples),
      saveToUpdatedModuleMs: percentiles(moduleSamples),
      presentedFrame: {
        measured: false,
        reason: "measured by measure-native.mjs",
      },
      measuredAt: new Date().toISOString(),
    };
    if (reportPath) {
      fs.mkdirSync(path.dirname(reportPath), { recursive: true });
      fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
    }
    return report;
  } finally {
    socket?.close();
    await handle.close();
    fs.rmSync(appDir, { recursive: true, force: true });
    fs.rmSync(workDir, { recursive: true, force: true });
  }
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = await measureDevLoop({
    iterations: Number(process.env.SQUARK_DEV_ITERATIONS ?? 5),
    reportPath: path.join(repoRoot, "target/squark-dev/measure.json"),
  });
  console.log(
    `dev-loop/${report.app}: save -> hmr update p50 ${report.saveToHmrUpdateMs.p50} ms ` +
      `(max ${report.saveToHmrUpdateMs.max}); save -> updated module p50 ` +
      `${report.saveToUpdatedModuleMs.p50} ms (max ${report.saveToUpdatedModuleMs.max})`,
  );
  console.log(`report: target/squark-dev/measure.json`);
  console.log(`save -> presented frame: not measurable yet (${report.presentedFrame.reason})`);
}
