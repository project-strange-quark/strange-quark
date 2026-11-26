// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { cmdShimLine, needsShell, parseDuration, spawnPlan } from "./with-timeout.mjs";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "with-timeout.mjs");
const node = process.execPath;

function withTimeout(duration, command, args = []) {
  return spawnSync(node, [script, duration, command, ...args], { encoding: "utf8" });
}

function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

async function waitForDeath(pid, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!isAlive(pid)) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return !isAlive(pid);
}

test("parseDuration accepts plain seconds and unit suffixes", () => {
  assert.equal(parseDuration("90"), 90_000);
  assert.equal(parseDuration("1.5s"), 1_500);
  assert.equal(parseDuration("2m"), 120_000);
  assert.equal(parseDuration("1h"), 3_600_000);
  assert.throws(() => parseDuration("0"));
  assert.throws(() => parseDuration("soon"));
});

test("returns the child's exit code", () => {
  const result = withTimeout("10", node, ["-e", "process.exit(7)"]);
  assert.equal(result.status, 7, result.stderr);
});

test("needsShell routes only Windows .cmd and .bat shims through a shell", () => {
  assert.equal(needsShell("npm.cmd", "win32"), true);
  assert.equal(needsShell("C:\\tools\\Build.BAT", "win32"), true);
  assert.equal(needsShell("node.exe", "win32"), false);
  assert.equal(needsShell("npm.cmd", "linux"), false);
  assert.equal(needsShell("npm.cmd", "darwin"), false);
});

test("runs a .cmd shim on Windows", { skip: process.platform !== "win32" }, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-with-timeout-"));
  try {
    const shim = path.join(dir, "exit-five.cmd");
    fs.writeFileSync(shim, "@exit /b 5\r\n");
    const result = withTimeout("10", shim);
    assert.equal(result.status, 5, result.stderr);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("cmdShimLine quotes every argument and escapes cmd metacharacters", () => {
  assert.equal(
    cmdShimLine("npm.cmd", ["a b", 'q"t', "50%", "dir\\"]),
    'npm.cmd ^^^"a^^^ b^^^" ^^^"q\\^^^"t^^^" ^^^"50^^^%^^^" ^^^"dir\\\\^^^"',
  );
  assert.equal(cmdShimLine("C:\\Program Files\\x.cmd", []), "C:\\Program^ Files\\x.cmd");
});

test("spawnPlan sends only Windows shims through cmd.exe", () => {
  assert.deepEqual(spawnPlan("npm", ["test"], "linux"), {
    command: "npm",
    args: ["test"],
    options: {},
  });
  const plan = spawnPlan("npm.cmd", ["test"], "win32");
  assert.deepEqual(plan.args, ["/d", "/s", "/c", '"npm.cmd ^^^"test^^^""']);
  assert.deepEqual(plan.options, { windowsVerbatimArguments: true });
});

test(
  "passes arguments through a .cmd shim unchanged on Windows",
  { skip: process.platform !== "win32" },
  () => {
    // A directory with a space, and a shim shaped like npm.cmd: it hands %*
    // to node, which prints the arguments it got.
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark with-timeout "));
    try {
      const printer = path.join(dir, "print-args.mjs");
      fs.writeFileSync(printer, "console.log(JSON.stringify(process.argv.slice(2)));\n");
      const shim = path.join(dir, "print-args.cmd");
      fs.writeFileSync(shim, `@"${node}" "%~dp0print-args.mjs" %*\r\n`);
      const args = [
        "a b",
        "x&echo injected",
        "pipe|more",
        "50%",
        "%PATH%",
        'say "hi"',
        "trailing\\",
        "(paren) ^caret! <in> >out",
        "",
      ];
      const result = withTimeout("10", shim, args);
      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(JSON.parse(result.stdout.trim()), args);
      assert.doesNotMatch(result.stderr, /DEP0190/);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  },
);

test("returns 125 when the command cannot be spawned", () => {
  const start = Date.now();
  const result = withTimeout("30", "definitely-not-a-real-command-squark");
  assert.equal(result.status, 125, result.stderr);
  assert.ok(Date.now() - start < 5000, "does not wait for the deadline");
});

test("returns 124 when the deadline passes", () => {
  const result = withTimeout("0.3", node, ["-e", "setInterval(() => {}, 1000)"]);
  assert.equal(result.status, 124, result.stderr);
});

test("kills grandchildren when the deadline passes", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-with-timeout-"));
  try {
    const pidFile = path.join(dir, "grandchild.pid");
    const parent = `
      import { spawn } from "node:child_process";
      import fs from "node:fs";
      const grandchild = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
        stdio: "ignore",
      });
      fs.writeFileSync(${JSON.stringify(pidFile)}, String(grandchild.pid));
      setInterval(() => {}, 1000);
    `;
    const result = withTimeout("0.7", node, ["-e", parent]);
    assert.equal(result.status, 124, result.stderr);

    const pid = Number(fs.readFileSync(pidFile, "utf8"));
    assert.ok(pid > 0, "the parent wrote the grandchild pid");
    assert.equal(await waitForDeath(pid), true, `grandchild ${pid} is still running`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
