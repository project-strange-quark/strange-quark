// SPDX-License-Identifier: MIT
// Two drivers for one flow description: Chromium through Playwright and
// Strange Quark through test control. Both expose the same methods, so a step is
// either run and asserted in both engines or recorded as a failure in both.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { chromium } from "playwright-core";

import { findChromiumPath } from "../../../../scripts/chromium-path.mjs";
import { SquarkDriver, encodeModifiers, keyToCode } from "../../../compat/src/squark-driver.mjs";
import { startStaticServer } from "../../../reftests/src/chromium.mjs";
import { STATE_PROBE } from "./state.mjs";

const VIEWPORT = { width: 1280, height: 800 };

const CHROMIUM_ARGS = [
  "--no-sandbox",
  "--disable-gpu",
  "--disable-dev-shm-usage",
  "--force-color-profile=srgb",
  "--font-render-hinting=none",
  "--disable-lcd-text",
  "--hide-scrollbars",
];

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Resolves `primary`, the platform's shortcut modifier: Command on macOS and
 * Control elsewhere, the same choice apps make from navigator.platform. Flow
 * steps press `{ primary: true }` for select-all, undo, redo, zoom and paste.
 */
export function platformModifiers(modifiers = {}, platform = process.platform) {
  const { primary, ...rest } = modifiers;
  if (!primary) return rest;
  return platform === "darwin" ? { ...rest, meta: true } : { ...rest, ctrl: true };
}

function combo(key, modifiers = {}) {
  const parts = [];
  if (modifiers.ctrl) parts.push("Control");
  if (modifiers.meta) parts.push("Meta");
  if (modifiers.shift) parts.push("Shift");
  if (modifiers.alt) parts.push("Alt");
  parts.push(key);
  return parts.join("+");
}

class BaseDriver {
  async state() {
    return this.eval(STATE_PROBE);
  }

  async canvasRect() {
    const state = await this.state();
    return state.dom.canvas;
  }

  async selectTool(name) {
    await this.click(`[data-testid="toolbar-${name}"]`);
  }

  /** Drags inside the canvas using fractions of its size. */
  async dragOnCanvas(from, to, steps = 8) {
    const rect = await this.canvasRect();
    if (!rect) throw new Error("the canvas is not laid out");
    const point = (fraction) => ({
      x: rect.x + rect.width * fraction.x,
      y: rect.y + rect.height * fraction.y,
    });
    const start = point(from);
    const end = point(to);
    await this.drag(start, end, steps);
    await sleep(150);
  }

  /** Wheel at a canvas fraction. Positive dy scrolls the scene down. */
  async wheelOnCanvas(fraction, dx, dy, modifiers = {}) {
    const rect = await this.canvasRect();
    if (!rect) throw new Error("the canvas is not laid out");
    await this.wheel(rect.x + rect.width * fraction.x, rect.y + rect.height * fraction.y, dx, dy, modifiers);
    await sleep(200);
  }
}

export class ChromiumExcalidraw extends BaseDriver {
  static async launch({ buildDir, userDataDir, headless = true, width = VIEWPORT.width, height = VIEWPORT.height }) {
    const server = await startStaticServer(buildDir);
    const driver = new ChromiumExcalidraw({ server, userDataDir, headless, width, height });
    await driver._open();
    return driver;
  }

  constructor({ server, userDataDir, headless, width, height }) {
    super();
    this.server = server;
    this.userDataDir = userDataDir ?? fs.mkdtempSync(path.join(os.tmpdir(), "squark-exc-profile-"));
    this.headless = headless;
    this.width = width;
    this.height = height;
    this.context = null;
    this.page = null;
  }

  async _open() {
    this.context = await chromium.launchPersistentContext(this.userDataDir, {
      executablePath: findChromiumPath(),
      headless: this.headless,
      viewport: { width: this.width, height: this.height },
      acceptDownloads: true,
      permissions: ["clipboard-read", "clipboard-write"],
      args: CHROMIUM_ARGS,
    });
    await this.context.grantPermissions(["clipboard-read", "clipboard-write"], {
      origin: this.server.url,
    });
    // Strange Quark keeps the File System Access API undefined, so
    // the app takes browser-fs-access's legacy <input type=file> / a[download]
    // paths. Hide the pickers in Chromium too, so both engines run the same
    // path and Playwright can drive the file chooser and download events.
    await this.context.addInitScript(() => {
      try {
        delete window.showOpenFilePicker;
        delete window.showSaveFilePicker;
        delete window.showDirectoryPicker;
      } catch {
        // Ignore; deleting non-configurable properties is best effort.
      }
    });
    this.page = this.context.pages()[0] ?? (await this.context.newPage());
    this.consoleLog = [];
    this.page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") {
        this.consoleLog.push(`${message.type()}: ${message.text()}`);
      }
    });
    this.page.on("pageerror", (error) => this.consoleLog.push(`pageerror: ${error.message}`));
    await this.page.goto(`${this.server.url}/index.html`, { waitUntil: "load" });
    await this.page.waitForSelector(".excalidraw", { timeout: 60_000 });
    // Excalidraw loads its fonts asynchronously; give it a beat before drawing.
    await this.page.evaluate(() => document.fonts?.ready);
    await sleep(500);
  }

  async close({ keepServer = false } = {}) {
    await this.context?.close().catch(() => undefined);
    this.context = null;
    if (!keepServer) await this.server?.close().catch(() => undefined);
  }

  /** Restart keeps the origin (and so localStorage) and the profile directory. */
  async restart() {
    await this.context?.close().catch(() => undefined);
    this.context = null;
    await this._open();
  }

  async eval(expression) {
    return this.page.evaluate(expression);
  }

  async click(selector) {
    await this.page.click(selector, { timeout: 15_000 });
    await sleep(100);
  }

  async selectTool(name) {
    const clicked = await this.eval(
      `(() => {
        const input = document.querySelector('[data-testid="toolbar-${name}"]');
        const label = input && input.closest("label");
        if (!label) return false;
        label.click();
        return true;
      })()`,
    );
    if (!clicked) throw new Error(`toolbar tool ${name} is not in the DOM`);
    await sleep(150);
  }

  async mainMenu() {
    // The first pointerdown after a file load is swallowed by the app's focus
    // handling, so open the menu and retry once if it did not appear.
    await this.click('[data-testid="main-menu-trigger"]');
    await sleep(200);
    const open = await this.page.evaluate(
      () => !!document.querySelector('[data-testid="image-export-button"]'),
    );
    if (!open) await this.click('[data-testid="main-menu-trigger"]');
    await sleep(200);
  }

  async clickPoint(point) {
    await this.page.mouse.click(point.x, point.y, { clickCount: 1 });
    await sleep(100);
  }

  async dblclick(point) {
    // Playwright's dblclick sends the two presses with detail 1 and 2, the same
    // sequence the native driver sends explicitly.
    await this.page.mouse.dblclick(point.x, point.y);
    await sleep(300);
  }

  async press(key, modifiers = {}) {
    await this.page.keyboard.press(combo(key, platformModifiers(modifiers)));
    await sleep(100);
  }

  async type(text) {
    await this.page.keyboard.type(text, { delay: 15 });
    await sleep(100);
  }

  async drag(from, to, steps = 8) {
    await this.page.mouse.move(from.x, from.y);
    await this.page.mouse.down();
    for (let i = 1; i <= steps; i += 1) {
      await this.page.mouse.move(
        from.x + ((to.x - from.x) * i) / steps,
        from.y + ((to.y - from.y) * i) / steps,
      );
    }
    await this.page.mouse.up();
  }

  /** One continuous pointer path (a freedraw stroke, not separate clicks). */
  async dragPath(points, stepsPerSegment = 4) {
    await this.page.mouse.move(points[0].x, points[0].y);
    await this.page.mouse.down();
    for (let index = 1; index < points.length; index += 1) {
      const from = points[index - 1];
      const to = points[index];
      for (let step = 1; step <= stepsPerSegment; step += 1) {
        await this.page.mouse.move(
          from.x + ((to.x - from.x) * step) / stepsPerSegment,
          from.y + ((to.y - from.y) * step) / stepsPerSegment,
        );
      }
    }
    await this.page.mouse.up();
  }

  async wheel(x, y, dx, dy) {
    await this.page.mouse.move(x, y);
    await this.page.mouse.wheel(dx, dy);
  }

  /**
   * Zoom gesture: a WheelEvent with ctrlKey, which Playwright's mouse.wheel
   * cannot set. Excalidraw's own wheel handler reads event.ctrlKey.
   */
  async zoomAt(point, deltaY) {
    await this.page.evaluate(
      ({ x, y, deltaY: dy }) => {
        const target = document.querySelector(".excalidraw") ?? document.body;
        target.dispatchEvent(
          new WheelEvent("wheel", {
            deltaY: dy,
            deltaMode: 0,
            ctrlKey: true,
            clientX: x,
            clientY: y,
            bubbles: true,
            cancelable: true,
          }),
        );
      },
      { x: point.x, y: point.y, deltaY },
    );
    await sleep(300);
  }

  async screenshot(outPath) {
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    await this.page.screenshot({ path: outPath, type: "png" });
  }

  async toggleDarkMode() {
    await this.mainMenu();
    await this.click('[data-testid="toggle-dark-mode"]');
  }

  async openLibrary() {
    await this.click(".default-sidebar-trigger");
    await sleep(300);
  }

  async openFile(filePath) {
    await this.mainMenu();
    const [chooser] = await Promise.all([
      this.page.waitForEvent("filechooser", { timeout: 15_000 }),
      this.page.click('[data-testid="load-button"]'),
    ]);
    await chooser.setFiles(filePath);
    await sleep(800);
    // The load leaves the menu mounted; a baseline should show the scene only.
    await this.closeDialogs();
    await sleep(300);
  }

  async saveFile(outPath) {
    // "Export" opens the JSON export dialog; its "Save to file" card is the
    // download path (the menu's Save writes to browser storage when the scene
    // has no file handle).
    await this.mainMenu();
    await this.click('[data-testid="json-export-button"]');
    await this.page.waitForSelector(".ExportDialog--json", { timeout: 15_000 });
    const [download] = await Promise.all([
      this.page.waitForEvent("download", { timeout: 30_000 }),
      this.page.getByRole("button", { name: "Save to file" }).click(),
    ]);
    await download.saveAs(outPath);
    await this.closeDialogs();
    return outPath;
  }

  /** Closes Excalidraw modals and menus one layer at a time. */
  async closeDialogs() {
    for (let attempt = 0; attempt < 4; attempt += 1) {
      const open = await this.page.evaluate(() => {
        const modalContent = document.querySelector(".Modal__content");
        if (modalContent instanceof HTMLElement) modalContent.focus();
        return document.querySelectorAll('[role="dialog"], [role="menu"]').length;
      });
      if (open === 0) break;
      await this.page.keyboard.press("Escape").catch(() => undefined);
      await sleep(200);
    }
  }

  async _exportVia(kind, outPath) {
    // Close any dialog or menu left by the previous export before reopening.
    await this.closeDialogs();
    await this.mainMenu();
    await this.click('[data-testid="image-export-button"]');
    await this.page.waitForSelector(".ImageExportModal", { timeout: 15_000 });
    const label = kind === "png" ? "Export to PNG" : "Export to SVG";
    const [download] = await Promise.all([
      this.page.waitForEvent("download", { timeout: 60_000 }),
      this.page.getByRole("button", { name: label }).click(),
    ]);
    await download.saveAs(outPath);
    await this.closeDialogs();
    return outPath;
  }

  async exportPng(outPath) {
    return this._exportVia("png", outPath);
  }

  async exportSvg(outPath) {
    return this._exportVia("svg", outPath);
  }

  /**
   * Chromium clipboard: a synthetic paste event with a real image/png File in
   * its DataTransfer, which is the app's own paste handler input. The native
   * leg needs a host clipboard stand-in instead (see the harness report).
   */
  async pasteImage(imagePath) {
    // The app ignores paste while a dialog is open, when focus is outside the
    // canvas container, or when the last pointer position is not over a canvas.
    // Close dialogs, click a canvas point, and paste without blurring; retry a
    // couple of times because Radix popovers can still hold focus.
    const bytes = fs.readFileSync(imagePath);
    const points = [
      { fx: 0.5, fy: 0.5 },
      { fx: 0.42, fy: 0.58 },
      { fx: 0.62, fy: 0.42 },
    ];
    for (const point of points) {
      await this.closeDialogs();
      await this.selectTool("selection");
      const rect = await this.canvasRect();
      await this.page.mouse.click(rect.x + rect.width * point.fx, rect.y + rect.height * point.fy);
      await sleep(200);
      await this.page.evaluate(
        ({ base64, name }) => {
          const binary = atob(base64);
          const data = new Uint8Array(binary.length);
          for (let i = 0; i < binary.length; i += 1) data[i] = binary.charCodeAt(i);
          const file = new File([data], name, { type: "image/png" });
          const transfer = new DataTransfer();
          transfer.items.add(file);
          const event = new ClipboardEvent("paste", { clipboardData: transfer, bubbles: true });
          document.body.dispatchEvent(event);
        },
        { base64: bytes.toString("base64"), name: path.basename(imagePath) },
      );
      await sleep(1200);
      const state = await this.state();
      const images = (state.scene?.elements ?? []).filter((element) => element.type === "image");
      if (images.length > 0) return;
    }
  }

  /** IME composition through the DevTools protocol, into the focused textarea. */
  async composeIme(text, { commit = true } = {}) {
    const session = await this.context.newCDPSession(this.page);
    await session.send("Input.imeSetComposition", {
      text,
      selectionStart: text.length,
      selectionEnd: text.length,
    });
    await sleep(150);
    if (commit) {
      await session.send("Input.insertText", { text });
      await sleep(150);
    }
    await session.detach().catch(() => undefined);
  }
}

export class NativeExcalidraw extends BaseDriver {
  static async launch({ binaryPath, runtimeBin, workDir, xdgDataHome = null, headless = true, gpuAdapter = null, timeoutMs = 120_000 }) {
    const ownXdg = xdgDataHome ?? fs.mkdtempSync(path.join(os.tmpdir(), "squark-exc-xdg-"));
    const driver = new NativeExcalidraw({
      binaryPath,
      runtimeBin,
      workDir,
      xdgDataHome: ownXdg,
      headless,
      gpuAdapter,
      timeoutMs,
    });
    await driver._open();
    return driver;
  }

  constructor({ binaryPath, runtimeBin, workDir, xdgDataHome, headless, gpuAdapter, timeoutMs }) {
    super();
    this.binaryPath = binaryPath;
    this.runtimeBin = runtimeBin;
    this.workDir = workDir;
    this.xdgDataHome = xdgDataHome;
    this.headless = headless;
    this.gpuAdapter = gpuAdapter;
    this.timeoutMs = timeoutMs;
    this.driver = null;
  }

  async _open() {
    this.driver = await SquarkDriver.launch({
      binaryPath: this.binaryPath,
      headless: this.headless,
      width: VIEWPORT.width,
      height: VIEWPORT.height,
      gpuAdapter: this.gpuAdapter === "auto" ? null : this.gpuAdapter,
      env: { XDG_DATA_HOME: this.xdgDataHome },
      timeoutMs: this.timeoutMs,
    });
    // A real window needs presented frames and a mounted app before its
    // surface accepts input. `frame.waitIdle` never goes quiet while
    // Excalidraw animates, so wait for the shell and one presentation.
    if (!this.headless) {
      await this.driver.waitForSelector('[data-testid="main-menu-trigger"]', 60_000);
      await this.driver.nextPresented().catch(() => null);
      await sleep(500);
    }
  }

  /**
   * A real window tracks a cursor position, so a press without a preceding
   * move can land before the surface sees the pointer. Headless injection has
   * no cursor and needs no move.
   */
  async _hoverForPress(point) {
    if (this.headless) return;
    await this.driver.request("input.pointer", {
      kind: "move",
      x: point.x,
      y: point.y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
    });
    await sleep(30);
  }

  async close() {
    await this.driver?.close().catch(() => undefined);
    this.driver = null;
  }

  /** Restart reuses the same XDG_DATA_HOME, so the app's storage comes back. */
  async restart() {
    await this.close();
    await this._open();
  }

  async eval(expression) {
    return this.driver.eval(expression);
  }

  async click(selector) {
    await this.driver.click(selector);
    await sleep(100);
  }

  async mainMenu() {
    await this.click('[data-testid="main-menu-trigger"]');
    await sleep(200);
    const open = await this.eval(`!!document.querySelector('[data-testid="image-export-button"]')`);
    if (!open) await this.click('[data-testid="main-menu-trigger"]');
    await sleep(200);
  }

  /** Closes Excalidraw modals and menus one layer at a time. */
  async closeDialogs() {
    for (let attempt = 0; attempt < 4; attempt += 1) {
      const open = await this.eval(`(() => {
        const modalContent = document.querySelector(".Modal__content");
        if (modalContent && typeof modalContent.focus === "function") modalContent.focus();
        return document.querySelectorAll('[role="dialog"], [role="menu"]').length;
      })()`);
      if (open === 0) break;
      await this.press("Escape");
      await sleep(150);
    }
  }

  async clickPoint(point, clickCount = 1) {
    await this._hoverForPress(point);
    await this.driver.request("input.pointer", {
      kind: "down",
      x: point.x,
      y: point.y,
      button: 0,
      buttons: 1,
      modifiers: 0,
      pointerId: 1,
      clickCount,
    });
    await this.driver.request("input.pointer", {
      kind: "up",
      x: point.x,
      y: point.y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
      clickCount,
    });
    await sleep(100);
  }

  /** clickCount 1 then 2, so the host does not depend on its timing rule. */
  async dblclick(point) {
    const started = performance.now();
    await this.clickPoint(point, 1);
    const firstDone = performance.now();
    await this.clickPoint(point, 2);
    this.lastDoubleClick = {
      counts: [1, 2],
      firstPressMs: Number((firstDone - started).toFixed(1)),
      gapMs: Number((performance.now() - started).toFixed(1)),
      note: "clickCount carries the press pair; the gap can exceed the 500 ms timing rule",
    };
    await sleep(300);
  }

  async press(key, modifiers = {}) {
    const bits = encodeModifiers(platformModifiers(modifiers));
    await this.driver.request("input.key", {
      kind: "down",
      key,
      code: keyToCode(key),
      modifiers: bits,
    });
    await this.driver.request("input.key", {
      kind: "up",
      key,
      code: keyToCode(key),
      modifiers: bits,
    });
    await sleep(100);
  }

  async type(text) {
    for (const character of text) {
      await this.driver.request("input.key", {
        kind: "down",
        key: character,
        code: keyToCode(character),
        text: character,
        modifiers: 0,
      });
      await this.driver.request("input.key", {
        kind: "up",
        key: character,
        code: keyToCode(character),
        modifiers: 0,
      });
    }
    await sleep(150);
  }

  async drag(from, to, steps = 8) {
    await this._hoverForPress(from);
    await this.driver.drag(from, to, steps);
    await sleep(150);
  }

  async dragPath(points, stepsPerSegment = 4) {
    const [first] = points;
    await this._hoverForPress(first);
    await this.driver.request("input.pointer", {
      kind: "down",
      x: first.x,
      y: first.y,
      button: 0,
      buttons: 1,
      modifiers: 0,
      pointerId: 1,
    });
    for (let index = 1; index < points.length; index += 1) {
      const from = points[index - 1];
      const to = points[index];
      for (let step = 1; step <= stepsPerSegment; step += 1) {
        await this.driver.request("input.pointer", {
          kind: "move",
          x: from.x + ((to.x - from.x) * step) / stepsPerSegment,
          y: from.y + ((to.y - from.y) * step) / stepsPerSegment,
          button: 0,
          buttons: 1,
          modifiers: 0,
          pointerId: 1,
        });
      }
    }
    const last = points[points.length - 1];
    await this.driver.request("input.pointer", {
      kind: "up",
      x: last.x,
      y: last.y,
      button: 0,
      buttons: 0,
      modifiers: 0,
      pointerId: 1,
    });
    await sleep(150);
  }

  async wheel(x, y, dx, dy) {
    await this.driver.request("input.wheel", { x, y, dx, dy, modifiers: 0 });
  }

  async zoomAt(point, deltaY) {
    await this.driver.request("input.wheel", {
      x: point.x,
      y: point.y,
      dx: 0,
      dy: deltaY,
      modifiers: encodeModifiers({ ctrl: true }),
    });
    await sleep(300);
  }

  async screenshot(outPath) {
    await this.driver.screenshot(outPath);
  }

  async toggleDarkMode() {
    await this.mainMenu();
    await this.click('[data-testid="toggle-dark-mode"]');
  }

  async openLibrary() {
    await this.click(".default-sidebar-trigger");
    await sleep(300);
  }

  /** Queues the host's next open dialog answer, then runs the app's Open. */
  async openFile(filePath) {
    await this.driver.request("fileDialog.queue", { kind: "open", path: filePath });
    await this.closeDialogs();
    await this.mainMenu();
    await this.click('[data-testid="load-button"]');
    await sleep(900);
    await this.closeDialogs();
  }

  /** Queues the next save answer and runs the app's JSON export download. */
  async saveFile(outPath) {
    await this.driver.request("fileDialog.queue", { kind: "save", path: outPath });
    await this.closeDialogs();
    await this.mainMenu();
    await this.click('[data-testid="json-export-button"]');
    await this.driver.waitForSelector(".ExportDialog--json", 15_000);
    await this.click('[aria-label="Save to file"]');
    await sleep(800);
    await this.closeDialogs();
  }

  async _exportVia(kind, outPath) {
    await this.driver.request("fileDialog.queue", { kind: "save", path: outPath });
    await this.closeDialogs();
    await this.mainMenu();
    await this.click('[data-testid="image-export-button"]');
    await this.driver.waitForSelector(".ImageExportModal", 15_000);
    const label = kind === "png" ? "Export to PNG" : "Export to SVG";
    await this.click(`[aria-label="${label}"]`);
    await sleep(1000);
    await this.closeDialogs();
  }

  async exportPng(outPath) {
    await this._exportVia("png", outPath);
  }

  async exportSvg(outPath) {
    await this._exportVia("svg", outPath);
  }

  /**
   * Puts the image on the isolated test-control clipboard, focuses the canvas,
   * and sends Ctrl+V, so squark-dom dispatches a paste event with the image.
   */
  async pasteImage(imagePath) {
    const bytes = fs.readFileSync(imagePath);
    const written = await this.driver.request("clipboard.write", {
      items: [{ mime: "image/png", base64: bytes.toString("base64") }],
    });
    const read = await this.driver.request("clipboard.read", {});
    await this.selectTool("selection");
    const rect = await this.canvasRect();
    await this.clickPoint({ x: rect.x + rect.width * 0.5, y: rect.y + rect.height * 0.5 });
    await this.press("v", { primary: true });
    await sleep(800);
    this.lastClipboard = {
      written,
      items: (read?.items ?? []).map((item) => ({ mime: item.mime, bytes: Math.floor((item.base64.length * 3) / 4) })),
    };
  }

  /**
   * Feeds the host's IME task the same preedit/commit pair a Wayland IME
   * sends, so the focused textarea sees trusted composition events.
   */
  async composeIme(text, { commit = true } = {}) {
    const cursor = [Buffer.byteLength(text, "utf8"), Buffer.byteLength(text, "utf8")];
    await this.driver.request("input.ime", { kind: "preedit", text, cursor });
    await sleep(150);
    if (commit) {
      await this.driver.request("input.ime", { kind: "commit", text });
      await sleep(150);
    }
    this.lastIme = { text, cursor, committed: commit };
  }
}

export { sleep };
