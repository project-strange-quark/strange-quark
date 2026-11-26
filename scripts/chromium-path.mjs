#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Resolves the reference Chromium for every test harness, so local runs and CI
// use the same build. The baselines and WPT expectations are tied to it.
//
// Order:
//   1. SQUARK_CHROMIUM_PATH (CI sets it from this script's --path output)
//   2. the Chromium that `playwright-core` pins (revision from its browsers.json)
//   3. the system Chromium/Chrome, as a last resort
//
// Usage: node scripts/chromium-path.mjs [--path|--version|--json]

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);

/** The Playwright-pinned Chromium path, or null when it is not installed. */
export function playwrightChromiumPath() {
  try {
    const { chromium } = require("playwright-core");
    const executable = chromium.executablePath();
    return executable && fs.existsSync(executable) ? executable : null;
  } catch {
    return null;
  }
}

const platformCandidates = {
  darwin: [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
  ],
  win32: [
    path.join(process.env.PROGRAMFILES ?? "C:\\Program Files", "Google/Chrome/Application/chrome.exe"),
    path.join(
      process.env["PROGRAMFILES(X86)"] ?? "C:\\Program Files (x86)",
      "Google/Chrome/Application/chrome.exe",
    ),
    path.join(
      process.env.LOCALAPPDATA ?? "",
      "Google/Chrome/Application/chrome.exe",
    ),
  ],
};

export const SYSTEM_CANDIDATES = [
  ...(platformCandidates[process.platform] ?? []),
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
  "/usr/bin/google-chrome",
];

/** The reference Chromium binary, or a thrown error naming every candidate. */
export function findChromiumPath({ allowSystem = true } = {}) {
  const pinned = playwrightChromiumPath();
  const candidates = [
    process.env.SQUARK_CHROMIUM_PATH,
    pinned,
    ...(allowSystem ? SYSTEM_CANDIDATES : []),
  ].filter(Boolean);

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  throw new Error(
    `Could not find the reference Chromium. Checked: ${candidates.join(", ")}. ` +
      "Run `node node_modules/playwright-core/cli.js install chromium` or set SQUARK_CHROMIUM_PATH.",
  );
}

/** The `--version` line of a Chromium binary. */
export function chromiumVersion(executablePath = findChromiumPath()) {
  const output = execFileSync(executablePath, ["--version"], {
    encoding: "utf8",
    timeout: 30_000,
  }).trim();
  return output;
}

/** Playwright's pinned revision and browser version, for CI cache keys. */
export function pinnedChromium() {
  const packageJson = require.resolve("playwright-core/package.json");
  const browsers = JSON.parse(
    fs.readFileSync(path.join(path.dirname(packageJson), "browsers.json"), "utf8"),
  );
  const chromium = browsers.browsers.find((browser) => browser.name === "chromium");
  return {
    revision: chromium?.revision ?? null,
    browserVersion: chromium?.browserVersion ?? null,
  };
}

const isMain = process.argv[1] && process.argv[1].endsWith("chromium-path.mjs");
if (isMain) {
  const arg = process.argv[2] ?? "--path";
  if (arg === "--json") {
    console.log(
      JSON.stringify(
        { ...pinnedChromium(), path: findChromiumPath(), version: chromiumVersion() },
        null,
        2,
      ),
    );
  } else if (arg === "--version") {
    console.log(chromiumVersion());
  } else {
    console.log(findChromiumPath());
  }
}
