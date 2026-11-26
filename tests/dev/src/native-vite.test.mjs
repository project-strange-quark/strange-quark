// SPDX-License-Identifier: MIT
// Native `squark build` and `squark dev` on every supported Vite major, with
// a React plugin (see vite-matrix.mjs): a production build runs in
// the real runtime, and under `squark dev` an edit arrives through HMR while
// Fast Refresh keeps component state. Needs a GPU device, so it runs only
// when SQUARK_GPU_TESTS=1 or SQUARK_NATIVE_DEV_TESTS=1.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

import { SquarkDriver } from "../../compat/src/squark-driver.mjs";
import { launchable } from "../../excalidraw/src/build.mjs";
import { defaultSquarkBin } from "./measure-native.mjs";
import { VITE_MATRIX, createFixtureApp } from "./vite-matrix.mjs";

const enabled =
  process.env.SQUARK_GPU_TESTS === "1" || process.env.SQUARK_NATIVE_DEV_TESTS === "1";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(driver, expression, label, timeoutMs = 20_000) {
  const deadline = performance.now() + timeoutMs;
  let last;
  while (performance.now() < deadline) {
    last = await driver.eval(expression).catch((error) => `eval error: ${error.message}`);
    if (last === true) return;
    await sleep(25);
  }
  const page = await driver
    .eval(
      `JSON.stringify({ h1: document.querySelector("h1")?.textContent ?? null, ` +
        `count: document.querySelector('[data-testid="count"]')?.textContent ?? null })`,
    )
    .catch((error) => `eval error: ${error.message}`);
  throw new Error(
    `timed out waiting for ${label} (last=${JSON.stringify(last)}, page=${page})\n${driver.stderrText()}`,
  );
}

for (const fixture of VITE_MATRIX) {
  test(
    `squark build packs a Vite ${fixture.vite} production build with ${fixture.react.name} that runs natively`,
    {
      timeout: 180_000,
      skip: enabled ? false : "set SQUARK_GPU_TESTS=1 or SQUARK_NATIVE_DEV_TESTS=1 to run native squark build",
    },
    async () => {
      const { appDir, workDir, cleanup } = createFixtureApp(fixture, "build");
      let driver = null;
      try {
        // squark build runs the app's own `npm run build`, as `npx squark build`
        // would, but keeps its output; run it once here to see which Vite builds.
        const npm = process.platform === "win32" ? "npm.cmd" : "npm";
        const banner = execFileSync(npm, ["run", "build"], {
          cwd: appDir,
          encoding: "utf8",
          shell: process.platform === "win32",
          timeout: 120_000,
        });
        assert.match(banner, new RegExp(`vite v${fixture.vite.replaceAll(".", "\\.")}\\b`));

        const squarkBin = defaultSquarkBin();
        const runtime =
          process.env.SQUARK_RUNTIME_BIN ||
          path.join(path.dirname(squarkBin), `squark-runtime${process.platform === "win32" ? ".exe" : ""}`);
        const out = path.join(workDir, "counter");
        fs.mkdirSync(workDir, { recursive: true });
        execFileSync(squarkBin, ["build", appDir, "--runtime", runtime, "--out", out], {
          stdio: ["ignore", "pipe", "pipe"],
          timeout: 120_000,
        });
        const built = fs.readFileSync(path.join(appDir, "dist/index.html"), "utf8");
        assert.match(built, /<script type="module"[^>]*src="\/assets\/[^"]+\.js"/);

        driver = await SquarkDriver.launch({
          binaryPath: launchable(out).executable,
          headless: true,
          width: 800,
          height: 600,
          timeoutMs: 60_000,
        });
        await waitFor(
          driver,
          `document.querySelector('[data-testid="count"]')?.textContent.trim() === "0"`,
          "the built counter to mount",
          60_000,
        );
        await driver.waitForIdle(2);
        await driver.click('[data-testid="inc"]');
        await waitFor(
          driver,
          `document.querySelector('[data-testid="count"]').textContent.trim() === "1"`,
          "a click to count to 1",
        );
      } finally {
        await driver?.close();
        cleanup();
      }
    },
  );

  test(
    `native squark dev on Vite ${fixture.vite} with ${fixture.react.name} ${fixture.react.version} keeps state across an edit`,
    {
      timeout: 180_000,
      skip: enabled ? false : "set SQUARK_GPU_TESTS=1 or SQUARK_NATIVE_DEV_TESTS=1 to run native squark dev",
    },
    async () => {
      const { appDir, workDir, cleanup } = createFixtureApp(fixture, "native");
      let driver = null;
      try {
        driver = await SquarkDriver.launch({
          binaryPath: defaultSquarkBin(),
          args: ["dev", appDir, "--port", "0", "--work-dir", workDir],
          headless: true,
          width: 800,
          height: 600,
          timeoutMs: 90_000,
        });
        await waitFor(
          driver,
          `document.querySelector('[data-testid="count"]')?.textContent.trim() === "0"`,
          "the counter to mount through squark dev",
          90_000,
        );
        // Interact only once the first frames have settled, as compare-apps
        // does: until then the button may not be where layout will put it.
        await driver.waitForIdle(2);

        await driver.click('[data-testid="inc"]');
        await driver.click('[data-testid="inc"]');
        await waitFor(
          driver,
          `document.querySelector('[data-testid="count"]').textContent.trim() === "2"`,
          "two clicks to count to 2",
        );
        await driver.waitForIdle(2);

        // Vite sends an update only to clients already connected. The runtime's
        // HMR socket opens about 100 ms after the app mounts, so a save made
        // sooner is missed, as it would be in a browser. Save again with a new
        // marker until one arrives.
        const source = path.join(appDir, "src/App.tsx");
        const pristine = fs.readFileSync(source, "utf8");
        let applied = false;
        for (let attempt = 0; attempt < 5 && !applied; attempt += 1) {
          const marker = `HMR_${process.pid}_${attempt}`;
          fs.writeFileSync(source, pristine.replace(">Counter<", `>Counter ${marker}<`));
          applied = await waitFor(
            driver,
            `document.querySelector("h1").textContent.includes(${JSON.stringify(marker)})`,
            "the edit to arrive through HMR",
            attempt === 4 ? 20_000 : 2_000,
          ).then(
            () => true,
            (error) => {
              if (attempt === 4) throw error;
              return false;
            },
          );
        }
        assert.equal(
          await driver.eval(`document.querySelector('[data-testid="count"]').textContent.trim()`),
          "2",
          "Fast Refresh reset the counter's state",
        );
      } finally {
        await driver?.close();
        cleanup();
      }
    },
  );
}
