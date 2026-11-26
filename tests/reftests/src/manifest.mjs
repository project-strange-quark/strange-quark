// SPDX-License-Identifier: MIT
// Discovers HTML reftest cases and parses optional leading metadata comments.

import fs from "node:fs";
import path from "node:path";
import { DEFAULT_TOLERANCE } from "./compare.mjs";

export const DEFAULT_CASE_META = Object.freeze({
  width: 800,
  height: 600,
  dpr: 1,
  maxChannelDelta: DEFAULT_TOLERANCE.maxChannelDelta,
  maxDiffRatio: DEFAULT_TOLERANCE.maxDiffRatio,
  maxDiffPixels: DEFAULT_TOLERANCE.maxDiffPixels,
  waitMs: 0,
});

const META_RE = /<!--\s*reftest:\s*(\{[\s\S]*?\})\s*-->/;

/**
 * Extracts optional `<!-- reftest: { ... } -->` metadata from an HTML case.
 */
export function parseCaseMetadata(html) {
  const match = html.match(META_RE);
  if (!match) {
    return { ...DEFAULT_CASE_META };
  }
  const parsed = JSON.parse(match[1]);
  return {
    width: parsed.width ?? DEFAULT_CASE_META.width,
    height: parsed.height ?? DEFAULT_CASE_META.height,
    dpr: parsed.dpr ?? DEFAULT_CASE_META.dpr,
    maxChannelDelta: parsed.maxChannelDelta ?? DEFAULT_CASE_META.maxChannelDelta,
    maxDiffRatio: parsed.maxDiffRatio ?? DEFAULT_CASE_META.maxDiffRatio,
    maxDiffPixels: parsed.maxDiffPixels ?? DEFAULT_CASE_META.maxDiffPixels,
    waitMs: parsed.waitMs ?? DEFAULT_CASE_META.waitMs,
  };
}

/**
 * Recursively discovers `.html` test cases under `<casesRoot>/<suite>`.
 */
export function discoverCases(casesRoot, suite, filter = null) {
  const suiteDir = path.join(casesRoot, suite);
  if (!fs.existsSync(suiteDir)) {
    throw new Error(`Reftest suite directory does not exist: ${suiteDir}`);
  }

  const results = [];
  function walk(dir) {
    const entries = fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    );
    for (const ent of entries) {
      if (ent.name.startsWith("_")) continue;
      const full = path.join(dir, ent.name);
      if (ent.isDirectory()) {
        walk(full);
      } else if (ent.isFile() && ent.name.endsWith(".html")) {
        const relFromSuite = path.relative(suiteDir, full).replace(/\\/g, "/");
        const id = relFromSuite.replace(/\.html$/, "");
        if (filter && !id.includes(filter)) continue;
        const html = fs.readFileSync(full, "utf8");
        results.push({
          id,
          suite,
          filePath: full,
          relUrlPath: "/" + path.relative(casesRoot, full).replace(/\\/g, "/"),
          meta: parseCaseMetadata(html),
        });
      }
    }
  }

  walk(suiteDir);
  return results;
}
