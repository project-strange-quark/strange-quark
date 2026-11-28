// SPDX-License-Identifier: MIT
// SquarkDriver: NDJSON JSON-RPC 2.0 client over Unix domain socket driving
// `squark-runtime --test-control <socket>`, speaking its NDJSON test-control protocol.

import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { AppDriver, platformModifiers } from "./driver.mjs";

const MOD_SHIFT = 1 << 0;
const MOD_CTRL = 1 << 1;
const MOD_ALT = 1 << 2;
const MOD_META = 1 << 3;

// macOS limits sockaddr_un.sun_path to 104 bytes including the NUL terminator,
// and os.tmpdir() is a ~49-byte /var/folders/... path there.
const UNIX_SUN_PATH_MAX = 103;

/**
 * A fresh test-control socket path. On Windows, where neither Node nor
 * squark-runtime speaks Unix domain sockets, it is a named pipe,
 * `\\.\pipe\squark-<tag>-…`; elsewhere it is a `.sock` file kept short enough
 * to fit within macOS's 104-byte `sun_path` (using `/tmp` on macOS or when
 * `os.tmpdir()` would exceed the limit).
 */
export function controlSocketPath(
  tag = "tc",
  { platform = process.platform, tmpDir = os.tmpdir() } = {},
) {
  const suffix = `${process.pid}-${crypto.randomBytes(4).toString("hex")}`;
  if (platform === "win32") {
    return `\\\\.\\pipe\\squark-${tag}-${suffix}`;
  }
  const preferredDir = platform === "darwin" ? "/tmp" : tmpDir;
  let candidate = path.posix.join(preferredDir, `squark-${tag}-${suffix}.sock`);
  if (Buffer.byteLength(candidate) > UNIX_SUN_PATH_MAX) {
    candidate = path.posix.join("/tmp", `squark-${tag}-${suffix}.sock`);
  }
  if (Buffer.byteLength(candidate) > UNIX_SUN_PATH_MAX) {
    const overhead = Buffer.byteLength(`/tmp/squark--${suffix}.sock`);
    const maxTagBytes = Math.max(0, UNIX_SUN_PATH_MAX - overhead);
    let shortTag = "";
    for (const ch of String(tag)) {
      if (Buffer.byteLength(shortTag + ch) > maxTagBytes) break;
      shortTag += ch;
    }
    candidate = shortTag
      ? `/tmp/squark-${shortTag}-${suffix}.sock`
      : `/tmp/squark-${suffix}.sock`;
  }
  return candidate;
}

export function encodeModifiers(modifiers = {}) {
  let bits = 0;
  if (modifiers.shift) bits |= MOD_SHIFT;
  if (modifiers.ctrl) bits |= MOD_CTRL;
  if (modifiers.alt) bits |= MOD_ALT;
  if (modifiers.meta) bits |= MOD_META;
  return bits;
}

/**
 * Physical KeyboardEvent.code for a key string, matching Chromium's US layout
 * (the values Playwright produces for the same key). Letters and digits map to
 * KeyA/Digit0, unshifted punctuation to Minus/Equal/BracketLeft/..., shifted
 * punctuation to the unshifted physical key, and named keys to their code.
 */
const NAMED_CODES = {
  " ": "Space",
  Enter: "Enter",
  Escape: "Escape",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  Shift: "ShiftLeft",
  Control: "ControlLeft",
  Alt: "AltLeft",
  Meta: "MetaLeft",
  CapsLock: "CapsLock",
  ContextMenu: "ContextMenu",
  Pause: "Pause",
  PrintScreen: "PrintScreen",
  ScrollLock: "ScrollLock",
};

const PUNCTUATION_CODES = {
  "-": "Minus",
  "=": "Equal",
  "[": "BracketLeft",
  "]": "BracketRight",
  "\\": "Backslash",
  ";": "Semicolon",
  "'": "Quote",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "`": "Backquote",
};

const SHIFTED_CODES = {
  "!": "Digit1",
  "@": "Digit2",
  "#": "Digit3",
  $: "Digit4",
  "%": "Digit5",
  "^": "Digit6",
  "&": "Digit7",
  "*": "Digit8",
  "(": "Digit9",
  ")": "Digit0",
  _: "Minus",
  "+": "Equal",
  "{": "BracketLeft",
  "}": "BracketRight",
  "|": "Backslash",
  ":": "Semicolon",
  '"': "Quote",
  "<": "Comma",
  ">": "Period",
  "?": "Slash",
  "~": "Backquote",
};

export function keyToCode(key) {
  if (Object.hasOwn(NAMED_CODES, key)) return NAMED_CODES[key];
  if (/^F([1-9]|1[0-2])$/.test(key)) return key;
  if (/^Numpad[0-9]$/.test(key)) return key;
  if (["NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal", "NumpadEnter"].includes(key)) {
    return key;
  }
  if (Object.hasOwn(PUNCTUATION_CODES, key)) return PUNCTUATION_CODES[key];
  if (Object.hasOwn(SHIFTED_CODES, key)) return SHIFTED_CODES[key];
  if (key.length === 1) {
    const upper = key.toUpperCase();
    if (upper >= "A" && upper <= "Z") return `Key${upper}`;
    if (upper >= "0" && upper <= "9") return `Digit${upper}`;
  }
  return key;
}

function connectSocketWithRetry(socketPath, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    function attempt() {
      const socket = net.createConnection({ path: socketPath });
      const onConnect = () => {
        socket.removeListener("error", onError);
        resolve(socket);
      };
      const onError = (err) => {
        socket.destroy();
        if (Date.now() >= deadline) {
          reject(
            new Error(
              `Timed out after ${timeoutMs}ms connecting to test-control socket ${socketPath}: ${err.message}`,
            ),
          );
          return;
        }
        setTimeout(attempt, 25);
      };
      socket.once("connect", onConnect);
      socket.once("error", onError);
    }
    attempt();
  });
}

export class SquarkDriver extends AppDriver {
  constructor({
    socket,
    child = null,
    socketPath = null,
    ownSocket = false,
    xdgDataHome = null,
    timeoutMs = 15_000,
  }) {
    super();
    this._socket = socket;
    this._child = child;
    this._socketPath = socketPath;
    this._ownSocket = ownSocket;
    this._xdgDataHome = xdgDataHome;
    this._timeoutMs = timeoutMs;
    this._nextId = 1;
    this._buffer = "";
    this._pending = new Map();
    this._closed = false;

    this._socket.setEncoding("utf8");
    this._socket.on("data", (chunk) => this._onData(chunk));
    this._socket.on("close", () => this._rejectAll(new Error("test-control socket closed")));
    this._socket.on("error", (err) => this._rejectAll(err));
  }

  /**
   * Connects directly to an already-listening `--test-control` Unix socket.
   */
  static async connect({ socketPath, timeoutMs = 15_000 }) {
    const socket = await connectSocketWithRetry(socketPath, timeoutMs);
    return new SquarkDriver({ socket, socketPath, ownSocket: false, timeoutMs });
  }

  /**
   * Spawns a native runtime binary with `--test-control <socketPath>` and connects to it.
   */
  static async launch({
    binaryPath,
    args = [],
    socketPath = null,
    width = null,
    height = null,
    dpr = null,
    gpuAdapter = null,
    headless = true,
    env = {},
    timeoutMs = 15_000,
  }) {
    const sock = socketPath ?? controlSocketPath("tc");
    try {
      fs.unlinkSync(sock);
    } catch {
      // Ignore if absent.
    }

    const spawnArgs = [...args, "--test-control", sock];
    if (headless) spawnArgs.push("--headless");
    if (width !== null) spawnArgs.push("--width", String(width));
    if (height !== null) spawnArgs.push("--height", String(height));
    if (dpr !== null) spawnArgs.push("--dpr", String(dpr));
    if (gpuAdapter !== null) spawnArgs.push("--gpu-adapter", String(gpuAdapter));

    // Isolate file-backed localStorage per launch so consecutive runs never inherit state.
    const ownXdgDataHome =
      env.XDG_DATA_HOME === undefined
        ? fs.mkdtempSync(path.join(os.tmpdir(), "squark-xdg-"))
        : null;
    const childEnv = { ...process.env };
    for (const [key, value] of Object.entries(env)) {
      // null removes an inherited variable, so one launch can pin the Vulkan
      // ICD while another falls back to the system default.
      if (value === null) delete childEnv[key];
      else childEnv[key] = value;
    }
    if (ownXdgDataHome) childEnv.XDG_DATA_HOME = ownXdgDataHome;

    const child = spawn(binaryPath, spawnArgs, {
      stdio: ["ignore", "pipe", "pipe"],
      env: childEnv,
    });

    let stderr = "";
    child.stderr?.on("data", (chunk) => {
      stderr = (stderr + String(chunk)).slice(-64 * 1024);
    });
    // Drain stdout so a chatty dev server cannot block on a full pipe.
    let stdout = "";
    child.stdout?.on("data", (chunk) => {
      stdout = (stdout + String(chunk)).slice(-64 * 1024);
    });

    let exitedEarly = null;
    child.once("exit", (code, signal) => {
      exitedEarly = { code, signal };
    });

    try {
      const socket = await new Promise((resolve, reject) => {
        const deadline = Date.now() + timeoutMs;
        function poll() {
          if (exitedEarly) {
            reject(
              new Error(
                `Runtime exited before opening ${sock} (code=${exitedEarly.code}, signal=${exitedEarly.signal}): ${stderr.trim()}`,
              ),
            );
            return;
          }
          const s = net.createConnection({ path: sock });
          s.once("connect", () => resolve(s));
          s.once("error", (err) => {
            s.destroy();
            if (Date.now() >= deadline) {
              reject(
                new Error(
                  `Timed out after ${timeoutMs}ms waiting for ${sock}: ${err.message}\n${stderr.trim()}`,
                ),
              );
              return;
            }
            setTimeout(poll, 25);
          });
        }
        poll();
      });
      const driver = new SquarkDriver({
        socket,
        child,
        socketPath: sock,
        ownSocket: true,
        xdgDataHome: ownXdgDataHome,
        timeoutMs,
      });
      driver._stderrRef = () => stderr;
      return driver;
    } catch (err) {
      child.kill("SIGKILL");
      try {
        fs.unlinkSync(sock);
      } catch {
        // Ignore cleanup failure.
      }
      if (ownXdgDataHome) {
        fs.rmSync(ownXdgDataHome, { recursive: true, force: true });
      }
      throw err;
    }
  }

  _onData(chunk) {
    this._buffer += chunk;
    while (true) {
      const nl = this._buffer.indexOf("\n");
      if (nl === -1) break;
      const line = this._buffer.slice(0, nl).trim();
      this._buffer = this._buffer.slice(nl + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch (err) {
        this._rejectAll(new Error(`Invalid NDJSON from test-control server: ${err.message}`));
        return;
      }
      const entry = this._pending.get(msg.id);
      if (!entry) continue;
      this._pending.delete(msg.id);
      clearTimeout(entry.timer);
      if (msg.error) {
        entry.reject(
          new Error(
            `test-control RPC ${entry.method} failed (${msg.error.code ?? -32000}): ${msg.error.message}`,
          ),
        );
      } else {
        entry.resolve(msg.result);
      }
    }
  }

  _rejectAll(err) {
    for (const [id, entry] of this._pending) {
      clearTimeout(entry.timer);
      entry.reject(err);
      this._pending.delete(id);
    }
  }

  /**
   * Sends a JSON-RPC 2.0 request over the NDJSON Unix socket.
   */
  request(method, params = {}, timeoutMs = this._timeoutMs) {
    if (this._closed) {
      return Promise.reject(new Error(`SquarkDriver is closed (cannot call ${method})`));
    }
    const id = this._nextId++;
    const payload = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this._pending.delete(id);
        reject(new Error(`test-control RPC ${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this._pending.set(id, { method, resolve, reject, timer });
      this._socket.write(payload, (err) => {
        if (err) {
          this._pending.delete(id);
          clearTimeout(timer);
          reject(err);
        }
      });
    });
  }

  /** The runtime/CLI stderr collected so far, for failure diagnostics. */
  stderrText() {
    return this._stderrRef ? this._stderrRef() : "";
  }

  async _resolvePoint(target) {
    if (typeof target !== "string") {
      return { x: target.x, y: target.y };
    }
    const rect = await this.queryRect(target);
    if (!rect) {
      throw new Error(`Selector did not match any visible element: ${target}`);
    }
    return {
      x: rect.x + rect.width / 2,
      y: rect.y + rect.height / 2,
    };
  }

  async click(target, { clickCount = 1 } = {}) {
    const pt = await this._resolvePoint(target);
    await this.request("input.pointer", {
      kind: "down",
      x: pt.x,
      y: pt.y,
      button: 0,
      buttons: 1,
      modifiers: 0,
      pointerId: 1,
      clickCount,
    });
    await this.request("input.pointer", {
      kind: "up",
      x: pt.x,
      y: pt.y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
      clickCount,
    });
  }

  /** Two clicks with clickCount 1 then 2, as Chromium's DevTools does. */
  async dblclick(target) {
    const pt = await this._resolvePoint(target);
    await this.click(pt, { clickCount: 1 });
    await this.click(pt, { clickCount: 2 });
  }

  async move(x, y) {
    await this.request("input.pointer", {
      kind: "move",
      x,
      y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
    });
  }

  async drag(from, to, steps = 5) {
    const n = Math.max(1, steps);
    await this.request("input.pointer", {
      kind: "down",
      x: from.x,
      y: from.y,
      button: 0,
      buttons: 1,
      modifiers: 0,
      pointerId: 1,
      clickCount: 1,
    });
    for (let i = 1; i <= n; i++) {
      const t = i / n;
      await this.request("input.pointer", {
        kind: "move",
        x: from.x + (to.x - from.x) * t,
        y: from.y + (to.y - from.y) * t,
        button: 0,
        buttons: 1,
        modifiers: 0,
        pointerId: 1,
      });
    }
    await this.request("input.pointer", {
      kind: "up",
      x: to.x,
      y: to.y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
      clickCount: 1,
    });
  }

  async wheel(x, y, dx, dy, modifiers = {}) {
    await this.request("input.wheel", {
      x,
      y,
      dx,
      dy,
      modifiers: encodeModifiers(platformModifiers(modifiers)),
    });
  }

  async type(text) {
    for (const ch of text) {
      const code = keyToCode(ch);
      await this.request("input.key", {
        kind: "down",
        key: ch,
        code,
        text: ch,
        modifiers: 0,
      });
      await this.request("input.key", {
        kind: "up",
        key: ch,
        code,
        modifiers: 0,
      });
    }
  }

  async press(key, modifiers = {}) {
    const code = keyToCode(key);
    const resolvedModifiers = platformModifiers(modifiers);
    const modBits = encodeModifiers(resolvedModifiers);
    const printable = key.length === 1 && !resolvedModifiers.ctrl && !resolvedModifiers.alt && !resolvedModifiers.meta;
    await this.request("input.key", {
      kind: "down",
      key,
      code,
      ...(printable ? { text: key } : {}),
      modifiers: modBits,
    });
    await this.request("input.key", {
      kind: "up",
      key,
      code,
      modifiers: modBits,
    });
  }

  async waitForSelector(selector, timeoutMs = 10_000) {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const rect = await this.queryRect(selector);
      if (rect !== null) return;
      if (Date.now() >= deadline) {
        throw new Error(`Timed out after ${timeoutMs}ms waiting for selector ${selector}`);
      }
      await new Promise((r) => setTimeout(r, 25));
    }
  }

  async waitForIdle(frames = 2) {
    const res = await this.request("frame.waitIdle", { frames });
    return res.frameId;
  }

  /**
   * Resolves when the first frame committed after the request has been
   * presented. Headless runs read the frame back instead.
   * Returns `{ frameId, presentedAtMs }`, with the timestamp on the runtime's
   * monotonic clock since process start.
   */
  async nextPresented() {
    return this.request("frame.nextPresented", {});
  }

  async queryText(selector) {
    return this.eval(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)}); return el ? el.textContent : null; })()`,
    );
  }

  async queryAttr(selector, name) {
    return this.eval(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)}); return el ? el.getAttribute(${JSON.stringify(name)}) : null; })()`,
    );
  }

  async queryCount(selector) {
    return this.eval(`document.querySelectorAll(${JSON.stringify(selector)}).length`);
  }

  async queryValue(selector) {
    return this.eval(
      `(() => { const el = document.querySelector(${JSON.stringify(selector)}); return el && "value" in el ? String(el.value) : null; })()`,
    );
  }

  async queryRect(selector) {
    const res = await this.request("dom.queryRect", { selector });
    return res?.rect ?? null;
  }

  async eval(expr) {
    const res = await this.request("dom.eval", { script: expr });
    return res?.value;
  }

  async screenshot(outPath) {
    if (outPath) {
      fs.mkdirSync(path.dirname(outPath), { recursive: true });
    }
    const res = await this.request("frame.screenshot", outPath ? { path: outPath } : {});
    if (res.pngBase64) {
      const buf = Buffer.from(res.pngBase64, "base64");
      if (outPath) {
        fs.writeFileSync(outPath, buf);
      }
      return buf;
    }
    if (res.path && fs.existsSync(res.path)) {
      return fs.readFileSync(res.path);
    }
    if (outPath && fs.existsSync(outPath)) {
      return fs.readFileSync(outPath);
    }
    throw new Error("frame.screenshot returned neither pngBase64 nor a readable path");
  }

  async close() {
    if (this._closed) return;
    try {
      await this.request("app.quit", {}, 2_000);
    } catch {
      // Ignore if remote already exited.
    }
    this._closed = true;
    this._socket.destroy();

    if (this._child && this._child.exitCode === null && !this._child.killed) {
      await new Promise((resolve) => {
        const timer = setTimeout(() => {
          this._child.kill("SIGKILL");
          resolve();
        }, 2_000);
        this._child.once("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }

    if (this._ownSocket && this._socketPath) {
      try {
        fs.unlinkSync(this._socketPath);
      } catch {
        // Ignore cleanup errors.
      }
    }
    if (this._xdgDataHome) {
      fs.rmSync(this._xdgDataHome, { recursive: true, force: true });
    }
  }
}
