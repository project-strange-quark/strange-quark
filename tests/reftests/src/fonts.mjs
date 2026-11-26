// SPDX-License-Identifier: MIT
// Off Linux, warn once when the system test fonts are missing: the committed
// Chromium baselines and the Rust layout expectations were made on Linux with
// Liberation and DejaVu installed.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const TEST_FONTS = Object.freeze([
  { family: "Liberation Sans", needle: "liberationsans" },
  { family: "DejaVu Sans", needle: "dejavusans" },
]);

/** The usual system font directories for the host. */
export function fontDirs(platform = process.platform, env = process.env) {
  if (platform === "darwin") {
    return [
      path.join(os.homedir(), "Library", "Fonts"),
      "/Library/Fonts",
    ];
  }
  if (platform === "win32") {
    return [
      path.join(env.WINDIR ?? "C:\\Windows", "Fonts"),
      path.join(
        env.LOCALAPPDATA ?? path.join(os.homedir(), "AppData", "Local"),
        "Microsoft",
        "Windows",
        "Fonts",
      ),
    ];
  }
  return ["/usr/share/fonts", "/usr/local/share/fonts", path.join(os.homedir(), ".fonts")];
}

function collectFontNames(dir, out) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      collectFontNames(full, out);
    } else if (entry.isFile()) {
      out.push(entry.name.toLowerCase());
    }
  }
}

/** Test families that no file in `dirs` provides. */
export function missingTestFonts(dirs = fontDirs()) {
  const names = [];
  for (const dir of dirs) collectFontNames(dir, names);
  return TEST_FONTS.filter((font) => !names.some((name) => name.includes(font.needle))).map(
    (font) => font.family,
  );
}

/**
 * Prints one warning per process, off Linux, when the fonts used by the Linux
 * baselines are absent. `surface` names the affected tests.
 */
export function warnIfTestFontsMissing({ platform = process.platform, dirs, surface } = {}) {
  if (platform === "linux") return [];
  const missing = missingTestFonts(dirs ?? fontDirs(platform));
  if (missing.length === 0) return [];
  const checked = (dirs ?? fontDirs(platform)).join(", ");
  const what =
    surface ??
    "Reftests that render system-font text (for example r3 03-text/01-text-align-baseline)";
  console.error(
    `WARNING: ${missing.join(" and ")} not found in ${checked}.\n` +
      `  ${what} will differ from the committed Linux numbers/baselines.\n` +
      "  Install them on macOS with `brew install --cask font-liberation font-dejavu` " +
      "(or install the Liberation and DejaVu families on Windows).",
  );
  return missing;
}
