// SPDX-License-Identifier: MIT
// Tests for the reftest pixel comparator, manifest parser, licence checker,
// and headless Chromium screenshots.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";
import { isSpdxAllowed } from "../../../scripts/check-licenses.mjs";
import {
  chromiumVersion,
  findChromiumPath,
  pinnedChromium,
  playwrightChromiumPath,
} from "../../../scripts/chromium-path.mjs";
import { comparePngBuffers, samplePngPixel } from "./compare.mjs";
import { warnIfTestFontsMissing } from "./fonts.mjs";
import { discoverCases, parseCaseMetadata } from "./manifest.mjs";
import { runReftests } from "./runner.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const reftestsRoot = path.resolve(__dirname, "..");
const testsRoot = path.resolve(reftestsRoot, "..");

warnIfTestFontsMissing({
  surface: "The system-font reftests (for example r3 03-text/01-text-align-baseline)",
});

function makeSolidPng(width, height, [r, g, b, a]) {
  const png = new PNG({ width, height });
  for (let i = 0; i < png.data.length; i += 4) {
    png.data[i] = r;
    png.data[i + 1] = g;
    png.data[i + 2] = b;
    png.data[i + 3] = a;
  }
  return PNG.sync.write(png);
}

test("comparePngBuffers passes identical PNGs", () => {
  const a = makeSolidPng(16, 16, [24, 28, 36, 255]);
  const res = comparePngBuffers(a, a);
  assert.equal(res.pass, true);
  assert.equal(res.diffPixels, 0);
  assert.equal(res.maxObservedDelta, 0);
  assert.equal(res.diffPng, null);
});

test("comparePngBuffers respects maxChannelDelta and maxDiffRatio", () => {
  const base = makeSolidPng(10, 10, [100, 100, 100, 255]);
  const slight = makeSolidPng(10, 10, [102, 99, 101, 255]);
  assert.equal(comparePngBuffers(base, slight, { maxChannelDelta: 2 }).pass, true);

  const alteredPng = PNG.sync.read(base);
  // Paint 2 pixels out of 100 (2%) with delta = 20.
  for (let x = 0; x < 2; x++) {
    alteredPng.data[x * 4] = 120;
  }
  const altered = PNG.sync.write(alteredPng);

  const failRes = comparePngBuffers(base, altered, { maxChannelDelta: 2, maxDiffRatio: 0.01 });
  assert.equal(failRes.pass, false);
  assert.equal(failRes.diffPixels, 2);
  assert.equal(failRes.maxObservedDelta, 20);
  assert.deepEqual(failRes.diffBounds, { x: 0, y: 0, width: 2, height: 1 });
  assert.ok(Buffer.isBuffer(failRes.diffPng) && failRes.diffPng.length > 0);

  const passRes = comparePngBuffers(base, altered, { maxChannelDelta: 2, maxDiffRatio: 0.02 });
  assert.equal(passRes.pass, true);
});

test("comparePngBuffers fails on dimension mismatch", () => {
  const a = makeSolidPng(8, 8, [0, 0, 0, 255]);
  const b = makeSolidPng(16, 8, [0, 0, 0, 255]);
  const res = comparePngBuffers(a, b);
  assert.equal(res.pass, false);
  assert.match(res.reason, /dimension mismatch/);
});

test("parseCaseMetadata parses leading reftest comment and defaults", () => {
  const withComment = `<!-- reftest: {"width": 640, "height": 480, "maxChannelDelta": 5} --><html></html>`;
  const parsed = parseCaseMetadata(withComment);
  assert.equal(parsed.width, 640);
  assert.equal(parsed.height, 480);
  assert.equal(parsed.dpr, 1);
  assert.equal(parsed.maxChannelDelta, 5);

  const fallback = parseCaseMetadata("<!doctype html><html></html>");
  assert.equal(fallback.width, 800);
  assert.equal(fallback.height, 600);
});

test("discoverCases finds the smoke case", () => {
  const cases = discoverCases(path.join(reftestsRoot, "cases"), "smoke");
  assert.equal(cases.length, 1);
  assert.equal(cases[0].id, "box-and-text");
});

test("isSpdxAllowed enforces AGENTS.md licence policy", () => {
  assert.equal(isSpdxAllowed("MIT"), true);
  assert.equal(isSpdxAllowed("Apache-2.0"), true);
  assert.equal(isSpdxAllowed("BSD-3-Clause"), true);
  assert.equal(isSpdxAllowed("ISC"), true);
  assert.equal(isSpdxAllowed("MPL-2.0"), true);
  assert.equal(isSpdxAllowed("BSL-1.0"), true);
  assert.equal(isSpdxAllowed("CC-BY-4.0"), true);
  assert.equal(isSpdxAllowed("CC-BY-SA-4.0"), false);
  assert.equal(isSpdxAllowed("(MIT OR Apache-2.0)"), true);
  assert.equal(isSpdxAllowed("(MIT OR GPL-3.0-only)"), true);
  assert.equal(isSpdxAllowed("GPL-3.0-only"), false);
  assert.equal(isSpdxAllowed("LGPL-2.1-or-later"), false);
  assert.equal(isSpdxAllowed("AGPL-3.0-only"), false);
  assert.equal(isSpdxAllowed("MIT AND GPL-2.0-only"), false);
  assert.equal(isSpdxAllowed(""), false);
  assert.equal(isSpdxAllowed(null), false);
});

test("headless Chromium renders smoke/box-and-text.html into target/", async (t) => {
  const repoRoot = path.resolve(testsRoot, "..");
  const tmpRoot = path.join(repoRoot, "target", "tmp");
  fs.mkdirSync(tmpRoot, { recursive: true });
  // Render into a scratch directory, never into the tracked tests/results.
  const resultsRoot = fs.mkdtempSync(path.join(tmpRoot, "reftests-smoke-"));
  try {
    const summary = await runReftests({
      casesRoot: path.join(reftestsRoot, "cases"),
      resultsRoot,
      suite: "smoke",
      mode: "chromium",
      timeoutMs: 30_000,
    });

    assert.equal(summary.total, 1);
    const pngPath = path.join(resultsRoot, "smoke", "expected", "box-and-text.png");
    assert.equal(fs.existsSync(pngPath), true);

    const buf = fs.readFileSync(pngPath);
    const decoded = PNG.sync.read(buf);
    assert.equal(decoded.width, 800);
    assert.equal(decoded.height, 600);

    // Corner pixel must match body background (#181c24 -> 24, 28, 36, 255).
    assert.deepEqual(samplePngPixel(buf, 10, 10), [24, 28, 36, 255]);
    // Center-top inside card must match card background (#252b38 -> 37, 43, 56, 255)
    // or banner gradient (not empty white/black).
    const corner = samplePngPixel(buf, 10, 10);
    const insideCard = samplePngPixel(buf, 130, 300);
    assert.notDeepEqual(insideCard, corner);

    const committedPath = path.join(
      testsRoot,
      "results",
      "smoke",
      "expected",
      "box-and-text.png",
    );
    if (process.platform === "linux") {
      const cmp = comparePngBuffers(fs.readFileSync(committedPath), buf);
      assert.equal(
        cmp.pass,
        true,
        `the fresh render differs from the committed baseline: diffPixels=${cmp.diffPixels} ` +
          `(${(cmp.diffRatio * 100).toFixed(3)}%), maxDelta=${cmp.maxObservedDelta}`,
      );
    } else {
      t.diagnostic(
        `skipping the committed-baseline comparison on ${process.platform}: ` +
          "Chromium font and rasterization differ per platform",
      );
    }
  } finally {
    fs.rmSync(resultsRoot, { recursive: true, force: true });
  }
});

test("r1 suite has 150 static cases, expected Chromium PNGs, and inline geometry", () => {
  const cases = discoverCases(path.join(reftestsRoot, "cases"), "r1");
  assert.equal(cases.length, 150);

  for (const c of cases) {
    const html = fs.readFileSync(c.filePath, "utf8");
    assert.equal(
      /<script\b/i.test(html),
      false,
      `R1 static case ${c.id} must not contain <script>`,
    );

    const pngPath = path.join(testsRoot, "results", "r1", "expected", `${c.id}.png`);
    assert.equal(fs.existsSync(pngPath), true, `missing expected PNG for ${c.id}`);
    const decoded = PNG.sync.read(fs.readFileSync(pngPath));
    assert.equal(decoded.width, Math.round(c.meta.width * c.meta.dpr), `width mismatch for ${c.id}`);
    assert.equal(decoded.height, Math.round(c.meta.height * c.meta.dpr), `height mismatch for ${c.id}`);
  }

  const geomPath = path.join(testsRoot, "results", "r1", "geometry.json");
  assert.equal(fs.existsSync(geomPath), true, "missing r1 geometry.json");
  const geom = JSON.parse(fs.readFileSync(geomPath, "utf8"));
  for (let i = 1; i <= 10; i++) {
    const prefix = `01-inline/${String(i).padStart(2, "0")}-`;
    const match = Object.keys(geom.cases).find((k) => k.startsWith(prefix));
    assert.ok(match, `missing inline geometry baseline for ${prefix}*`);
    const els = geom.cases[match].elements;
    assert.ok(Object.keys(els).length > 0, `empty geometry elements for ${match}`);
  }
});

test("the reference Chromium is the Playwright-pinned build", (t) => {
  const pinnedPath = playwrightChromiumPath();
  if (!pinnedPath) {
    t.skip("Playwright Chromium not installed; run node node_modules/playwright-core/cli.js install chromium");
    return;
  }
  assert.equal(
    findChromiumPath({ allowSystem: false }),
    pinnedPath,
    "SQUARK_CHROMIUM_PATH unset: the pinned browser must win over the system one",
  );
  const pinned = pinnedChromium();
  const version = chromiumVersion(pinnedPath);
  assert.ok(
    version.includes(pinned.browserVersion),
    `pinned browser version ${version} does not match playwright's ${pinned.browserVersion}`,
  );
});

test("r3 canvas suite reproduces its committed Chromium baselines", { timeout: 300_000 }, async (t) => {
  const resultsRoot = path.join(testsRoot, "results");
  const cases = discoverCases(path.join(reftestsRoot, "cases"), "r3-canvas");
  assert.ok(cases.length >= 15, `expected the R3 canvas suite, got ${cases.length} cases`);

  const fresh = fs.mkdtempSync(path.join(testsRoot, ".tmp-r3-baselines-"));
  try {
    const summary = await runReftests({
      casesRoot: path.join(reftestsRoot, "cases"),
      resultsRoot: fresh,
      suite: "r3-canvas",
      mode: "chromium",
      timeoutMs: 60_000,
    });
    assert.equal(summary.total, cases.length);

    for (const c of cases) {
      const committedPath = path.join(resultsRoot, "r3-canvas", "expected", `${c.id}.png`);
      const renderedPath = path.join(fresh, "r3-canvas", "expected", `${c.id}.png`);
      assert.ok(fs.existsSync(committedPath), `missing committed baseline for ${c.id}`);

      const committed = fs.readFileSync(committedPath);
      const rendered = fs.readFileSync(renderedPath);
      // The baselines are Linux renders. The text cases shape the bundled
      // fonts everywhere, but Chromium rasterizes glyphs differently on
      // macOS and Windows, so like the smoke proof they compare on Linux only.
      if (c.id.startsWith("03-text/") && process.platform !== "linux") {
        t.diagnostic(`skipping ${c.id}'s baseline comparison on ${process.platform}: glyph rasterization differs per platform`);
      } else {
        const cmp = comparePngBuffers(committed, rendered, c.meta);
        assert.equal(cmp.pass, true, `${c.id} differs from its baseline: ${cmp.reason}`);
      }

      // A blank baseline means the case's script threw before drawing.
      const decoded = PNG.sync.read(committed);
      const colors = new Set();
      for (let i = 0; i < decoded.data.length; i += 4) {
        colors.add(
          `${decoded.data[i]},${decoded.data[i + 1]},${decoded.data[i + 2]},${decoded.data[i + 3]}`,
        );
        if (colors.size > 4) break;
      }
      assert.ok(colors.size > 1, `${c.id} baseline is blank`);
    }
  } finally {
    fs.rmSync(fresh, { recursive: true, force: true });
  }
});

test("runReftests compare mode invokes squark screenshot with adapter/runtime flags and writes diff on mismatch", async () => {
  const tmp = fs.mkdtempSync(path.join(testsRoot, ".tmp-reftest-compare-"));
  try {
    const casesRoot = path.join(tmp, "cases");
    const resultsRoot = path.join(tmp, "results");
    fs.mkdirSync(path.join(casesRoot, "demo"), { recursive: true });
    fs.mkdirSync(path.join(resultsRoot, "demo", "expected"), { recursive: true });

    fs.writeFileSync(
      path.join(casesRoot, "demo", "a-pass.html"),
      `<!-- reftest: {"width": 16, "height": 16, "dpr": 1} --><!doctype html><p>a</p>`,
    );
    fs.writeFileSync(
      path.join(casesRoot, "demo", "b-fail.html"),
      `<!-- reftest: {"width": 16, "height": 16, "dpr": 1} --><!doctype html><p>b</p>`,
    );

    const greenPng = makeSolidPng(16, 16, [0x21, 0x52, 0x2a, 255]);
    const redPng = makeSolidPng(16, 16, [0xcc, 0x22, 0x22, 255]);
    fs.writeFileSync(path.join(resultsRoot, "demo", "expected", "a-pass.png"), greenPng);
    fs.writeFileSync(path.join(resultsRoot, "demo", "expected", "b-fail.png"), greenPng);
    fs.writeFileSync(path.join(tmp, "green.png"), greenPng);
    fs.writeFileSync(path.join(tmp, "red.png"), redPng);

    const argsLog = path.join(tmp, "args.log");
    // A Node script, so the double runs on Windows too (the runner starts
    // .mjs doubles with Node).
    const mockSquark = path.join(tmp, "mock-squark.mjs");
    fs.writeFileSync(
      mockSquark,
      `import fs from "node:fs";
const args = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(argsLog)}, args.join(" ") + "\\n");
const out = args[args.indexOf("--out") + 1];
const source = args[1].endsWith("a-pass.html") ? ${JSON.stringify(path.join(tmp, "green.png"))} : ${JSON.stringify(path.join(tmp, "red.png"))};
fs.copyFileSync(source, out);
`,
    );

    const summary = await runReftests({
      casesRoot,
      resultsRoot,
      suite: "demo",
      mode: "compare",
      squarkBin: mockSquark,
      runtime: "/fake/squark-runtime",
      gpuAdapter: "software",
      timeoutMs: 10_000,
    });

    assert.equal(summary.total, 2);
    assert.equal(summary.passed, 1);
    assert.equal(summary.failed, 1);
    assert.equal(summary.renderErrors, 0);
    assert.equal(summary.passRate, 0.5);
    assert.equal(fs.existsSync(path.join(resultsRoot, "demo", "diff", "b-fail.png")), true);
    assert.equal(fs.existsSync(path.join(resultsRoot, "demo", "diff", "a-pass.png")), false);

    const summaryPath = path.join(resultsRoot, "demo", "summary.json");
    assert.equal(fs.existsSync(summaryPath), true);
    const written = JSON.parse(fs.readFileSync(summaryPath, "utf8"));
    assert.equal(written.mode, "compare");
    assert.equal(written.passed, 1);
    assert.equal(written.failed, 1);
    assert.equal(written.cases[0].id, "a-pass");
    assert.equal(written.cases[0].pass, true);
    assert.equal(written.cases[0].diffRatio, 0);
    assert.equal(written.cases[0].maxDelta, 0);
    assert.equal(written.cases[1].id, "b-fail");
    assert.equal(written.cases[1].pass, false);
    assert.equal(written.cases[1].diffRatio, 1);
    assert.equal(written.cases[1].maxDelta, 0xcc - 0x21);

    const logged = fs.readFileSync(argsLog, "utf8");
    assert.match(logged, /--gpu-adapter software/);
    assert.match(logged, /--runtime \/fake\/squark-runtime/);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
