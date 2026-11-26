#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Runs every check required before pushing to main (AGENTS.md). This is the
// portable form of scripts/check.sh: the shell wrapper hands over here so the
// same checks run on Linux, macOS and Windows.

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { runWithTimeout } from "./with-timeout.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
process.chdir(repoRoot);

const NODE = process.execPath;
const NPM = process.platform === "win32" ? "npm.cmd" : "npm";
// Windows launches .cmd shims through cmd.exe; everything else spawns direct.
const shellFor = (command) =>
  process.platform === "win32" && command.toLowerCase().endsWith(".cmd");

function signalExitCode(signal) {
  return 128 + (os.constants.signals[signal] ?? 0);
}

// Throw instead of process.exit() so Node flushes the step headers already
// written to a pipe before the wrapper's exit status reaches the caller.
class CheckFailed extends Error {
  constructor(code) {
    super(`check failed with exit code ${code}`);
    this.code = code;
  }
}

function spawnCommand(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: "inherit",
      shell: shellFor(command),
      windowsHide: true,
    });
    child.on("error", reject);
    child.on("exit", (code, signal) => resolve(code ?? signalExitCode(signal)));
  });
}

function step(label) {
  console.log(`==> ${label}`);
}

async function run(command, args, options = {}) {
  const code =
    options.timeoutSeconds === undefined
      ? await spawnCommand(command, args)
      : await runWithTimeout(command, args, options.timeoutSeconds * 1000, {
          shell: shellFor(command),
        });
  if (code !== 0) throw new CheckFailed(code || 1);
}

async function main() {
  step("Checking vendor/excalidraw submodule cleanliness");
  if (
    fs.existsSync(path.join(repoRoot, "vendor/excalidraw/.git"))
  ) {
    const result = spawnSync("git", ["-C", "vendor/excalidraw", "status", "--porcelain"], {
      encoding: "utf8",
    });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new CheckFailed(result.status ?? 1);
    if (result.stdout.trim()) {
      console.error("ERROR: vendor/excalidraw has uncommitted changes:");
      console.error(result.stdout.trimEnd());
      throw new CheckFailed(1);
    }
  }

  step("Running cargo fmt --all -- --check");
  await run("cargo", ["fmt", "--all", "--", "--check"]);

  step("Building npm workspaces (squark-host and squark-js embed/load squark-dom)");
  await run(NPM, ["install", "--no-audit", "--no-fund", "--silent"]);
  await run(NPM, ["run", "build", "--workspaces", "--if-present"]);

  const chromium = spawnSync(NODE, ["scripts/chromium-path.mjs", "--version"], {
    encoding: "utf8",
  });
  if (chromium.error) throw chromium.error;
  if (chromium.status !== 0) {
    process.stderr.write(chromium.stderr ?? "");
    throw new CheckFailed(chromium.status ?? 1);
  }
  step(`Reference Chromium: ${chromium.stdout.trim()}`);

  step("Running cargo clippy --workspace --all-targets");
  await run("cargo", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]);

  step("Running cargo test --workspace");
  await run("cargo", ["test", "--workspace"]);

  step("Running cargo-deny (if installed)");
  const deny = spawnSync("cargo-deny", ["--version"], { stdio: "ignore" });
  if (!deny.error) {
    await run("cargo", ["deny", "check", "licenses", "bans", "sources"]);
  } else if (process.env.CI === "true") {
    console.error("ERROR: cargo-deny must be installed in CI");
    throw new CheckFailed(1);
  } else {
    console.log("    cargo-deny not installed locally; skipping (enforced in CI)");
  }

  step("Running npm licence check");
  await run(NODE, ["scripts/check-licenses.mjs"]);

  step("Ensuring React 19.0.0 test source is prepared");
  await run(NODE, ["tests/react-dom/prepare.mjs"], { timeoutSeconds: 360 });

  step("Running npm test");
  await run(NPM, ["test"], { timeoutSeconds: 300 });

  console.log("==> All checks passed.");
}

main().catch((error) => {
  if (error instanceof CheckFailed) {
    process.exitCode = error.code;
  } else {
    console.error(`ERROR: ${error.message ?? error}`);
    process.exitCode = 1;
  }
});
