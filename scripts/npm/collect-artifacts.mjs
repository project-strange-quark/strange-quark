#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Places the release workflow's per-platform build artifacts where
// stage.mjs looks for them.
//
//   node scripts/npm/collect-artifacts.mjs <artifacts-dir> [--target-dir <dir>]
//
// The workflow downloads each platform's binaries into
// <artifacts-dir>/bin-<platform id>/ (squark and squark-runtime, with .exe on
// Windows). This copies them to <target-dir>/<rust triple>/dist/ and makes
// them executable again, since artifact archives drop the mode bits.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { PLATFORM_TARGETS, REPO_ROOT } from "./platforms.mjs";

/** Copies every platform's artifacts into place; returns what was placed. */
export function collectArtifacts({ artifactsDir, targetDir = path.join(REPO_ROOT, "target") }) {
  const placed = [];
  for (const target of PLATFORM_TARGETS) {
    const from = path.join(artifactsDir, `bin-${target.id}`);
    if (!fs.existsSync(from)) continue;
    const to = path.join(targetDir, target.rustTriple, "dist");
    fs.mkdirSync(to, { recursive: true });
    const files = [];
    for (const name of ["squark", "squark-runtime"].map((bin) => `${bin}${target.exeSuffix}`)) {
      const source = path.join(from, name);
      if (!fs.existsSync(source)) {
        throw new Error(`artifact bin-${target.id} is missing ${name}`);
      }
      const destination = path.join(to, name);
      fs.copyFileSync(source, destination);
      if (target.exeSuffix === "") fs.chmodSync(destination, 0o755);
      files.push(destination);
    }
    placed.push({ id: target.id, files });
  }
  return placed;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const args = process.argv.slice(2);
  const artifactsDir = args[0];
  const targetFlag = args.indexOf("--target-dir");
  if (!artifactsDir || artifactsDir.startsWith("--")) {
    console.error("usage: collect-artifacts.mjs <artifacts-dir> [--target-dir <dir>]");
    process.exit(2);
  }
  const placed = collectArtifacts({
    artifactsDir: path.resolve(artifactsDir),
    ...(targetFlag >= 0 ? { targetDir: path.resolve(args[targetFlag + 1]) } : {}),
  });
  for (const { id, files } of placed) {
    console.log(`${id}: ${files.map((file) => path.relative(process.cwd(), file)).join(", ")}`);
  }
  if (placed.length === 0) {
    console.error(`no bin-<platform> artifacts under ${artifactsDir}`);
    process.exit(1);
  }
}
