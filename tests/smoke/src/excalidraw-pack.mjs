#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Excalidraw v0.18.1 pack smoke test:
// 1. Builds vendor/excalidraw/excalidraw-app with its release env vars (submodule untouched).
// 2. Packs build/ with `squark pack`.
// 3. Verifies via `squark inspect --dump-pack --json` that every non-.map file is present
//    with the right MIME type and that document.bin holds index.html's 5 inline scripts,
//    2 inline styles, module entry, and external stylesheet in exact document order.
// 4. Opens the pack in `squark-runtime` headless without errors and records pack size and
//    startup time to first frame.

import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";
import { npmCommand } from "../../../scripts/npm/npm-command.mjs";

/** npx's command and arguments for execFileSync (see npm-command.mjs). */
const npx = (args) => {
  const { command, args: commandArgs } = npmCommand("npx", args);
  return [command, commandArgs];
};

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

const EXCALIDRAW_D2_ENV = {
  VITE_APP_DISABLE_SENTRY: "true",
  VITE_APP_ENABLE_PWA: "false",
  VITE_APP_ENABLE_TRACKING: "false",
  VITE_APP_ENABLE_ESLINT: "false",
  VITE_APP_WS_SERVER_URL: "",
  VITE_APP_FIREBASE_CONFIG: "{}",
};

function defaultSquarkBin() {
  return process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark");
}

function defaultRuntimeBin() {
  return process.env.SQUARK_RUNTIME_BIN || path.join(repoRoot, "target/debug/squark-runtime");
}

export function isExcalidrawSubmodulePresent() {
  return fs.existsSync(path.join(repoRoot, "vendor/excalidraw/excalidraw-app/package.json"));
}

function assertSubmoduleClean(stage) {
  const status = execFileSync("git", ["-C", path.join(repoRoot, "vendor/excalidraw"), "status", "--porcelain"], {
    encoding: "utf8",
    timeout: 15_000,
  });
  assert.equal(
    status.trim(),
    "",
    `vendor/excalidraw must stay completely unmodified (${stage}), found:\n${status}`,
  );
}

function expectedMimeForFile(relPath) {
  const ext = path.extname(relPath).slice(1).toLowerCase();
  switch (ext) {
    case "html":
    case "htm":
      return "text/html; charset=utf-8";
    case "js":
    case "mjs":
    case "cjs":
      return "text/javascript; charset=utf-8";
    case "css":
      return "text/css; charset=utf-8";
    case "json":
    case "webmanifest":
      return "application/json";
    case "woff2":
      return "font/woff2";
    case "ttf":
      return "font/ttf";
    case "otf":
      return "font/otf";
    case "png":
      return "image/png";
    case "jpg":
    case "jpeg":
      return "image/jpeg";
    case "webp":
      return "image/webp";
    case "svg":
      return "image/svg+xml";
    case "ico":
      return "image/x-icon";
    case "wasm":
      return "application/wasm";
    case "txt":
      return "text/plain; charset=utf-8";
    case "xml":
      return "application/xml";
    default:
      return "application/octet-stream";
  }
}

function walkFiles(dir, baseDir = dir, out = []) {
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      walkFiles(full, baseDir, out);
    } else if (ent.isFile()) {
      out.push(path.relative(baseDir, full).replace(/\\/g, "/"));
    }
  }
  return out;
}

export function runExcalidrawPackSmoke({
  squarkBin = defaultSquarkBin(),
  runtimeBin = defaultRuntimeBin(),
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto",
  outDir = path.join(repoRoot, "target/smoke/excalidraw"),
  skipAppBuild = false,
  skipScreenshot = false,
} = {}) {
  if (!isExcalidrawSubmodulePresent()) {
    return { skipped: true, reason: "vendor/excalidraw submodule is not checked out" };
  }

  const submoduleDir = path.join(repoRoot, "vendor/excalidraw");
  const appDir = path.join(submoduleDir, "excalidraw-app");
  const buildDir = path.join(appDir, "build");
  fs.mkdirSync(outDir, { recursive: true });

  assertSubmoduleClean("before build");

  // 1. Build vendor/excalidraw/excalidraw-app with its release env vars (submodule stays clean).
  let buildMs = 0;
  if (!skipAppBuild) {
    if (!fs.existsSync(path.join(submoduleDir, "node_modules"))) {
      execFileSync(
        ...npx([
          "--yes",
          "yarn@1.22.22",
          "--cwd",
          submoduleDir,
          "install",
          "--ignore-engines",
          "--frozen-lockfile",
        ]),
        {
          cwd: repoRoot,
          stdio: "inherit",
          timeout: 360_000,
          env: { ...process.env, HUSKY: "0" },
        },
      );
    }
    const buildStart = performance.now();
    execFileSync(
      ...npx([
        "--yes",
        "yarn@1.22.22",
        "--ignore-engines",
        "--cwd",
        appDir,
        "vite",
        "build",
      ]),
      {
        cwd: repoRoot,
        encoding: "utf8",
        maxBuffer: 32 * 1024 * 1024,
        timeout: 180_000,
        env: { ...process.env, ...EXCALIDRAW_D2_ENV },
      },
    );
    buildMs = performance.now() - buildStart;
  }

  assert.ok(fs.existsSync(path.join(buildDir, "index.html")), `missing ${buildDir}/index.html`);
  assertSubmoduleClean("after build");

  // 2. Pack build/ into a standalone .sqpak.
  const outSqpak = path.join(outDir, "excalidraw.sqpak");
  if (fs.existsSync(outSqpak)) fs.rmSync(outSqpak);
  const packStart = performance.now();
  execFileSync(squarkBin, ["pack", buildDir, "--out", outSqpak, "--name", "excalidraw"], {
    encoding: "utf8",
    timeout: 60_000,
  });
  const packMs = performance.now() - packStart;
  const packBytes = fs.statSync(outSqpak).size;

  // 3. Verify every non-.map file, MIME type, and document.bin script/style order via `squark inspect`.
  const inspectRaw = execFileSync(squarkBin, ["inspect", "--dump-pack", outSqpak, "--json"], {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    timeout: 60_000,
  });
  const inspection = JSON.parse(inspectRaw);
  assert.equal(inspection.container, "standalone-sqpak");
  assert.equal(inspection.pack.total_bytes, packBytes);

  const diskFiles = walkFiles(buildDir).sort();
  const nonMapDiskFiles = diskFiles.filter((f) => !f.endsWith(".map"));
  const mapDiskFiles = diskFiles.filter((f) => f.endsWith(".map"));

  const entriesByPath = new Map(inspection.pack.entries.map((e) => [e.path, e]));
  assert.ok(entriesByPath.has("/__squark/document.bin"), "missing /__squark/document.bin");
  assert.ok(entriesByPath.has("/__squark/manifest.json"), "missing /__squark/manifest.json");
  assert.equal(
    inspection.pack.entry_count,
    nonMapDiskFiles.length + 2,
    `expected ${nonMapDiskFiles.length} non-.map files + 2 reserved entries`,
  );

  for (const rel of mapDiskFiles) {
    assert.equal(entriesByPath.has(`/${rel}`), false, `sourcemap /${rel} must not be packed`);
  }
  for (const rel of nonMapDiskFiles) {
    const packPath = `/${rel}`;
    const entry = entriesByPath.get(packPath);
    assert.ok(entry, `missing packed entry for ${packPath}`);
    const expectedLen = fs.statSync(path.join(buildDir, rel)).size;
    assert.equal(entry.data_len, expectedLen, `byte length mismatch for ${packPath}`);
    const expectedMime = expectedMimeForFile(rel);
    assert.equal(entry.mime, expectedMime, `MIME mismatch for ${packPath}`);
  }

  // Verify document.bin holds Excalidraw index.html's exact 5 inline scripts,
  // 2 inline styles, module entry, and external stylesheet in document order.
  const doc = inspection.document;
  assert.equal(doc.quirks_mode, "standards");
  assert.equal(doc.inline_styles.length, 2, "expected 2 inline <style> sheets");
  assert.equal(doc.scripts.length, 7, "expected 5 inline scripts + 1 module entry + 1 stylesheet");

  const combined = [
    ...doc.inline_styles.map((s) => ({ kind: "inline-style", node_id: s.node_id, text: s.css })),
    ...doc.scripts.map((s) => ({ kind: s.kind, node_id: s.node_id, text: s.payload })),
  ].sort((a, b) => a.node_id - b.node_id);

  assert.deepEqual(
    combined.map((item) => item.kind),
    [
      "inline-classic",
      "inline-style",
      "inline-classic",
      "inline-classic",
      "inline-classic",
      "inline-style",
      "external-module",
      "external-stylesheet",
      "inline-classic",
    ],
    "document.bin scripts and inline styles must appear in exact index.html document order",
  );
  assert.match(combined[0].text, /excalidraw-theme/);
  assert.match(combined[1].text, /html\.dark\{background-color:#121212/);
  assert.match(combined[2].text, /excplus-autoredirect=true/);
  assert.match(combined[3].text, /EXCALIDRAW_ASSET_PATH/);
  assert.match(combined[4].text, /window\.name\s*=\s*"_excalidraw"/);
  assert.match(combined[5].text, /\.visually-hidden/);
  assert.match(combined[6].text, /^\/assets\/index-.*\.js$/);
  assert.match(combined[7].text, /^\/assets\/index-.*\.css$/);
  assert.match(combined[8].text, /simpleanalyticscdn\.com/);

  const report = {
    skipped: false,
    app: "vendor/excalidraw/excalidraw-app",
    packPath: outSqpak,
    packBytes,
    packMiB: Math.round((packBytes / (1024 * 1024)) * 100) / 100,
    entryCount: inspection.pack.entry_count,
    nonMapFiles: nonMapDiskFiles.length,
    excludedMaps: mapDiskFiles.length,
    inlineStyles: doc.inline_styles.length,
    scripts: doc.scripts.length,
    buildMs: Math.round(buildMs * 10) / 10,
    packMs: Math.round(packMs * 10) / 10,
    headless: null,
  };

  // 4. Open pack in squark-runtime headless without errors and record time to first frame.
  if (!skipScreenshot) {
    const outPng = path.join(outDir, "excalidraw-first-frame.png");
    const timingsMs = [];
    for (let run = 0; run < 3; run++) {
      if (fs.existsSync(outPng)) fs.rmSync(outPng);
      const xdgDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-smoke-excalidraw-xdg-"));
      let res;
      let elapsed;
      try {
        const t0 = performance.now();
        res = spawnSync(
          runtimeBin,
          [
            outSqpak,
            "--screenshot",
            "--out",
            outPng,
            "--width",
            "1280",
            "--height",
            "800",
            "--dpr",
            "1",
            "--gpu-adapter",
            gpuAdapter,
          ],
          { encoding: "utf8", timeout: 60_000, env: { ...process.env, XDG_DATA_HOME: xdgDir } },
        );
        elapsed = performance.now() - t0;
      } finally {
        fs.rmSync(xdgDir, { recursive: true, force: true });
      }
      assert.equal(res.error, undefined, `failed to spawn squark-runtime: ${res.error}`);
      assert.equal(
        res.status,
        0,
        `squark-runtime headless failed on Excalidraw pack (exit ${res.status}):\n${res.stderr}`,
      );
      // Allow Vulkan driver and CSS diagnostic warning lines; reject runtime errors.
      // Also allow the note squark-gpu prints when Canvas 2D rendering is
      // unavailable; native-smoke.mjs checks the canvas output.
      const errorLines = (res.stderr || "")
        .split("\n")
        .filter(
          (line) =>
            line.trim() !== "" &&
            !line.startsWith("WARNING:") &&
            !line.startsWith("CSS warning:") &&
            !/^squark-gpu: canvas \d+ stays transparent until Canvas 2D rendering is available$/.test(
              line,
            ),
        );
      assert.deepEqual(errorLines, [], `unexpected stderr from squark-runtime:\n${res.stderr}`);
      timingsMs.push(Math.round(elapsed * 10) / 10);
    }

    const decoded = PNG.sync.read(fs.readFileSync(outPng));
    assert.equal(decoded.width, 1280);
    assert.equal(decoded.height, 800);

    const sorted = [...timingsMs].sort((a, b) => a - b);
    report.headless = {
      gpuAdapter,
      width: decoded.width,
      height: decoded.height,
      outPng,
      firstRunMs: timingsMs[0],
      medianMs: sorted[Math.floor(sorted.length / 2)],
      runsMs: timingsMs,
    };
  }

  assertSubmoduleClean("after pack & screenshot");
  fs.writeFileSync(path.join(outDir, "report.json"), JSON.stringify(report, null, 2) + "\n");
  return report;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const skipAppBuild = process.argv.includes("--skip-app-build");
  const report = runExcalidrawPackSmoke({ skipAppBuild });
  console.log(JSON.stringify(report, null, 2));
}
