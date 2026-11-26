// SPDX-License-Identifier: MIT
// ChromiumDriver: Playwright-backed implementation of AppDriver driving
// /usr/bin/chromium against a built web directory.

import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { chromium } from "playwright-core";
import { AppDriver, platformModifiers } from "./driver.mjs";

const MIME_MAP = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".otf": "font/otf",
  ".ico": "image/x-icon",
};

import { findChromiumPath } from "../../../scripts/chromium-path.mjs";

export { findChromiumPath };

function startStaticServer(rootDir) {
  const resolvedRoot = path.resolve(rootDir);
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const reqUrl = new URL(req.url || "/", "http://127.0.0.1");
      const reqPath = reqUrl.pathname === "/" ? "/index.html" : reqUrl.pathname;
      const safeRel = path
        .normalize(decodeURIComponent(reqPath))
        .replace(/^(\.\.[/\\])+/, "");
      const target = path.join(resolvedRoot, safeRel);
      if (
        !target.startsWith(resolvedRoot) ||
        !fs.existsSync(target) ||
        fs.statSync(target).isDirectory()
      ) {
        if (reqPath === "/favicon.ico") {
          res.writeHead(204);
          res.end();
          return;
        }
        res.writeHead(404);
        res.end("Not Found");
        return;
      }
      const ext = path.extname(target).toLowerCase();
      res.writeHead(200, { "Content-Type": MIME_MAP[ext] || "application/octet-stream" });
      fs.createReadStream(target).pipe(res);
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolve({
        url: `http://127.0.0.1:${addr.port}`,
        close: () => new Promise((r) => server.close(() => r())),
      });
    });
  });
}

function formatKeyChord(key, modifiers = {}) {
  const parts = [];
  if (modifiers.ctrl) parts.push("Control");
  if (modifiers.alt) parts.push("Alt");
  if (modifiers.shift) parts.push("Shift");
  if (modifiers.meta) parts.push("Meta");
  parts.push(key);
  return parts.join("+");
}

export class ChromiumDriver extends AppDriver {
  constructor({ server, browser, context, page, consoleErrors, pageErrors }) {
    super();
    this._server = server;
    this._browser = browser;
    this._context = context;
    this._page = page;
    this.consoleErrors = consoleErrors;
    this.pageErrors = pageErrors;
  }

  /**
   * Launches headless Chromium serving `distDir` and navigates to `/`.
   */
  static async launch({
    distDir,
    width = 800,
    height = 600,
    dpr = 1,
    timeoutMs = 30_000,
    executablePath = findChromiumPath(),
  }) {
    if (!fs.existsSync(path.join(distDir, "index.html"))) {
      throw new Error(`Built app index.html not found in ${distDir}`);
    }
    const server = await startStaticServer(distDir);
    let browser = null;
    let context = null;
    try {
      browser = await chromium.launch({
        executablePath,
        headless: true,
        timeout: timeoutMs,
        args: [
          "--headless=new",
          "--no-sandbox",
          "--disable-gpu",
          "--disable-dev-shm-usage",
          "--force-color-profile=srgb",
          "--font-render-hinting=none",
          "--disable-lcd-text",
          "--hide-scrollbars",
        ],
      });
      context = await browser.newContext({
        viewport: { width, height },
        deviceScaleFactor: dpr,
      });
      await context.grantPermissions(["clipboard-read", "clipboard-write"], {
        origin: server.url,
      });
      const page = await context.newPage();
      page.setDefaultTimeout(timeoutMs);

      const consoleErrors = [];
      const pageErrors = [];
      page.on("console", (msg) => {
        if (msg.type() === "error") consoleErrors.push(msg.text());
      });
      page.on("pageerror", (err) => {
        pageErrors.push(String(err));
      });

      await page.goto(`${server.url}/`, { waitUntil: "load" });
      const driver = new ChromiumDriver({
        server,
        browser,
        context,
        page,
        consoleErrors,
        pageErrors,
      });
      await driver.waitForIdle(2);
      return driver;
    } catch (err) {
      if (context) await context.close().catch(() => undefined);
      if (browser) await browser.close().catch(() => undefined);
      await server.close();
      throw err;
    }
  }

  async click(target) {
    if (typeof target === "string") {
      await this._page.locator(target).first().click();
    } else {
      await this._page.mouse.click(target.x, target.y);
    }
  }

  async dblclick(target) {
    if (typeof target === "string") {
      await this._page.locator(target).first().dblclick();
    } else {
      await this._page.mouse.dblclick(target.x, target.y);
    }
  }

  async move(x, y) {
    await this._page.mouse.move(x, y);
  }

  async drag(from, to, steps = 5) {
    await this._page.mouse.move(from.x, from.y);
    await this._page.mouse.down();
    await this._page.mouse.move(to.x, to.y, { steps });
    await this._page.mouse.up();
  }

  async wheel(x, y, dx, dy, modifiers = {}) {
    modifiers = platformModifiers(modifiers);
    await this._page.mouse.move(x, y);
    const held = [];
    if (modifiers.ctrl) held.push("Control");
    if (modifiers.shift) held.push("Shift");
    if (modifiers.alt) held.push("Alt");
    if (modifiers.meta) held.push("Meta");
    for (const k of held) await this._page.keyboard.down(k);
    try {
      await this._page.mouse.wheel(dx, dy);
    } finally {
      for (const k of held.reverse()) await this._page.keyboard.up(k);
    }
  }

  async type(text) {
    await this._page.keyboard.type(text);
  }

  async press(key, modifiers = {}) {
    await this._page.keyboard.press(formatKeyChord(key, platformModifiers(modifiers)));
  }

  async waitForSelector(selector, timeoutMs = 10_000) {
    await this._page.waitForSelector(selector, { timeout: timeoutMs });
  }

  async waitForIdle(frames = 2) {
    await this._page.evaluate(async (n) => {
      if (document.fonts && document.fonts.ready) {
        await document.fonts.ready;
      }
      for (let i = 0; i < n; i++) {
        await new Promise((r) => requestAnimationFrame(r));
      }
    }, frames);
  }

  async queryText(selector) {
    return this._page.evaluate((sel) => {
      const el = document.querySelector(sel);
      return el ? el.textContent : null;
    }, selector);
  }

  async queryAttr(selector, name) {
    return this._page.evaluate(
      ({ sel, attr }) => {
        const el = document.querySelector(sel);
        return el ? el.getAttribute(attr) : null;
      },
      { sel: selector, attr: name },
    );
  }

  async queryCount(selector) {
    return this._page.evaluate((sel) => document.querySelectorAll(sel).length, selector);
  }

  async queryValue(selector) {
    return this._page.evaluate((sel) => {
      const el = document.querySelector(sel);
      return el && "value" in el ? String(el.value) : null;
    }, selector);
  }

  async queryRect(selector) {
    return this._page.evaluate((sel) => {
      const el = document.querySelector(sel);
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    }, selector);
  }

  async eval(expr) {
    return this._page.evaluate((code) => (0, eval)(code), expr);
  }

  async screenshot(outPath) {
    if (outPath) {
      fs.mkdirSync(path.dirname(outPath), { recursive: true });
    }
    return this._page.screenshot({ path: outPath || undefined, type: "png" });
  }

  async close() {
    try {
      await this._context.close();
    } finally {
      try {
        if (this._browser) await this._browser.close();
      } finally {
        await this._server.close();
      }
    }
  }
}
