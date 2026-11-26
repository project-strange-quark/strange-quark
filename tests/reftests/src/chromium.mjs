// SPDX-License-Identifier: MIT
// Renders HTML/CSS reftest cases in headless Chromium via playwright-core.

import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { chromium } from "playwright-core";
import { findChromiumPath } from "../../../scripts/chromium-path.mjs";

export { findChromiumPath };

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
};

export function startStaticServer(rootDir) {
  const resolvedRoot = path.resolve(rootDir);
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const reqUrl = new URL(req.url || "/", "http://127.0.0.1");
      const safeRel = path.normalize(decodeURIComponent(reqUrl.pathname)).replace(/^(\.\.[/\\])+/, "");
      const target = path.join(resolvedRoot, safeRel);
      if (!target.startsWith(resolvedRoot) || !fs.existsSync(target) || fs.statSync(target).isDirectory()) {
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

/**
 * Renders a list of discovered reftest cases in headless Chromium and writes
 * each PNG to `<outDir>/<case.id>.png`.
 */
export async function renderCasesInChromium(cases, casesRoot, outDir, options = {}) {
  const timeoutMs = options.timeoutMs ?? 30_000;
  const executablePath = options.executablePath ?? findChromiumPath();

  fs.mkdirSync(outDir, { recursive: true });
  const server = await startStaticServer(casesRoot);
  let browser = null;

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

    const concurrency = Math.max(1, Math.min(options.concurrency ?? 4, cases.length || 1));
    const rendered = new Array(cases.length);
    let nextIndex = 0;

    async function worker() {
      while (true) {
        const idx = nextIndex++;
        if (idx >= cases.length) return;
        const testCase = cases[idx];
        const { width, height, dpr, waitMs } = testCase.meta;
        const context = await browser.newContext({
          viewport: { width, height },
          deviceScaleFactor: dpr,
        });

        try {
          const page = await context.newPage();
          page.setDefaultTimeout(timeoutMs);
          await page.goto(`${server.url}${testCase.relUrlPath}`, { waitUntil: "load" });
          const geometry = await page.evaluate(async () => {
            if (document.fonts && document.fonts.ready) {
              await document.fonts.ready;
            }
            await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));

            const round = (n) => Math.round(n * 1000) / 1000;
            const toRect = (r) => ({
              x: round(r.x),
              y: round(r.y),
              width: round(r.width),
              height: round(r.height),
            });
            const nodes = Array.from(document.querySelectorAll("[data-geom]"));
            if (nodes.length === 0) return null;
            const out = {};
            for (const el of nodes) {
              const key = el.getAttribute("data-geom");
              const range = document.createRange();
              range.selectNodeContents(el);
              out[key] = {
                boundingRect: toRect(el.getBoundingClientRect()),
                clientRects: Array.from(el.getClientRects()).map(toRect),
                rangeRects: Array.from(range.getClientRects()).map(toRect),
              };
            }
            return out;
          });
          if (waitMs > 0) {
            await page.waitForTimeout(waitMs);
          }

          const outPath = path.join(outDir, `${testCase.id}.png`);
          fs.mkdirSync(path.dirname(outPath), { recursive: true });
          await page.screenshot({ path: outPath, type: "png" });
          const entry = { id: testCase.id, outPath, width, height, dpr };
          if (geometry) {
            entry.geometry = geometry;
          }
          rendered[idx] = entry;
        } finally {
          await context.close();
        }
      }
    }

    await Promise.all(Array.from({ length: concurrency }, () => worker()));
    return rendered;
  } finally {
    if (browser) {
      await browser.close();
    }
    await server.close();
  }
}
