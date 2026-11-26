#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// End-to-end local install smoke test:
//   1. Packs Strange Quark, @strange-quark/vite-plugin, and the host platform package
//      into tarballs in a temp directory.
//   2. Installs those tarballs into a fresh temp project via `npm install`.
//   3. Runs `npx --no-install squark --help` with SQUARK_BIN unset and a clean PATH
//      so the shim must resolve the installed platform package binary.
//   4. Checks that `squark-runtime` sits beside `squark` and JS imports load cleanly.
//
// Usage:
//   node scripts/npm/smoke-test.mjs [--manifest <file>] [--keep-temp]

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { npmCommand } from "./npm-command.mjs";
import { REPO_ROOT, hostPlatformTarget } from "./platforms.mjs";

const npmBin = "npm";
const npxBin = "npx";

const ISOLATED_ENV_KEY = /^(path|squark_bin|squark_runtime_bin)$/i;

/**
 * Copies `env` without the host's PATH or squark binary overrides, then sets one
 * PATH. On Windows the process environment exposes "Path", while deletes on a
 * plain copy are case-sensitive, so every spelling must be dropped first.
 */
export function cleanChildEnv(env, pathValue) {
  const clean = {};
  for (const [key, value] of Object.entries(env)) {
    if (ISOLATED_ENV_KEY.test(key)) continue;
    clean[key] = value;
  }
  clean.PATH = pathValue;
  return clean;
}

/** Runs npm or npx (see npm-command.mjs) and throws unless it succeeds. */
function runChecked(tool, toolArgs, options = {}) {
  const { command: cmd, args } = npmCommand(tool, toolArgs);
  const res = spawnSync(cmd, args, {
    encoding: "utf8",
    shell: false,
    timeout: 120_000,
    ...options,
  });
  if (res.error) {
    throw new Error(`Failed to spawn ${cmd}: ${res.error.message}`);
  }
  if (res.status !== 0) {
    throw new Error(
      `Command failed (${res.status}): ${cmd} ${args.join(" ")}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`,
    );
  }
  return res;
}

function packTarball(pkgDir, destDir, npmEnv) {
  const res = runChecked(npmBin, ["pack", "--json", "--pack-destination", destDir], {
    cwd: pkgDir,
    env: npmEnv,
  });
  const start = res.stdout.indexOf("[");
  const end = res.stdout.lastIndexOf("]");
  const parsed = JSON.parse(res.stdout.slice(start, end + 1));
  return path.join(destDir, parsed[0].filename);
}

export function runSmokeTest({
  repoRoot = REPO_ROOT,
  manifestPath = path.join(REPO_ROOT, "target/npm/pack-manifest.json"),
  keepTemp = false,
} = {}) {
  const host = hostPlatformTarget();
  if (!host) {
    throw new Error(`Unsupported host platform for smoke test: ${process.platform}-${process.arch}`);
  }

  let strangeQuarkDir = path.join(repoRoot, "packages/strange-quark");
  let vitePluginDir = path.join(repoRoot, "packages/@strange-quark/vite-plugin");
  let platformDir = path.join(repoRoot, host.dir);

  if (manifestPath && fs.existsSync(manifestPath)) {
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    const byName = new Map(manifest.packages.map((p) => [p.name, p]));
    if (byName.has("strange-quark")) strangeQuarkDir = byName.get("strange-quark").dir;
    if (byName.has("@strange-quark/vite-plugin")) vitePluginDir = byName.get("@strange-quark/vite-plugin").dir;
    if (byName.has(host.packageName)) {
      const entry = byName.get(host.packageName);
      if (!entry.staged) {
        throw new Error(
          `Host platform package ${host.packageName} is not staged. Build dist binaries with ` +
            `"cargo build --profile dist -p squark-cli -p squark-host" and run "node scripts/npm/stage.mjs" first.`,
        );
      }
      platformDir = entry.dir;
    }
  }

  const expectedSquark = path.join(platformDir, `bin/squark${host.exeSuffix}`);
  const expectedRuntime = path.join(platformDir, `bin/squark-runtime${host.exeSuffix}`);
  if (!fs.existsSync(expectedSquark) || !fs.existsSync(expectedRuntime)) {
    throw new Error(
      `Missing staged binaries in ${platformDir}/bin/. Run "node scripts/npm/stage.mjs" first.`,
    );
  }

  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-smoke-"));
  const tarballsDir = path.join(tmpRoot, "tarballs");
  const projectDir = path.join(tmpRoot, "project");
  const npmCacheDir = path.join(tmpRoot, "npm-cache");
  fs.mkdirSync(tarballsDir, { recursive: true });
  fs.mkdirSync(projectDir, { recursive: true });
  fs.mkdirSync(npmCacheDir, { recursive: true });
  const npmEnv = { ...process.env, npm_config_cache: npmCacheDir };

  try {
    const platformTgz = packTarball(platformDir, tarballsDir, npmEnv);
    const strangeQuarkTgz = packTarball(strangeQuarkDir, tarballsDir, npmEnv);
    const vitePluginTgz = packTarball(vitePluginDir, tarballsDir, npmEnv);

    fs.writeFileSync(
      path.join(projectDir, "package.json"),
      `${JSON.stringify({ name: "squark-npm-smoke-project", private: true, type: "module" }, null, 2)}\n`,
    );

    // Install strictly from the local tarballs; --legacy-peer-deps avoids
    // fetching Vite from the registry during offline/sandboxed smoke tests.
    runChecked(
      npmBin,
      [
        "install",
        "--no-audit",
        "--no-fund",
        "--offline",
        "--legacy-peer-deps",
        platformTgz,
        strangeQuarkTgz,
        vitePluginTgz,
      ],
      { cwd: projectDir, env: npmEnv },
    );

    // Build a restricted PATH containing only node_modules/.bin, Node's bin dir,
    // and base OS dirs so the shim MUST resolve @strange-quark/cli-<host> instead of
    // any repo or global `squark` binary.
    const nodeBinDir = path.dirname(process.execPath);
    const pathSep = process.platform === "win32" ? ";" : ":";
    const baseDirs =
      process.platform === "win32"
        ? [path.join(projectDir, "node_modules/.bin"), nodeBinDir, process.env.SystemRoot ? path.join(process.env.SystemRoot, "System32") : ""]
        : [path.join(projectDir, "node_modules/.bin"), nodeBinDir, "/usr/bin", "/bin"];
    const cleanEnv = cleanChildEnv(process.env, baseDirs.filter(Boolean).join(pathSep));

    const helpRes = runChecked(npxBin, ["--no-install", "squark", "--help"], {
      cwd: projectDir,
      env: cleanEnv,
    });
    if (!helpRes.stdout.includes("Strange Quark")) {
      throw new Error(`Unexpected "npx squark --help" output:\n${helpRes.stdout}`);
    }

    // Verify squark-runtime sits beside squark in the installed platform package AND
    // that `npx squark build --skip-app-build` discovers it automatically without
    // printing a debug-build fallback warning.
    const installedBinDir = path.join(projectDir, "node_modules", host.packageName, "bin");
    const installedSquark = path.join(installedBinDir, `squark${host.exeSuffix}`);
    const installedRuntime = path.join(installedBinDir, `squark-runtime${host.exeSuffix}`);
    if (!fs.existsSync(installedSquark) || !fs.existsSync(installedRuntime)) {
      throw new Error(`Installed platform package is missing squark or squark-runtime in ${installedBinDir}`);
    }

    // The licences of the bundled third-party software ship beside the
    // binaries, and the runtime, which every built app carries, prints them.
    const installedNotices = path.join(installedBinDir, "..", "THIRD_PARTY_NOTICES.txt");
    if (!fs.existsSync(installedNotices)) {
      throw new Error(`Installed platform package is missing ${installedNotices}`);
    }
    const printed = spawnSync(installedRuntime, ["--third-party-notices"], {
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
      timeout: 60_000,
    });
    if (printed.status !== 0 || printed.stdout !== fs.readFileSync(installedNotices, "utf8")) {
      throw new Error(
        `squark-runtime --third-party-notices (status ${printed.status}) did not print THIRD_PARTY_NOTICES.txt:\n${printed.stderr}`,
      );
    }

    const webDir = path.join(projectDir, "web");
    fs.mkdirSync(webDir, { recursive: true });
    fs.writeFileSync(
      path.join(webDir, "index.html"),
      "<!doctype html><html><body><h1>Strange Quark smoke</h1></body></html>\n",
    );
    const outBinary = path.join(projectDir, `smoke-out${host.exeSuffix}`);
    const buildRes = runChecked(
      npxBin,
      ["--no-install", "squark", "build", "--skip-app-build", "--dist", "web", "--out", outBinary],
      { cwd: projectDir, env: cleanEnv },
    );
    if (buildRes.stderr.includes("debug build")) {
      throw new Error(
        `Unexpected debug-build fallback warning from installed squark build:\n${buildRes.stderr}`,
      );
    }
    // macOS gets an app bundle around the unchanged runtime; Linux and
    // Windows get the runtime with the pack appended.
    const bundle = `${outBinary}.app`;
    if (fs.existsSync(bundle)) {
      const executable = path.join(bundle, "Contents", "MacOS", path.basename(outBinary));
      const pack = path.join(bundle, "Contents", "Resources", "app.sqpak");
      // Signing the bundle replaces the runtime's linker signature, so its
      // size changes; it must still be a Mach-O with no pack of its own.
      const magic = fs.readFileSync(executable).subarray(0, 4).toString("hex");
      if (magic !== "cffaedfe" && magic !== "cafebabe") {
        throw new Error(`Expected ${executable} to be a Mach-O executable (magic ${magic})`);
      }
      if (!fs.statSync(pack).isFile()) throw new Error(`Missing bundle pack ${pack}`);
      runChecked("codesign", ["--verify", "--strict", bundle], { cwd: projectDir, env: cleanEnv });
    } else {
      const outSize = fs.statSync(outBinary).size;
      if (outSize <= fs.statSync(installedRuntime).size) {
        throw new Error(`Expected packed binary ${outBinary} (${outSize} B) to be larger than squark-runtime`);
      }
    }

    // Verify ESM exports from Strange Quark and @strange-quark/vite-plugin.
    runChecked(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        `
        import assert from "node:assert/strict";
        import { defineConfig } from "strange-quark";
        import vitePkg from "@strange-quark/vite-plugin/package.json" with { type: "json" };
        const cfg = defineConfig({ name: "smoke-app", window: { width: 800, height: 600 } });
        assert.equal(cfg.name, "smoke-app");
        assert.equal(vitePkg.name, "@strange-quark/vite-plugin");
        `,
      ],
      { cwd: projectDir, env: cleanEnv },
    );

    return {
      hostPackage: host.packageName,
      tarballs: [
        path.basename(platformTgz),
        path.basename(strangeQuarkTgz),
        path.basename(vitePluginTgz),
      ],
      helpFirstLine: helpRes.stdout.trim().split("\n")[0],
      tmpRoot: keepTemp ? tmpRoot : null,
    };
  } finally {
    if (!keepTemp) {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  }
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const args = process.argv.slice(2);
  let manifestPath = path.join(REPO_ROOT, "target/npm/pack-manifest.json");
  let keepTemp = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--manifest") manifestPath = path.resolve(args[++i]);
    else if (args[i] === "--keep-temp") keepTemp = true;
    else if (args[i] === "--help" || args[i] === "-h") {
      console.log("Usage: node scripts/npm/smoke-test.mjs [--manifest <file>] [--keep-temp]");
      process.exit(0);
    } else {
      console.error(`Unknown argument: ${args[i]}`);
      process.exit(2);
    }
  }

  try {
    const result = runSmokeTest({ manifestPath, keepTemp });
    console.log(`Smoke test passed for ${result.hostPackage}:`);
    console.log(`  tarballs: ${result.tarballs.join(", ")}`);
    console.log(`  npx squark --help: ${result.helpFirstLine}`);
    if (result.tmpRoot) {
      console.log(`  temp project kept at: ${result.tmpRoot}`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
