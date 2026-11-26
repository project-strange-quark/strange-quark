#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Publishes the staged packages under target/npm/stage/ (scripts/npm/stage.mjs)
// to npm: the six platform packages first, so `strange-quark`'s
// optionalDependencies resolve the moment it appears, then
// @strange-quark/vite-plugin, then strange-quark.
//
// A version npm already has is skipped, so a release that failed part-way can
// be run again. A prerelease version (one with a "-", such as 0.2.0-rc.1) goes
// to the "next" dist-tag; a plain publish would make it "latest" for everyone.
//
// Usage:
//   node scripts/npm/publish.mjs [--stage-dir <dir>] [--dry-run] [--no-provenance]

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { npmCommand } from "./npm-command.mjs";
import { JS_PACKAGES, PLATFORM_TARGETS, REPO_ROOT } from "./platforms.mjs";

/** Runs npm and returns `{ status, stdout, stderr }`. */
function runNpm(args, options = {}) {
  const { command, args: fullArgs } = npmCommand("npm", args);
  const res = spawnSync(command, fullArgs, { encoding: "utf8", shell: false, ...options });
  if (res.error) throw new Error(`failed to run npm ${args.join(" ")}: ${res.error.message}`);
  return { status: res.status, stdout: res.stdout ?? "", stderr: res.stderr ?? "" };
}

/** The staged package directories, in publish order. */
export function publishOrder(stageDir) {
  return [
    ...PLATFORM_TARGETS.map((target) => path.join(stageDir, `cli-${target.id}`)),
    ...["vite-plugin", "strange-quark"].map((id) => {
      if (!JS_PACKAGES.some((pkg) => pkg.id === id)) throw new Error(`unknown JS package ${id}`);
      return path.join(stageDir, id);
    }),
  ];
}

export function distTag(version) {
  return version.includes("-") ? "next" : "latest";
}

/** Whether npm already has `name@version`. Any failure other than a 404 is an error. */
export function isPublished(name, version, run = runNpm) {
  const res = run(["view", `${name}@${version}`, "version", "--json"]);
  if (res.status === 0) return res.stdout.trim() !== "";
  if (/\bE404\b|404 Not Found/.test(res.stderr)) return false;
  throw new Error(`npm view ${name}@${version} failed:\n${res.stderr}`);
}

export function publishAll({
  stageDir = path.join(REPO_ROOT, "target/npm/stage"),
  dryRun = false,
  provenance = true,
  run = runNpm,
  log = console.log,
} = {}) {
  // Read every manifest first, so a missing package stops the release before
  // anything is published.
  const staged = publishOrder(stageDir).map((dir) => {
    const manifest = path.join(dir, "package.json");
    if (!fs.existsSync(manifest)) {
      throw new Error(`missing staged package ${manifest}; run scripts/npm/stage.mjs --require-all first`);
    }
    const { name, version } = JSON.parse(fs.readFileSync(manifest, "utf8"));
    return { dir, name, version };
  });
  const results = [];
  for (const { dir, name, version } of staged) {
    if (isPublished(name, version, run)) {
      log(`skip ${name}@${version}: already on npm`);
      results.push({ name, version, action: "skipped" });
      continue;
    }
    const tag = distTag(version);
    const args = ["publish", "--access", "public", "--tag", tag];
    if (provenance) args.push("--provenance");
    if (dryRun) args.push("--dry-run");
    log(`${dryRun ? "would publish" : "publish"} ${name}@${version} (${tag})`);
    const res = run(args, { cwd: dir, stdio: ["ignore", "inherit", "inherit"] });
    if (res.status !== 0) throw new Error(`npm publish failed for ${name}@${version}`);
    results.push({ name, version, action: dryRun ? "dry-run" : "published", tag });
  }
  return results;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const argv = process.argv.slice(2);
  const options = { dryRun: false, provenance: true };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--stage-dir") options.stageDir = path.resolve(argv[++i]);
    else if (argv[i] === "--dry-run") options.dryRun = true;
    else if (argv[i] === "--no-provenance") options.provenance = false;
    else {
      console.error(`Unknown argument: ${argv[i]}`);
      process.exit(2);
    }
  }
  try {
    publishAll(options);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
