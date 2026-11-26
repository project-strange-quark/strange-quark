// SPDX-License-Identifier: MIT
// `squark dev` checks:
//   1. a real `squark dev` run writes a dev pack that points the runtime at the
//      Vite origin, and stops the Vite server when it exits;
//   2. a save reaches Vite's HMR client and the updated module in under
//      1 second (native.test.mjs covers save to presented frame).

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { measureDevLoop, repoRoot } from "./measure.mjs";
import { sqpakEntry, sqpakManifest, readSqPak } from "./sqpak.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const squarkBin = process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark");
const runtimeDir = path.join(repoRoot, "target/squark-dev/tests");

function hasSquarkBinary() {
  return fs.existsSync(squarkBin);
}

test("squark dev writes a dev pack pointing at the Vite origin and stops Vite", { timeout: 180_000 }, async () => {
  if (!hasSquarkBinary()) {
    console.log(`skipping: no squark binary at ${squarkBin} (run cargo build --workspace)`);
    return;
  }
  fs.mkdirSync(runtimeDir, { recursive: true });
  const fakeRuntime = path.join(runtimeDir, `fake-runtime-${process.pid}.sh`);
  const receivedPath = path.join(runtimeDir, `received-${process.pid}.txt`);
  const workDir = path.join(runtimeDir, `work-${process.pid}`);
  fs.rmSync(workDir, { recursive: true, force: true });
  fs.writeFileSync(fakeRuntime, '#!/bin/sh\nprintf "%s\\n" "$1" > "$SQUARK_TEST_PACK_OUT"\nexit 0\n');
  fs.chmodSync(fakeRuntime, 0o755);

  let origin = null;
  try {
    const output = execFileSync(
      squarkBin,
      [
        "dev",
        path.join(repoRoot, "apps/counter"),
        "--port",
        "0",
        "--runtime",
        fakeRuntime,
        "--work-dir",
        workDir,
      ],
      {
        encoding: "utf8",
        timeout: 120_000,
        env: { ...process.env, SQUARK_TEST_PACK_OUT: receivedPath },
      },
    );
    assert.match(output, /squark dev: /);

    const packPath = fs.readFileSync(receivedPath, "utf8").trim();
    assert.equal(packPath, path.join(workDir, "dev.sqpak"));
    const entries = readSqPak(packPath);
    const manifest = sqpakManifest(entries);
    assert.equal(manifest.version, 1);
    assert.equal(manifest.entry_html, "/index.html");
    assert.ok(manifest.dev, "pack manifest must carry the dev block");
    origin = manifest.dev.origin;
    assert.match(origin, /^http:\/\/127\.0\.0\.1:\d+$/);
    assert.equal(manifest.dev.hmr, origin.replace("http:", "ws:") + "/");
    assert.equal(manifest.dev.entry, "/src/main.tsx");
    assert.equal(manifest.dev.client, "/@vite/client");
    assert.equal(manifest.dev.refresh, "/@react-refresh");
    assert.ok(manifest.network.allow.includes(origin));
    assert.ok(manifest.network.allow.includes(origin.replace("http:", "ws:")));

    // The document compiled from what Vite served: preamble, client, app entry.
    const document = sqpakEntry(entries, "/__squark/document.bin");
    assert.ok(document && document.length > 0);
    const html = sqpakEntry(entries, "/index.html").toString("utf8");
    assert.match(html, /\/@vite\/client/);
    assert.match(html, /\/src\/main\.tsx/);

    // `squark dev` kills Vite before it returns.
    await assert.rejects(fetch(`${origin}/__squark/health`), /fetch failed|ECONNREFUSED/);
  } finally {
    fs.rmSync(workDir, { recursive: true, force: true });
    fs.rmSync(fakeRuntime, { force: true });
    fs.rmSync(receivedPath, { force: true });
  }
});

test("a save reaches the HMR client and the updated module in under 1 second", { timeout: 180_000 }, async () => {
  const report = await measureDevLoop({
    iterations: 3,
    workRoot: path.join(repoRoot, "target/squark-dev/tests"),
  });
  console.log(
    `dev-loop: save -> hmr update p50 ${report.saveToHmrUpdateMs.p50} ms ` +
      `(max ${report.saveToHmrUpdateMs.max}); save -> updated module p50 ` +
      `${report.saveToUpdatedModuleMs.p50} ms (max ${report.saveToUpdatedModuleMs.max})`,
  );
  assert.ok(
    report.saveToHmrUpdateMs.max < 1000,
    `save -> HMR update took ${report.saveToHmrUpdateMs.max} ms (limit: < 1000 ms)`,
  );
  assert.ok(
    report.saveToUpdatedModuleMs.max < 1000,
    `save -> updated module took ${report.saveToUpdatedModuleMs.max} ms (limit: < 1000 ms)`,
  );
});
