// SPDX-License-Identifier: MIT
// Platform names for result files and scratch directories. Linux keeps the
// release names; other hosts get a suffix so their reports never replace the
// Linux evidence.

import fs from "node:fs";
import path from "node:path";

/** "" on Linux, "-macos" on macOS, "-windows" on Windows. */
export function platformSuffix(platform = process.platform) {
  return { linux: "", darwin: "-macos", win32: "-windows" }[platform] ?? `-${platform}`;
}

/** A repository path relative to the checkout, with forward slashes. */
export function repoRelative(repoRoot, file) {
  const relative = path.relative(repoRoot, file);
  return relative.startsWith("..") || path.isAbsolute(relative)
    ? file
    : relative.split(path.sep).join("/");
}

/** An empty scratch directory under target/tmp, gitignored and on the same disk. */
export function scratchTempDir(repoRoot, prefix) {
  const root = path.join(repoRoot, "target", "tmp");
  fs.mkdirSync(root, { recursive: true });
  return fs.mkdtempSync(path.join(root, prefix));
}
