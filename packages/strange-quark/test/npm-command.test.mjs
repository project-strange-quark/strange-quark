// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { npmCommand } from "../../../scripts/npm/npm-command.mjs";

test("npmCommand runs npm and npx by name off Windows", () => {
  assert.deepEqual(npmCommand("npm", ["pack"], { platform: "linux" }), { command: "npm", args: ["pack"] });
  assert.deepEqual(npmCommand("npx", ["squark"], { platform: "darwin" }), { command: "npx", args: ["squark"] });
  assert.deepEqual(npmCommand("codesign", ["--verify"], { platform: "win32" }), {
    command: "codesign",
    args: ["--verify"],
  });
});

test("npmCommand runs the CLI scripts with node on Windows", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-command-"));
  try {
    const bin = path.join(root, "node_modules", "npm", "bin");
    fs.mkdirSync(bin, { recursive: true });
    for (const name of ["npm-cli.js", "npx-cli.js"]) fs.writeFileSync(path.join(bin, name), "");
    const execPath = path.join(root, "node.exe");
    const options = { platform: "win32", env: {}, execPath };
    assert.deepEqual(npmCommand("npm", ["pack", "--json"], options), {
      command: execPath,
      args: [path.join(bin, "npm-cli.js"), "pack", "--json"],
    });
    // npm_execpath, set by npm for its scripts, wins over the copy beside node.
    const other = path.join(root, "other");
    fs.mkdirSync(other);
    fs.writeFileSync(path.join(other, "npx-cli.js"), "");
    const fromEnv = npmCommand("npx", ["squark"], { ...options, env: { npm_execpath: path.join(other, "npm-cli.js") } });
    assert.equal(fromEnv.args[0], path.join(other, "npx-cli.js"));
    assert.throws(() => npmCommand("npm", [], { platform: "win32", env: {}, execPath: path.join(os.tmpdir(), "nowhere", "node.exe") }), /cannot find npm-cli\.js/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
