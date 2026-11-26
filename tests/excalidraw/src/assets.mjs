// SPDX-License-Identifier: MIT
// Asset audit: what the built app asks the browser for, and whether the pack
// can serve it.
//
// Excalidraw's stylesheet lists `url(./Assistant-<weight>.woff2)` next to a CDN
// URL. The app build emits only some of them, so Chromium silently uses the CDN
// while a native pack falls back to a system font. The relative CSS URLs are
// therefore checked against the pack even when Chromium never loads them from
// the app origin, and the Chromium request log catches everything the page
// actually fetches from that origin.

import fs from "node:fs";
import path from "node:path";

import { chromium } from "playwright-core";

import { findChromiumPath } from "../../../scripts/chromium-path.mjs";
import { startStaticServer } from "../../reftests/src/chromium.mjs";

const URL_PATTERN = /url\(\s*(?:"|')?([^)"']+)(?:"|')?\s*\)/g;

/** Pack paths a stylesheet references, resolved against the sheet's own path. */
export function cssRelativeAssets(cssText, cssPackPath) {
  const base = cssPackPath.slice(0, cssPackPath.lastIndexOf("/") + 1);
  const found = new Set();
  for (const match of cssText.matchAll(URL_PATTERN)) {
    const reference = match[1].trim();
    if (
      reference === "" ||
      reference.startsWith("#") ||
      reference.startsWith("data:") ||
      /^[a-z][a-z0-9+.-]*:/i.test(reference)
    ) {
      continue;
    }
    const resolved = new URL(reference, `http://pack.invalid${base}`);
    found.add(resolved.pathname);
  }
  return [...found];
}

/** Pack paths a stylesheet references that the pack cannot serve. */
export function missingCssAssets(cssText, cssPackPath, packPaths) {
  const available = new Set(packPaths);
  return cssRelativeAssets(cssText, cssPackPath).filter((asset) => !available.has(asset));
}

function walk(dir, predicate, root = dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full, predicate, root));
    else if (predicate(full)) out.push(full);
  }
  return out;
}

/**
 * Runs the built app in the pinned Chromium, records every request, and checks
 * the app-origin ones and every CSS-relative asset against the pack.
 */
export async function auditAssets({ buildDir, packPaths, settleMs = 3000, timeoutMs = 120_000 }) {
  const available = new Set(packPaths);
  const missingFromCss = [];
  const cssFiles = walk(buildDir, (file) => file.endsWith(".css")).map((file) =>
    `/${path.relative(buildDir, file).replace(/\\/g, "/")}`,
  );
  for (const cssPath of cssFiles) {
    const text = fs.readFileSync(path.join(buildDir, cssPath.slice(1)), "utf8");
    for (const asset of missingCssAssets(text, cssPath, packPaths)) {
      missingFromCss.push({ requestedBy: cssPath, asset });
    }
  }

  const server = await startStaticServer(buildDir);
  let browser = null;
  const requests = [];
  const responses = [];
  try {
    browser = await chromium.launch({
      executablePath: findChromiumPath(),
      headless: true,
      timeout: timeoutMs,
      args: ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", "--force-color-profile=srgb"],
    });
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    page.on("request", (request) => requests.push({ url: request.url(), type: request.resourceType() }));
    page.on("response", (response) =>
      responses.push({ url: response.url(), status: response.status() }),
    );
    page.on("requestfailed", (request) =>
      responses.push({ url: request.url(), status: 0, failure: request.failure()?.errorText }),
    );
    await page.goto(`${server.url}/index.html`, { waitUntil: "load", timeout: timeoutMs });
    await page.waitForTimeout(settleMs);
  } finally {
    await browser?.close().catch(() => undefined);
    await server.close();
  }

  const origin = new URL(server.url).origin;
  const statusByUrl = new Map(responses.map((response) => [response.url, response.status]));
  const originRequests = requests
    .filter((request) => request.url.startsWith(origin))
    .map((request) => {
      const url = new URL(request.url);
      return {
        path: url.pathname,
        type: request.type,
        status: statusByUrl.get(request.url) ?? null,
      };
    });
  const external = [...new Set(
    requests
      .filter((request) => !request.url.startsWith(origin) && !request.url.startsWith("data:"))
      .map((request) => request.url),
  )];
  const unserved = originRequests.filter(
    (request) => request.status !== null && request.status >= 200 && request.status < 300 && !available.has(request.path),
  );
  const originFailures = originRequests.filter(
    (request) => request.status !== null && (request.status === 0 || request.status >= 400),
  );

  return {
    status: unserved.length === 0 && missingFromCss.length === 0 ? "passing" : "failing",
    origin,
    packEntries: available.size,
    requests: originRequests.length,
    unserved,
    originFailures,
    external,
    cssRelativeMissing: missingFromCss,
    passed: unserved.length === 0 && missingFromCss.length === 0,
  };
}
