// SPDX-License-Identifier: MIT
// Build and pack Excalidraw for its harness: the app's own vite build
// with its release env flags (Sentry, PWA and tracking off, collaboration off), packed onto the stripped dist runtime, with the shipped
// size ceiling. The submodule is never modified.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { npmCommand } from "../../../scripts/npm/npm-command.mjs";

/** npx's command and arguments for execFileSync (see npm-command.mjs). */
const npx = (args) => {
  const { command, args: commandArgs } = npmCommand("npx", args);
  return [command, commandArgs];
};

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(__dirname, "../../..");
export const excalidrawDir = path.join(repoRoot, "vendor/excalidraw");
export const appDir = path.join(excalidrawDir, "excalidraw-app");
export const buildDir = path.join(appDir, "build");
export const outDir = path.join(repoRoot, "target/excalidraw-native");
export const MAX_BINARY_BYTES = 150 * 1024 * 1024;

/** Cargo's executables are `.exe` files on Windows. */
const exeSuffix = process.platform === "win32" ? ".exe" : "";

export const squarkBin = process.env.SQUARK_BIN || path.join(repoRoot, `target/debug/squark${exeSuffix}`);
export const gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto";

export const EXCALIDRAW_D2_ENV = Object.freeze({
  VITE_APP_DISABLE_SENTRY: "true",
  VITE_APP_ENABLE_PWA: "false",
  VITE_APP_ENABLE_TRACKING: "false",
  VITE_APP_ENABLE_ESLINT: "false",
  VITE_APP_WS_SERVER_URL: "",
  VITE_APP_FIREBASE_CONFIG: "{}",
});

/** The stripped `[profile.dist]` runtime; a debug runtime is refused. */
export function resolveRuntime() {
  const candidates = [
    process.env.SQUARK_RUNTIME_BIN ? { path: process.env.SQUARK_RUNTIME_BIN, profile: "explicit" } : null,
    { path: path.join(repoRoot, `target/dist/squark-runtime${exeSuffix}`), profile: "dist" },
    { path: path.join(repoRoot, `target/debug/squark-runtime${exeSuffix}`), profile: "debug" },
  ].filter(Boolean);
  const found = candidates.find((candidate) => fs.existsSync(candidate.path));
  if (!found) {
    throw new Error(
      "no squark-runtime found; build the stripped runtime with `cargo build --profile dist -p squark-host --bin squark-runtime`",
    );
  }
  if (found.profile === "debug") {
    throw new Error(
      `only a debug squark-runtime was found at ${found.path}; build the stripped ` +
        "target/dist/squark-runtime with `cargo build --profile dist -p squark-host --bin squark-runtime`",
    );
  }
  return { ...found, bytes: fs.statSync(found.path).size };
}

export function buildWithYarn() {
  assert.ok(
    fs.existsSync(path.join(excalidrawDir, "node_modules")),
    "vendor/excalidraw/node_modules is missing; run npx yarn@1.22.22 --cwd vendor/excalidraw install --ignore-engines --frozen-lockfile",
  );
  execFileSync(
    ...npx(["--yes", "yarn@1.22.22", "--ignore-engines", "--cwd", appDir, "vite", "build"]),
    {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      timeout: 300_000,
      env: { ...process.env, ...EXCALIDRAW_D2_ENV },
    },
  );
  assert.ok(fs.existsSync(path.join(buildDir, "index.html")), `missing ${buildDir}/index.html`);
}

export function submoduleStatus() {
  return execFileSync("git", ["-C", excalidrawDir, "status", "--porcelain"], {
    encoding: "utf8",
    timeout: 60_000,
  }).trim();
}

export function assertSubmoduleClean() {
  const status = submoduleStatus();
  assert.equal(status, "", `vendor/excalidraw is dirty:\n${status}`);
}

/** Packs `build/` onto the dist runtime and enforces the shipped ceiling. */
export function packExcalidraw({ name = "excalidraw-native", outputDir = outDir } = {}) {
  fs.mkdirSync(outputDir, { recursive: true });
  const binary = path.join(outputDir, name);
  fs.rmSync(binary, { force: true });
  fs.rmSync(`${binary}.app`, { recursive: true, force: true });
  fs.rmSync(`${binary}.exe`, { force: true });
  const runtime = resolveRuntime();
  const config = path.join(repoRoot, "apps/excalidraw/squark.config.ts");
  const args = [
    "build",
    appDir,
    "--skip-app-build",
    "--dist",
    buildDir,
    "--runtime",
    runtime.path,
    "--out",
    binary,
  ];
  // The vendored app declares its missing Assistant weights in this config.
  if (fs.existsSync(config)) args.push("--config", config);
  execFileSync(squarkBin, args, { encoding: "utf8", timeout: 300_000, maxBuffer: 32 * 1024 * 1024 });
  const { executable, bundle, bytes } = launchable(binary, name);
  assert.ok(
    bytes <= MAX_BINARY_BYTES,
    `the packed Excalidraw binary is ${(bytes / 1024 / 1024).toFixed(1)} MiB, over the ` +
      `${MAX_BINARY_BYTES / 1024 / 1024} MiB shipped ceiling`,
  );
  return { binary: executable, bundle, runtime, bytes };
}

/**
 * What `squark build --out <binary>` produced. On Linux that is the binary
 * itself, on Windows `<binary>.exe`, and on macOS `<binary>.app`, whose
 * executable runs the pack in `Contents/Resources`. `bundle` is the `.app`
 * path on macOS and null elsewhere. Bytes count the runtime and its pack
 * either way.
 */
export function launchable(binary, name = path.basename(binary)) {
  const bundle = `${binary}.app`;
  if (!fs.existsSync(bundle)) {
    const exe = `${binary}.exe`;
    const file = !fs.existsSync(binary) && fs.existsSync(exe) ? exe : binary;
    return { executable: file, bundle: null, bytes: fs.statSync(file).size };
  }
  const executable = path.join(bundle, "Contents", "MacOS", name);
  const pack = path.join(bundle, "Contents", "Resources", "app.sqpak");
  return { executable, bundle, bytes: fs.statSync(executable).size + fs.statSync(pack).size };
}

export function inspectPack(binary) {
  const info = JSON.parse(
    execFileSync(squarkBin, ["inspect", "--dump-pack", binary, "--json"], {
      encoding: "utf8",
      timeout: 120_000,
      maxBuffer: 32 * 1024 * 1024,
    }),
  );
  return {
    container: info.container,
    packEntries: info.pack?.entry_count ?? null,
    packBytes: info.pack?.total_bytes ?? null,
    documentScripts: info.document?.scripts?.length ?? null,
    binaryBytes: fs.statSync(binary).size,
    crcValid: info.trailer?.crc_valid ?? null,
    paths: (info.pack?.entries ?? []).map((entry) => entry.path),
  };
}
