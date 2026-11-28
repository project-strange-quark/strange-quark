// SPDX-License-Identifier: MIT
// Tests for SquarkDriver speaking the 8-method NDJSON test-control protocol
// against the mock Unix-socket server.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";
import { SquarkDriver, controlSocketPath, encodeModifiers, keyToCode } from "./squark-driver.mjs";
import { platformModifiers } from "./driver.mjs";
import { startMockTestControlServer } from "./mock-server.mjs";
import { runCounterScript } from "./scripts/counter.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const mockServerScript = path.join(__dirname, "mock-server.mjs");

test("platformModifiers maps primary to Command on macOS and Control elsewhere", () => {
  assert.deepEqual(platformModifiers({ primary: true, shift: true }, "darwin"), { shift: true, meta: true });
  assert.deepEqual(platformModifiers({ primary: true, shift: true }, "linux"), { shift: true, ctrl: true });
  assert.deepEqual(platformModifiers({ primary: true }, "win32"), { ctrl: true });
  assert.deepEqual(platformModifiers({ ctrl: true }, "darwin"), { ctrl: true });
});

function tempSock(tag) {
  return controlSocketPath(`test-${tag}`);
}

test("SquarkDriver runs the shared counter script over NDJSON Unix socket", async () => {
  const sock = tempSock("counter");
  const screenshotDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-driver-shots-"));
  const server = await startMockTestControlServer(sock, { screenshotMode: "base64" });
  const driver = await SquarkDriver.connect({ socketPath: sock });
  try {
    const result = await runCounterScript(driver, { screenshotDir });
    assert.equal(result.finalCount, "0");
    assert.ok(fs.existsSync(result.screenshots["counter-initial"]));
    assert.ok(fs.existsSync(result.screenshots["counter-after-ops"]));
    const png = PNG.sync.read(fs.readFileSync(result.screenshots["counter-after-ops"]));
    assert.equal(png.width, 800);
    assert.equal(png.height, 600);

    const methods = new Set(server.calls.map((c) => c.method));
    assert.ok(methods.has("input.pointer"));
    assert.ok(methods.has("frame.waitIdle"));
    assert.ok(methods.has("frame.screenshot"));
    assert.ok(methods.has("dom.queryRect"));
    assert.ok(methods.has("dom.eval"));
  } finally {
    await driver.close();
    assert.equal(server.state.quit, true);
    await server.close();
    fs.rmSync(screenshotDir, { recursive: true, force: true });
  }
});

test("SquarkDriver covers drag, wheel, type, press, modifiers, and JSON-RPC error propagation", async () => {
  const sock = tempSock("inputs");
  const server = await startMockTestControlServer(sock);
  const driver = await SquarkDriver.connect({ socketPath: sock });
  try {
    await driver.move(10, 20);
    await driver.drag({ x: 0, y: 0 }, { x: 100, y: 50 }, 2);
    await driver.wheel(50, 50, 0, -120, { ctrl: true, shift: true });
    await driver.type("Hi");
    await driver.press("Enter", { meta: true });
    await driver.press("a", { primary: true });

    const wheelCall = server.calls.find((c) => c.method === "input.wheel");
    assert.deepEqual(wheelCall.params, {
      x: 50,
      y: 50,
      dx: 0,
      dy: -120,
      modifiers: encodeModifiers({ ctrl: true, shift: true }),
    });

    const keyCalls = server.calls.filter((c) => c.method === "input.key");
    assert.equal(keyCalls.length, 8); // H down/up, i down/up, Enter down/up, a down/up
    assert.equal(keyCalls[0].params.code, keyToCode("H"));
    assert.equal(keyCalls[0].params.text, "H");
    assert.equal(keyCalls[4].params.key, "Enter");
    assert.equal(keyCalls[4].params.modifiers, encodeModifiers({ meta: true }));
    assert.equal(keyCalls[6].params.modifiers, encodeModifiers(platformModifiers({ primary: true })));
    assert.equal(keyCalls[7].params.modifiers, encodeModifiers(platformModifiers({ primary: true })));

    await assert.rejects(() => driver.request("unknown.method", {}), /Method not found/);
  } finally {
    await driver.close();
    await server.close();
  }
});

test("SquarkDriver.launch spawns a process with --test-control and shuts down on app.quit", async () => {
  const screenshotDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-driver-spawn-"));
  const driver = await SquarkDriver.launch({
    binaryPath: process.execPath,
    args: [mockServerScript],
    width: 640,
    height: 480,
    headless: true,
    timeoutMs: 10_000,
  });
  try {
    const frameId = await driver.waitForIdle(3);
    assert.equal(frameId, 3);
    assert.ok(driver._xdgDataHome && fs.existsSync(driver._xdgDataHome));
    const xdgPath = driver._xdgDataHome;
    const outPath = path.join(screenshotDir, "spawned.png");
    const pngBuf = await driver.screenshot(outPath);
    const decoded = PNG.sync.read(pngBuf);
    assert.equal(decoded.width, 640);
    assert.equal(decoded.height, 480);
    await driver.close();
    assert.equal(fs.existsSync(xdgPath), false, "temporary XDG_DATA_HOME must be removed on close");
  } finally {
    await driver.close();
    fs.rmSync(screenshotDir, { recursive: true, force: true });
  }
});

test("controlSocketPath names a pipe on Windows and keeps Unix sockets within sun_path", () => {
  const first = controlSocketPath("unit");
  assert.notEqual(first, controlSocketPath("unit"));
  if (process.platform === "win32") {
    assert.match(first, /^\\\\\.\\pipe\\squark-unit-\d+-[0-9a-f]{8}$/);
  } else {
    const expectedDir = process.platform === "darwin" ? "/tmp" : os.tmpdir();
    assert.equal(path.dirname(first), expectedDir);
    assert.match(path.basename(first), /^squark-unit-\d+-[0-9a-f]{8}\.sock$/);
    assert.ok(Buffer.byteLength(first) < 104, `socket path must fit macOS sun_path: ${first}`);
  }

  // Platform-explicit checks independent of the host OS.
  const win = controlSocketPath("unit", { platform: "win32" });
  assert.match(win, /^\\\\\.\\pipe\\squark-unit-\d+-[0-9a-f]{8}$/);

  const macDeepTmp = "/var/folders/ab/cdefghijklmnopqrstuvwxyz012345/T";
  const mac = controlSocketPath("unit", { platform: "darwin", tmpDir: macDeepTmp });
  assert.equal(path.posix.dirname(mac), "/tmp");
  assert.ok(Buffer.byteLength(mac) < 104);

  const macLongTag = controlSocketPath("x".repeat(120), { platform: "darwin", tmpDir: macDeepTmp });
  assert.equal(path.posix.dirname(macLongTag), "/tmp");
  assert.ok(Buffer.byteLength(macLongTag) < 104);

  const linuxDeepTmp = "/very/deep/" + "nested/".repeat(14) + "tmp";
  const linuxFallback = controlSocketPath("unit", { platform: "linux", tmpDir: linuxDeepTmp });
  assert.equal(path.posix.dirname(linuxFallback), "/tmp");
  assert.ok(Buffer.byteLength(linuxFallback) < 104);

  const linuxShort = controlSocketPath("unit", { platform: "linux", tmpDir: "/tmp" });
  assert.equal(path.posix.dirname(linuxShort), "/tmp");
});
