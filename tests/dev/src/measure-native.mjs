// SPDX-License-Identifier: MIT
// Native `squark dev` measurement: save in a component to the
// presented frame, with React Fast Refresh keeping component state.
//
// The clock bridge: `frame.nextPresented` reports `presentedAtMs` on the
// runtime's monotonic clock. Call it once before the save (t0) and once after
// the edited DOM is visible (t1), and subtract the Node-side gap between t0
// arriving and the write, so the result is measured on the runtime's clock.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { SquarkDriver } from "../../compat/src/squark-driver.mjs";
import { comparePngBuffers } from "../../reftests/src/compare.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(__dirname, "../../..");

export function defaultSquarkBin() {
  return process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark");
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(driver, expression, { label, timeoutMs = 15_000, intervalMs = 10 } = {}) {
  const deadline = performance.now() + timeoutMs;
  let last;
  while (performance.now() < deadline) {
    try {
      last = typeof expression === "function" ? await expression() : await driver.eval(expression);
      if (last) return last;
    } catch (error) {
      last = `eval error: ${error.message}`;
    }
    await sleep(intervalMs);
  }
  throw new Error(`timed out waiting for ${label ?? expression} (last=${JSON.stringify(last)})`);
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
 * Runs `squark dev <app>` headless with test control, edits the app's heading
 * `iterations` times, and times save -> presented frame for each edit.
 */
export async function measureNativeDevLoop({
  appName = "shadcn",
  iterations = 3,
  squarkBin = defaultSquarkBin(),
  workRoot = path.join(repoRoot, "target/squark-dev/native"),
  reportPath = null,
  mountTimeoutMs = 90_000,
  editTimeoutMs = 20_000,
} = {}) {
  const appDir = path.join(workRoot, `app-${appName}-${process.pid}`);
  const workDir = path.join(workRoot, `work-${process.pid}`);
  fs.rmSync(appDir, { recursive: true, force: true });
  fs.rmSync(workDir, { recursive: true, force: true });
  fs.mkdirSync(appDir, { recursive: true });
  fs.cpSync(path.join(repoRoot, "apps", appName), appDir, {
    recursive: true,
    filter: (src) => !["node_modules", "dist", "target"].includes(path.basename(src)),
  });

  const sourcePath = path.join(appDir, "src/App.tsx");
  const pristine = fs.readFileSync(sourcePath, "utf8");
  const editAnchor = "Workspace Settings";
  if (!pristine.includes(editAnchor)) {
    throw new Error(`${sourcePath} does not contain the edit anchor ${JSON.stringify(editAnchor)}`);
  }

  const shotDir = path.join(repoRoot, "tests/results/interactive-apps/actual");
  fs.mkdirSync(shotDir, { recursive: true });
  const beforeShot = path.join(shotDir, "hmr-before.png");
  const afterShot = path.join(shotDir, "hmr-after.png");
  let visibleChange = null;

  const launchedAt = performance.now();
  let driver = null;
  try {
    driver = await SquarkDriver.launch({
      binaryPath: squarkBin,
      args: ["dev", appDir, "--port", "0", "--work-dir", workDir],
      headless: true,
      width: 1280,
      height: 800,
      timeoutMs: mountTimeoutMs,
    });

    let mounted = false;
    try {
      await waitFor(driver, `!!document.querySelector('[data-testid="profile-form"]')`, {
        label: "shadcn to mount through squark dev",
        timeoutMs: mountTimeoutMs,
        intervalMs: 50,
      });
      mounted = true;
    } catch (error) {
      // Include the page state in the error: body text, title and any dev log.
      const body = await driver.eval("document.body ? document.body.innerText.slice(0, 4000) : null").catch(() => null);
      const title = await driver.eval("document.title").catch(() => null);
      const htmlLength = await driver
        .eval("document.documentElement ? document.documentElement.outerHTML.length : 0")
        .catch(() => null);
      throw new Error(
        `${error.message}\ndocument.title=${JSON.stringify(title)}\nhtmlLength=${htmlLength}\nbody=${JSON.stringify(body)}\ndev log:\n${driver.stderrText()}`,
      );
    }
    const mountMs = performance.now() - launchedAt;

    // The app can mount before Vite's WebSocket receives its startup message.
    await waitFor(driver, () => driver.stderrText().includes("console.debug: [vite] connected."), {
      label: "the Vite HMR connection",
      timeoutMs: mountTimeoutMs,
    });

    // One untimed save first: the first HMR update also re-applies the CSS
    // module, so it pays a full restyle that a steady-state edit does not.
    const warmupMarker = `SQUARK_HMR_WARMUP_${process.pid}`;
    const warmupStart = performance.now();
    fs.writeFileSync(sourcePath, pristine.replace(editAnchor, `${editAnchor} ${warmupMarker}`));
    await waitFor(
      driver,
      `document.querySelector('h1').textContent.includes(${JSON.stringify(warmupMarker)})`,
      { label: "the warm-up edit", timeoutMs: editTimeoutMs },
    );
    await driver.nextPresented();
    const warmupMs = performance.now() - warmupStart;

    // Fast Refresh must keep this tab selection across the edits.
    await driver.click('[data-testid="tab-security"]');
    await waitFor(
      driver,
      `document.querySelector('[role="tab"][data-state="active"]')?.getAttribute("data-testid") === "tab-security"`,
      { label: "the security tab to activate" },
    );

    const results = [];
    for (let i = 0; i < iterations; i += 1) {
      const marker = `SQUARK_HMR_${process.pid}_${i}`;

      // Runtime-clock baseline: the frame presented before the save.
      const before = await driver.nextPresented();
      if (i === 0) await driver.screenshot(beforeShot);
      const nodeBefore = performance.now();

      fs.writeFileSync(sourcePath, pristine.replace(editAnchor, `${editAnchor} ${marker}`));
      const nodeSaved = performance.now();

      await waitFor(
        driver,
        `document.querySelector('h1').textContent.includes(${JSON.stringify(marker)})`,
        { label: `edit ${i} to reach the heading`, timeoutMs: editTimeoutMs, intervalMs: 5 },
      );
      const nodeCommitted = performance.now();

      const after = await driver.nextPresented();
      const nodePresented = performance.now();
      if (i === 0) {
        await driver.screenshot(afterShot);
        // The presented frame must visibly contain the edit, not just the DOM.
        const cmp = comparePngBuffers(fs.readFileSync(beforeShot), fs.readFileSync(afterShot), {
          maxChannelDelta: 8,
          maxDiffRatio: 1,
        });
        visibleChange = {
          diffPixels: cmp.diffPixels,
          diffRatio: cmp.diffRatio,
          maxDelta: cmp.maxObservedDelta,
          before: path.relative(repoRoot, beforeShot),
          after: path.relative(repoRoot, afterShot),
        };
        assert.ok(
          cmp.diffPixels > 100,
          `the presented frame did not visibly change (${cmp.diffPixels} pixels, maxDelta ${cmp.maxObservedDelta})`,
        );
      }

      const saveToPresentedMs = after.presentedAtMs - before.presentedAtMs - (nodeSaved - nodeBefore);
      const activeTab = await driver.eval(
        `document.querySelector('[role="tab"][data-state="active"]')?.getAttribute("data-testid") ?? null`,
      );
      assert.equal(activeTab, "tab-security", `Fast Refresh lost tab state after edit ${i}`);

      results.push({
        marker,
        frameId: after.frameId,
        saveToPresentedMs: Number(saveToPresentedMs.toFixed(2)),
        saveToCommitMs: Number((nodeCommitted - nodeSaved).toFixed(2)),
        commitToPresentedMs: Number((nodePresented - nodeCommitted).toFixed(2)),
        observedSaveToPresentedMs: Number((nodePresented - nodeSaved).toFixed(2)),
      });
      console.log(
        `edit ${i}: save -> presented ${results[i].saveToPresentedMs} ms ` +
          `(commit ${results[i].saveToCommitMs}, present ${results[i].commitToPresentedMs}, frame ${after.frameId})`,
      );
      if (i + 1 < iterations) await sleep(250);
    }

    const bySave = percentiles(results.map((entry) => entry.saveToPresentedMs));
    const byObserved = percentiles(results.map((entry) => entry.observedSaveToPresentedMs));
    const report = {
      requirement: "live reload: save to presented frame under 1000 ms",
      app: appName,
      engine: "native",
      headless: true,
      squarkBin,
      gpuAdapter: process.env.SQUARK_GPU_ADAPTER ?? "auto",
      vkIcd: process.env.VK_ICD_FILENAMES ?? null,
      iterations,
      mountMs: Number(mountMs.toFixed(2)),
      warmupMs: Number(warmupMs.toFixed(2)),
      fastRefreshKeptState: true,
      visibleChange,
      saveToPresentedMs: bySave,
      observedSaveToPresentedMs: byObserved,
      passed: bySave.max < 1000,
      cases: results,
    };
    if (reportPath) {
      fs.mkdirSync(path.dirname(reportPath), { recursive: true });
      fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
    }
    return report;
  } catch (error) {
    const log = driver?.stderrText?.() ?? "";
    throw new Error(`${error.message}${log ? `\n--- squark dev log ---\n${log}` : ""}`);
  } finally {
    await driver?.close().catch(() => undefined);
    fs.rmSync(appDir, { recursive: true, force: true });
    fs.rmSync(workDir, { recursive: true, force: true });
  }
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = await measureNativeDevLoop({
    iterations: Number(process.env.SQUARK_DEV_ITERATIONS ?? 3),
    reportPath: path.join(repoRoot, "tests/results/interactive-apps/dev-loop.json"),
  });
  console.log(
    `native dev-loop/${report.app}: save -> presented frame p50 ${report.saveToPresentedMs.p50} ms ` +
      `(p95 ${report.saveToPresentedMs.p95}, max ${report.saveToPresentedMs.max}); mount ${report.mountMs} ms`,
  );
  console.log(`report: tests/results/interactive-apps/dev-loop.json`);
}
