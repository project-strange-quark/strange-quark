// SPDX-License-Identifier: MIT
// Starts a real Vite dev server through the plugin's launcher and checks the
// full hand-off: the dev manifest, transformed index.html, stock React Fast
// Refresh, and an HMR update delivered over the WebSocket squark-dom will use.

import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { startDevServer } from "../dist/launcher.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../../..");

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

function copyApp(source, destination) {
  fs.mkdirSync(destination, { recursive: true });
  fs.cpSync(source, destination, {
    recursive: true,
    filter: (src) => {
      const base = path.basename(src);
      return base !== "node_modules" && base !== "dist" && base !== "target";
    },
  });
}

function waitFor(socket, predicate, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.removeEventListener("message", onMessage);
      reject(new Error("timed out waiting for an HMR message"));
    }, timeoutMs);
    const onMessage = (event) => {
      const message = JSON.parse(event.data);
      if (!predicate(message)) return;
      clearTimeout(timer);
      socket.removeEventListener("message", onMessage);
      resolve(message);
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("close", () => {
      clearTimeout(timer);
      reject(new Error("HMR socket closed early"));
    });
  });
}

async function openHmrSocket(hmr) {
  const socket = new WebSocket(hmr, "vite-hmr");
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", () => reject(new Error(`cannot connect to ${hmr}`)), {
      once: true,
    });
  });
  await waitFor(socket, (message) => message.type === "connected");
  return socket;
}

test("dev server serves the squark hand-off and stock React Fast Refresh", { timeout: 60_000 }, async () => {
  const workRoot = path.join(repoRoot, "target/squark-dev-test");
  const appDir = path.join(workRoot, `counter-${process.pid}`);
  const workDir = path.join(workRoot, `work-${process.pid}`);
  fs.rmSync(appDir, { recursive: true, force: true });
  fs.rmSync(workDir, { recursive: true, force: true });
  copyApp(path.join(repoRoot, "apps/counter"), appDir);

  const handle = await startDevServer({
    appDir,
    workDir,
    host: "127.0.0.1",
    port: await freePort(),
  });
  let socket;
  try {
    const health = await fetch(new URL("/__squark/health", handle.url));
    assert.equal(health.status, 200);
    assert.equal((await health.text()).trim(), "ok");

    const manifest = await (await fetch(new URL("/__squark/dev-manifest.json", handle.url))).json();
    assert.equal(manifest.version, 1);
    assert.equal(manifest.entry, "/src/main.tsx");
    assert.equal(manifest.client, "/@vite/client");
    assert.equal(manifest.refresh, "/@react-refresh");
    assert.ok(manifest.origin.startsWith("http://127.0.0.1:"), manifest.origin);
    assert.equal(manifest.hmr, manifest.origin.replace("http:", "ws:") + "/");

    // The file `squark dev` compiles mirrors what a browser receives.
    const servedHtml = fs.readFileSync(handle.indexHtmlPath, "utf8");
    assert.match(servedHtml, /\/@vite\/client/);
    assert.match(servedHtml, /\/\@react-refresh/);
    assert.match(servedHtml, /\$RefreshReg\$/);
    assert.match(servedHtml, /\/src\/main\.tsx/);

    const onDisk = JSON.parse(fs.readFileSync(handle.devJsonPath, "utf8"));
    assert.equal(onDisk.origin, manifest.origin);
    assert.equal(onDisk.entry, "/src/main.tsx");

    // Warm the module graph the way the runtime does before anything changes.
    const main = await (await fetch(new URL("/src/main.tsx", handle.url))).text();
    assert.match(main, /\/src\/App\.tsx/);
    const appBefore = await (await fetch(new URL("/src/App.tsx", handle.url))).text();
    assert.match(appBefore, /import\.meta\.hot\.accept/);
    assert.match(appBefore, /\$RefreshReg\$/, "plugin-react must register components for Fast Refresh");
    assert.doesNotMatch(appBefore, /HMR_MARKER/);

    socket = await openHmrSocket(manifest.hmr);

    const appSource = path.join(appDir, "src/App.tsx");
    const original = fs.readFileSync(appSource, "utf8");
    assert.match(original, /Counter/);
    const started = performance.now();
    fs.writeFileSync(appSource, original.replace("Counter", "Counter HMR_MARKER"));

    const update = await waitFor(socket, (message) =>
      message.type === "update" &&
      (message.updates ?? []).some((entry) => entry.path.endsWith("/src/App.tsx")),
    );
    const notified = performance.now();
    assert.equal(update.updates[0].type, "js-update");

    // What the dev loader fetches for the update: the rewritten URL plus a timestamp.
    const updated = await (
      await fetch(new URL(`/src/App.tsx?t=${Math.round(notified)}`, handle.url))
    ).text();
    const fetched = performance.now();
    assert.match(updated, /HMR_MARKER/);
    assert.match(updated, /\$RefreshReg\$/);

    const notifyMs = notified - started;
    const fetchMs = fetched - started;
    console.log(
      `dev-loop: save -> hmr update ${notifyMs.toFixed(1)} ms, -> updated module ${fetchMs.toFixed(1)} ms`,
    );
    assert.ok(notifyMs < 1000, `HMR notification took ${notifyMs.toFixed(1)} ms (limit: < 1000 ms)`);
    assert.ok(fetchMs < 1000, `updated module took ${fetchMs.toFixed(1)} ms (limit: < 1000 ms)`);
  } finally {
    socket?.close();
    await handle.close();
    fs.rmSync(appDir, { recursive: true, force: true });
    fs.rmSync(workDir, { recursive: true, force: true });
  }
});
