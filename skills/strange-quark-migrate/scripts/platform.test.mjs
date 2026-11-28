// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { appArtifactBytes, appExecutable, controlEndpoint, processTreeRssKb } from "./platform.mjs";

test("control endpoints use local named pipes on Windows and short Unix socket paths", () => {
  assert.ok(controlEndpoint("win32").startsWith("\\\\.\\pipe\\sqm-"));
  const first = controlEndpoint("darwin");
  assert.ok(path.isAbsolute(first));
  assert.ok(first.endsWith(".sock"));
  assert.ok(Buffer.byteLength(first) < 104);
  assert.notEqual(controlEndpoint("darwin"), first);
});

test("process RSS includes all descendants of the given PID", () => {
  const rows = "  PID  PPID  RSS\r\n15 10 500\n10 1 1024\n 20  15  246\n21 1 4096";
  assert.equal(processTreeRssKb(rows, 10), 1770);
  assert.equal(processTreeRssKb(rows, 15), 746);
  assert.equal(processTreeRssKb(rows, 99), null);
});

test("app bundles resolve the executable from Info.plist", { skip: process.platform !== "darwin" }, (t) => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "sqm-app-"));
  t.after(() => fs.rmSync(work, { recursive: true, force: true }));
  const bundle = path.join(work, "Renamed.app");
  const contents = path.join(bundle, "Contents");
  fs.mkdirSync(contents, { recursive: true });
  fs.writeFileSync(path.join(contents, "Info.plist"), `<?xml version="1.0"?>
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>My App</string></dict></plist>`);
  const binary = path.join(contents, "MacOS", "My App");
  assert.equal(appExecutable(bundle), binary);
  assert.equal(appExecutable(binary), binary);
});

test("app size includes the runtime, pack and metadata", { skip: process.platform !== "darwin" }, (t) => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "sqm-size-"));
  t.after(() => fs.rmSync(work, { recursive: true, force: true }));
  const bundle = path.join(work, "Fixture.app");
  const binary = path.join(bundle, "Contents", "MacOS", "Fixture");
  for (const [file, bytes] of [
    [binary, 11],
    [path.join(bundle, "Contents", "Resources", "app.sqpak"), 13],
    [path.join(bundle, "Contents", "Info.plist"), 17],
  ]) {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, Buffer.alloc(bytes));
  }
  assert.equal(appArtifactBytes(bundle), 41);
  assert.equal(appArtifactBytes(binary), 41);
});
