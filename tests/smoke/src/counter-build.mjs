#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// End-to-end `squark build` smoke test on apps/counter with the real squark-runtime:
// app build -> squark pack -> append -> headless screenshot via `squark screenshot`
// and self-executing appended binary. Verifies React 19 mounts and renders
// the counter card inside the #f8fafc shell.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";

import { launchable } from "../../excalidraw/src/build.mjs";
import { sqpakManifest, readSqPak } from "../../dev/src/sqpak.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

const exeSuffix = process.platform === "win32" ? ".exe" : "";

function defaultSquarkBin() {
  return process.env.SQUARK_BIN || path.join(repoRoot, `target/debug/squark${exeSuffix}`);
}

function defaultRuntimeBin() {
  return process.env.SQUARK_RUNTIME_BIN || path.join(repoRoot, `target/debug/squark-runtime${exeSuffix}`);
}

function execOrThrow(cmd, args, opts = {}) {
  return execFileSync(cmd, args, {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    timeout: 120_000,
    ...opts,
  });
}

function pixelAt(decoded, x, y) {
  const idx = (y * decoded.width + x) * 4;
  return [
    decoded.data[idx],
    decoded.data[idx + 1],
    decoded.data[idx + 2],
    decoded.data[idx + 3],
  ];
}

function rgbaDelta(a, b) {
  return Math.max(
    Math.abs(a[0] - b[0]),
    Math.abs(a[1] - b[1]),
    Math.abs(a[2] - b[2]),
    Math.abs(a[3] - b[3]),
  );
}

function assertCounterRenderedPng(squarkShotPath, selfShotPath, expectedWidth, expectedHeight) {
  const squarkDecoded = PNG.sync.read(fs.readFileSync(squarkShotPath));
  const selfDecoded = PNG.sync.read(fs.readFileSync(selfShotPath));
  assert.equal(squarkDecoded.width, expectedWidth, `expected width ${expectedWidth}, got ${squarkDecoded.width}`);
  assert.equal(squarkDecoded.height, expectedHeight, `expected height ${expectedHeight}, got ${squarkDecoded.height}`);
  assert.equal(selfDecoded.width, expectedWidth);
  assert.equal(selfDecoded.height, expectedHeight);

  // Outer shell corner must match #f8fafc (248, 250, 252, 255).
  const expectedShell = [0xf8, 0xfa, 0xfc, 0xff];
  const corner = pixelAt(squarkDecoded, 10, 10);
  const cornerDelta = rgbaDelta(corner, expectedShell);
  assert.ok(
    cornerDelta <= 2,
    `expected outer shell corner RGBA ${expectedShell.join(",")}, got ${corner.join(",")}`,
  );

  // Verify React mounted the Counter card (including the primary #2563eb "+1" button)
  // and that `squark screenshot` matches direct self-execution pixel-for-pixel.
  const expectedPrimary = [0x25, 0x63, 0xeb, 0xff];
  let primaryBluePixels = 0;
  let nonShellPixels = 0;
  let maxSelfDiff = 0;
  for (let i = 0; i < squarkDecoded.data.length; i += 4) {
    const p = [
      squarkDecoded.data[i],
      squarkDecoded.data[i + 1],
      squarkDecoded.data[i + 2],
      squarkDecoded.data[i + 3],
    ];
    const q = [
      selfDecoded.data[i],
      selfDecoded.data[i + 1],
      selfDecoded.data[i + 2],
      selfDecoded.data[i + 3],
    ];
    const diff = rgbaDelta(p, q);
    if (diff > maxSelfDiff) maxSelfDiff = diff;
    if (rgbaDelta(p, expectedShell) > 4) nonShellPixels++;
    if (rgbaDelta(p, expectedPrimary) <= 4) primaryBluePixels++;
  }

  assert.ok(
    nonShellPixels > 20_000,
    `expected rendered React Counter card (>20000 non-shell pixels), got ${nonShellPixels}`,
  );
  assert.ok(
    primaryBluePixels > 1_000,
    `expected primary #2563eb "+1" button (>1000 pixels), got ${primaryBluePixels}`,
  );
  assert.ok(
    maxSelfDiff <= 2,
    `expected squark screenshot and self-executing binary to match (maxDelta<=2), got ${maxSelfDiff}`,
  );

  return {
    width: squarkDecoded.width,
    height: squarkDecoded.height,
    cornerDelta,
    nonShellPixels,
    primaryBluePixels,
    maxDelta: maxSelfDiff,
  };
}

export function runCounterBuildSmoke({
  squarkBin = defaultSquarkBin(),
  runtimeBin = defaultRuntimeBin(),
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto",
  outDir = path.join(repoRoot, "target/smoke/counter"),
  skipScreenshot = false,
} = {}) {
  const counterDir = path.join(repoRoot, "apps/counter");
  fs.mkdirSync(outDir, { recursive: true });

  const outBinary = path.join(outDir, "counter-native");
  const outSqpak = path.join(outDir, "counter.sqpak");
  const outShot = path.join(outDir, "counter-squark-screenshot.png");
  const outSelfShot = path.join(outDir, "counter-self-screenshot.png");

  for (const p of [outBinary, `${outBinary}.exe`, `${outBinary}.app`, outSqpak, outShot, outSelfShot]) {
    if (fs.existsSync(p)) fs.rmSync(p, { recursive: true, force: true });
  }

  // 1. Run `squark build` end-to-end: Vite app build -> squark pack -> append to real squark-runtime.
  const buildStart = performance.now();
  execOrThrow(squarkBin, [
    "build",
    counterDir,
    "--runtime",
    runtimeBin,
    "--out",
    outBinary,
    "--sqpak-out",
    outSqpak,
  ]);
  const buildMs = performance.now() - buildStart;

  // Linux and Windows append the pack to the runtime; macOS writes a signed
  // .app whose executable and app.sqpak live under Contents/.
  const artifact = launchable(outBinary, "counter-native");
  assert.equal(fs.existsSync(artifact.executable), true, "squark build produced no executable");
  const stat = fs.statSync(artifact.executable);
  assert.equal((stat.mode & 0o111) !== 0, true, "assembled runtime must be executable");

  // 2. Inspect the pack wherever the platform keeps it.
  const inspectJson = execOrThrow(squarkBin, ["inspect", "--dump-pack", artifact.executable, "--json"]);
  const inspection = JSON.parse(inspectJson);
  if (process.platform === "darwin") {
    assert.equal(inspection.container, "standalone-sqpak");
    assert.equal(inspection.trailer, null);
  } else {
    assert.equal(inspection.container, "appended-binary");
    assert.equal(inspection.trailer?.crc_valid, true);
    assert.equal(inspection.trailer.pack_offset % 4096, 0, "pack_offset must be 4096-byte aligned");
  }

  // The standalone pack carries the squark.config window block (title and size).
  const manifest = sqpakManifest(readSqPak(outSqpak));
  assert.equal(manifest.window?.title, "app-counter");
  assert.equal(manifest.window?.width, 1280);
  assert.equal(manifest.window?.height, 800);

  const entries = inspection.pack.entries;
  const byPath = new Map(entries.map((e) => [e.path, e]));
  assert.ok(byPath.has("/__squark/document.bin"), "missing /__squark/document.bin");
  assert.ok(byPath.has("/__squark/manifest.json"), "missing /__squark/manifest.json");
  assert.ok(byPath.has("/index.html"), "missing /index.html");
  assert.equal(byPath.get("/index.html").mime, "text/html; charset=utf-8");

  const jsEntry = entries.find((e) => e.path.startsWith("/assets/") && e.path.endsWith(".js"));
  const cssEntry = entries.find((e) => e.path.startsWith("/assets/") && e.path.endsWith(".css"));
  assert.ok(jsEntry, "missing bundled JS asset");
  assert.equal(jsEntry.mime, "text/javascript; charset=utf-8");
  assert.ok(cssEntry, "missing bundled CSS asset");
  assert.equal(cssEntry.mime, "text/css; charset=utf-8");
  assert.equal(
    entries.some((e) => e.path.endsWith(".map")),
    false,
    "sourcemaps must not be packed by default",
  );

  assert.equal(inspection.document.quirks_mode, "standards");
  assert.equal(inspection.document.scripts.length, 2);
  assert.equal(inspection.document.scripts[0].kind, "external-module");
  assert.equal(inspection.document.scripts[0].payload, jsEntry.path);
  assert.equal(inspection.document.scripts[1].kind, "external-stylesheet");
  assert.equal(inspection.document.scripts[1].payload, cssEntry.path);

  const report = {
    app: "apps/counter",
    binaryPath: artifact.executable,
    binaryBytes: artifact.bytes,
    ...(artifact.bundle ? { bundle: artifact.bundle } : {}),
    packBytes: inspection.pack.total_bytes,
    entryCount: inspection.pack.entry_count,
    buildMs: Math.round(buildMs * 10) / 10,
    screenshot: null,
  };

  if (!skipScreenshot) {
    const xdgDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-smoke-counter-xdg-"));
    try {
      const env = { ...process.env, XDG_DATA_HOME: xdgDir };
      // 3a. Run headless via `squark screenshot <built-binary>`.
      const shotStart = performance.now();
      execOrThrow(
        squarkBin,
        [
          "screenshot",
          artifact.bundle ?? artifact.executable,
          "--out",
          outShot,
          "--width",
          "800",
          "--height",
          "600",
          "--dpr",
          "1",
          "--gpu-adapter",
          gpuAdapter,
          "--runtime",
          runtimeBin,
        ],
        { env },
      );
      const squarkScreenshotMs = performance.now() - shotStart;

      // 3b. Also execute the assembled self-contained binary directly (`./counter-native --screenshot ...`).
      const selfStart = performance.now();
      execOrThrow(
        artifact.executable,
        [
          "--screenshot",
          "--out",
          outSelfShot,
          "--width",
          "800",
          "--height",
          "600",
          "--dpr",
          "1",
          "--gpu-adapter",
          gpuAdapter,
        ],
        { env },
      );
      const selfScreenshotMs = performance.now() - selfStart;

      const check = assertCounterRenderedPng(outShot, outSelfShot, 800, 600);

      report.screenshot = {
        gpuAdapter,
        width: check.width,
        height: check.height,
        expectedBackground: "#f8fafc",
        cornerDelta: check.cornerDelta,
        nonShellPixels: check.nonShellPixels,
        primaryBluePixels: check.primaryBluePixels,
        squarkScreenshotMs: Math.round(squarkScreenshotMs * 10) / 10,
        selfBinaryMs: Math.round(selfScreenshotMs * 10) / 10,
        maxDelta: check.maxDelta,
        outShot,
        outSelfShot,
      };
    } finally {
      fs.rmSync(xdgDir, { recursive: true, force: true });
    }
  }

  fs.writeFileSync(path.join(outDir, "report.json"), JSON.stringify(report, null, 2) + "\n");
  return report;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = runCounterBuildSmoke();
  console.log(JSON.stringify(report, null, 2));
}
