#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Root `npm test`: runs every workspace's test script when the repo has any
// workspaces, and succeeds when it has none. It works the same on every
// platform.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WORKSPACE_DIRS = ["packages", "packages/@strange-quark", "apps", "tests"];

function workspacePackages() {
  const packages = [];
  for (const dir of WORKSPACE_DIRS) {
    let entries;
    try {
      entries = fs.readdirSync(path.join(repoRoot, dir));
    } catch {
      continue;
    }
    for (const entry of entries) {
      const candidate = `${dir}/${entry}`;
      if (fs.existsSync(path.join(repoRoot, candidate, "package.json"))) {
        packages.push(candidate);
      }
    }
  }
  return packages;
}

const packages = workspacePackages();
if (packages.length === 0) process.exit(0);

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const child = spawn(npm, ["test", "--workspaces", "--if-present"], {
  cwd: repoRoot,
  stdio: "inherit",
  shell: process.platform === "win32",
  windowsHide: true,
});
child.on("error", (error) => {
  console.error(`ERROR: ${error.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  process.exit(code ?? 128 + (os.constants.signals[signal] ?? 0));
});
