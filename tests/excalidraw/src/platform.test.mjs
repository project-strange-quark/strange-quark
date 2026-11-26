// SPDX-License-Identifier: MIT
// How the harness names its reports and finds what `squark build` produced on
// each platform.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { launchable } from "./build.mjs";
import { reportName } from "./e2e.mjs";
import { platformModifiers } from "./flow/drivers.mjs";
import { repoRelative } from "./platform.mjs";

test("Linux keeps the release report names; other platforms add theirs", () => {
  assert.equal(reportName(false, "linux"), "e2e.json");
  assert.equal(reportName(true, "linux"), "e2e-windowed.json");
  assert.equal(reportName(false, "darwin"), "e2e-macos.json");
  assert.equal(reportName(true, "darwin"), "e2e-windowed-macos.json");
  assert.equal(reportName(true, "win32"), "e2e-windowed-windows.json");
  assert.equal(reportName(false, "freebsd"), "e2e-freebsd.json");
});

test("launchable is the binary, its .exe on Windows, or the executable of a macOS bundle", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-launchable-"));
  try {
    const single = path.join(dir, "single");
    fs.writeFileSync(single, Buffer.alloc(10));
    assert.deepEqual(launchable(single), { executable: single, bundle: null, bytes: 10 });

    const windows = path.join(dir, "windows");
    fs.writeFileSync(`${windows}.exe`, Buffer.alloc(3));
    assert.deepEqual(launchable(windows), { executable: `${windows}.exe`, bundle: null, bytes: 3 });

    const binary = path.join(dir, "excalidraw");
    const contents = path.join(`${binary}.app`, "Contents");
    fs.mkdirSync(path.join(contents, "MacOS"), { recursive: true });
    fs.mkdirSync(path.join(contents, "Resources"), { recursive: true });
    fs.writeFileSync(path.join(contents, "MacOS", "excalidraw"), Buffer.alloc(7));
    fs.writeFileSync(path.join(contents, "Resources", "app.sqpak"), Buffer.alloc(5));
    assert.deepEqual(launchable(binary), {
      executable: path.join(contents, "MacOS", "excalidraw"),
      bundle: `${binary}.app`,
      bytes: 12,
    });
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the primary shortcut modifier is Command on macOS and Control elsewhere", () => {
  assert.deepEqual(platformModifiers({ primary: true }, "darwin"), { meta: true });
  assert.deepEqual(platformModifiers({ primary: true, shift: true }, "linux"), { shift: true, ctrl: true });
  assert.deepEqual(platformModifiers({ primary: true }, "win32"), { ctrl: true });
  assert.deepEqual(platformModifiers({ ctrl: true }, "darwin"), { ctrl: true });
});

test("recorded paths are relative with forward slashes on every platform", () => {
  const root = path.resolve("repo-root");
  const runtime = path.join(root, "target", "dist", "squark-runtime.exe");
  assert.equal(repoRelative(root, runtime), "target/dist/squark-runtime.exe");
  const outside = path.resolve("elsewhere", "squark.exe");
  assert.equal(repoRelative(root, outside), outside);
});
