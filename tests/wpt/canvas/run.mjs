#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Canvas 2D WPT subset runner.
//
// The files under vendor/ are unmodified upstream WPT files pinned at
// WPT_REVISION; the BSD 3-Clause licence in ../../vendor/LICENSE.md applies.
// Run the pinned testharness.js in Chromium and the native runtime.

import { execFile } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { chromium } from "playwright-core";

export const WPT_REVISION = "f4d3f2eba09697ebcb2f338b8f2d709a3ce85217";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const vendorRoot = path.join(__dirname, "vendor");
const sharedResources = path.join(repoRoot, "tests/wpt/vendor/resources");

const STATUS = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"];

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".ttf": "font/ttf",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".gif": "image/gif",
  ".json": "application/json; charset=utf-8",
};

import { SquarkDriver } from "../../compat/src/squark-driver.mjs";
import { findChromiumPath } from "../../../scripts/chromium-path.mjs";

export { findChromiumPath };

function statusName(value) {
  return STATUS[value] ?? String(value);
}

/**
 * Serves upstream WPT paths: `/resources/*` from the canvas subset first and
 * the shared DOM subset second, `/html/canvas/*` from the canvas vendor tree.
 */
function startStaticServer() {
  const routes = [
    { prefix: "/resources/", dir: path.join(vendorRoot, "resources") },
    { prefix: "/resources/", dir: sharedResources },
    { prefix: "/html/canvas/", dir: path.join(vendorRoot, "html/canvas") },
    { prefix: "/fonts/", dir: path.join(vendorRoot, "fonts") },
    { prefix: "/images/", dir: path.join(vendorRoot, "images") },
  ];
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const pathname = decodeURIComponent(new URL(req.url || "/", "http://127.0.0.1").pathname);
      for (const route of routes) {
        if (!pathname.startsWith(route.prefix)) continue;
        const rel = path.normalize(pathname.slice(route.prefix.length)).replace(/^(\.\.[/\\])+/, "");
        const target = path.join(route.dir, rel);
        if (!target.startsWith(route.dir) || !fs.existsSync(target) || fs.statSync(target).isDirectory()) {
          continue;
        }
        res.writeHead(200, {
          "Content-Type": MIME[path.extname(target).toLowerCase()] || "application/octet-stream",
          "Cache-Control": "no-store",
        });
        fs.createReadStream(target).pipe(res);
        return;
      }
      res.writeHead(404);
      res.end("Not Found");
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({
        url: `http://127.0.0.1:${port}`,
        close: () => new Promise((r) => server.close(() => r())),
      });
    });
  });
}

// Appended to `testharnessreport.js`: it is the first script that runs after
// `testharness.js`, so the collector is registered before any test starts and
// before the load event the harness waits for.
const REPORT_PROBE = `
;(function () {
  window.__squarkWpt = { done: false, tests: [], harnessStatus: null };
  add_completion_callback(function (tests, harnessStatus) {
    window.__squarkWpt.done = true;
    window.__squarkWpt.harnessStatus = harnessStatus ? harnessStatus.status : null;
    window.__squarkWpt.tests = tests.map(function (test) {
      return { name: test.name || "", status: test.status, message: test.message || "" };
    });
  });
})();
`;

function expectedReason(expected, file, name) {
  return expected[`${file}::${name}`] ?? expected[file] ?? null;
}

function collectResult(file, collected, expected) {
  const subtests = collected.tests.map((test) => {
    const name = test.name || file;
    const reason = expectedReason(expected, file, name);
    const passed = test.status === 0;
    return {
      name,
      status: statusName(test.status),
      message: test.message,
      pass: passed || Boolean(reason),
      expected: reason ?? null,
    };
  });
  if (subtests.length === 0) {
    subtests.push({
      name: file,
      status: "NO_SUBTESTS",
      message: "testharness reported completion with no tests",
      pass: false,
      expected: expectedReason(expected, file, file),
    });
  }
  const failed = subtests.filter((subtest) => !subtest.pass);
  return {
    file,
    status: "ok",
    harnessStatus: statusName(collected.harnessStatus ?? 0),
    pass: failed.length === 0 && (collected.harnessStatus ?? 0) === 0,
    subtestsTotal: subtests.length,
    subtestsPassed: subtests.length - failed.length,
    subtests,
  };
}

async function runCaseInChromium(page, serverUrl, file, expected, timeoutMs) {
  try {
    await page.goto(`${serverUrl}/html/canvas/${file}`, { waitUntil: "load", timeout: timeoutMs });
    await page.waitForFunction(() => window.__squarkWpt?.done === true, null, { timeout: timeoutMs });
    const collected = await page.evaluate(() => window.__squarkWpt);
    return collectResult(file, collected, expected);
  } catch (error) {
    const reason = expectedReason(expected, file, file);
    return {
      file,
      status: "error",
      harnessStatus: null,
      pass: Boolean(reason),
      subtestsTotal: 0,
      subtestsPassed: 0,
      error: error?.message || String(error),
      subtests: [],
    };
  }
}

function summarize(engine, cases, results) {
  const passed = results.filter((result) => result.pass).length;
  const subtestsTotal = results.reduce((sum, result) => sum + result.subtestsTotal, 0);
  const subtestsPassed = results.reduce((sum, result) => sum + result.subtestsPassed, 0);
  return {
    engine,
    revision: WPT_REVISION,
    total: cases.length,
    passed,
    failed: cases.length - passed,
    passRate: cases.length === 0 ? 1 : passed / cases.length,
    subtestsTotal,
    subtestsPassed,
    cases: results,
  };
}

const exec = promisify(execFile);

async function runNativeCases({ cases, squarkBin, runtimeBin, gpuAdapter, env = null, timeoutMs }) {
  if (!fs.existsSync(squarkBin) || !fs.existsSync(runtimeBin)) {
    return {
      ...summarize("native", cases, []),
      status: "blocked",
      reason: "Build squark and squark-runtime before running native Canvas WPT",
    };
  }
  const target = path.join(repoRoot, "target");
  fs.mkdirSync(target, { recursive: true });
  const work = fs.mkdtempSync(path.join(target, "wpt-canvas-"));
  try {
    const staged = path.join(work, "vendor");
    const resources = path.join(staged, "resources");
    fs.cpSync(vendorRoot, staged, { recursive: true, dereference: true });
    fs.cpSync(sharedResources, resources, { recursive: true, force: false, dereference: true });
    fs.appendFileSync(path.join(resources, "testharnessreport.js"), REPORT_PROBE);

    const results = [];
    for (const file of cases) {
      const pack = path.join(work, "case.sqpak");
      const html = path.join(staged, "html/canvas", file);
      let driver;
      try {
        await exec(squarkBin, ["pack", staged, "--html", html, "--out", pack], { timeout: timeoutMs });
        driver = await SquarkDriver.launch({
          binaryPath: runtimeBin,
          args: [pack],
          width: 800,
          height: 600,
          gpuAdapter,
          env,
          timeoutMs,
        });
        const deadline = Date.now() + timeoutMs;
        let report;
        do {
          report = await driver.eval("window.__squarkWpt ?? null");
          if (report?.done) break;
          await sleep(25);
        } while (Date.now() < deadline);
        if (!report?.done) throw new Error("testharness did not report completion");
        results.push(collectResult(file, report, {}));
      } catch (error) {
        results.push({
          file,
          status: "error",
          harnessStatus: null,
          pass: false,
          subtestsTotal: 0,
          subtestsPassed: 0,
          error: error?.message || String(error),
          subtests: [],
        });
      } finally {
        await driver?.close();
      }
    }
    const summary = summarize("native", cases, results);
    return { ...summary, status: summary.failed ? "failing" : "passing" };
  } finally {
    fs.rmSync(work, { force: true, recursive: true });
  }
}

/** Run the pinned Canvas WPT subset in Chromium or the packed native runtime. */
export async function runCanvasWpt({
  engine = "chromium",
  filter = null,
  outPath = null,
  timeoutMs = 30_000,
  squarkBin = process.env.SQUARK_BIN ?? path.join(repoRoot, "target/debug/squark"),
  runtimeBin = process.env.SQUARK_RUNTIME_BIN ?? path.join(repoRoot, "target/debug/squark-runtime"),
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER ?? null,
  env = null,
} = {}) {
  const casesPath = path.join(__dirname, "cases.json");
  const expectedPath = path.join(__dirname, "expected-failures.json");
  let cases = JSON.parse(fs.readFileSync(casesPath, "utf8"));
  const expected = JSON.parse(fs.readFileSync(expectedPath, "utf8"));
  if (filter) cases = cases.filter((file) => file.includes(filter));

  let summary;
  if (engine === "native") {
    summary = await runNativeCases({ cases, squarkBin, runtimeBin, gpuAdapter, env, timeoutMs });
  } else if (engine !== "chromium") {
    throw new Error(`Unknown engine ${engine}: expected chromium or native`);
  } else {
    const server = await startStaticServer();
    let browser = null;
    try {
      browser = await chromium.launch({
        executablePath: findChromiumPath(),
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
      // One page for the whole run: a context per file is slow and flaky under
      // load.
      const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
      await context.route("**/resources/testharnessreport.js", async (route) => {
        const response = await route.fetch();
        await route.fulfill({ response, body: `${await response.text()}${REPORT_PROBE}` });
      });
      const page = await context.newPage();
      page.setDefaultTimeout(timeoutMs);
      const results = [];
      try {
        for (const file of cases) {
          if (!browser.isConnected()) {
            throw new Error(`Chromium closed after ${results.length}/${cases.length} cases`);
          }
          results.push(await runCaseInChromium(page, server.url, file, expected, timeoutMs));
        }
      } finally {
        await context.close().catch(() => undefined);
      }
      summary = summarize(engine, cases, results);
    } finally {
      await browser?.close().catch(() => undefined);
      await server.close();
    }
  }

  if (outPath) {
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, `${JSON.stringify(summary, null, 2)}\n`);
  }
  return summary;
}

function parseArgs(argv) {
  const opts = { engine: "chromium", filter: null, outPath: null, json: false, reportOnly: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--engine" && argv[i + 1]) opts.engine = argv[++i];
    else if (arg === "--filter" && argv[i + 1]) opts.filter = argv[++i];
    else if (arg === "--out" && argv[i + 1]) opts.outPath = argv[++i];
    else if (arg === "--json") opts.json = true;
    else if (arg === "--report-only") opts.reportOnly = true;
  }
  return opts;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const opts = parseArgs(process.argv.slice(2));
  const summary = await runCanvasWpt(opts);
  const outPath =
    opts.outPath ?? path.join(repoRoot, `tests/results/wpt-canvas/${opts.engine === "native" ? "native" : "summary"}.json`);
  if (!opts.outPath) {
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, `${JSON.stringify(summary, null, 2)}\n`);
  }
  if (opts.json) {
    console.log(JSON.stringify(summary, null, 2));
  } else if (summary.status === "blocked") {
    console.log(`canvas WPT ${summary.engine}: blocked (${summary.reason})`);
  } else {
    const pct = (summary.passRate * 100).toFixed(1);
    console.log(
      `canvas WPT ${summary.engine} (${WPT_REVISION.slice(0, 8)}): ${summary.passed}/${summary.total} files passed (${pct}%), ` +
        `${summary.subtestsPassed}/${summary.subtestsTotal} subtests`,
    );
    for (const result of summary.cases.filter((entry) => !entry.pass)) {
      console.log(`  FAIL ${result.file}${result.error ? `: ${result.error}` : ""}`);
      for (const subtest of result.subtests.filter((entry) => !entry.pass)) {
        console.log(`    ${subtest.status} ${subtest.name}${subtest.message ? `: ${subtest.message}` : ""}`);
      }
    }
  }
  if (summary.status !== "blocked" && summary.failed > 0 && !opts.reportOnly) {
    process.exit(1);
  }
}
