#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Verifies every staged npm package manifest and tarball contents using
// `npm pack --dry-run --json`, failing on any missing or unexpected files.
//
// Usage:
//   node scripts/npm/verify.mjs [--manifest <file>] [--require-all]

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { npmCommand } from "./npm-command.mjs";
import {
  JS_PACKAGES,
  MIN_USER_NODE_ENGINE,
  PLATFORM_TARGETS,
  REPO_ROOT,
  expectedFilesForPlatform,
  isValidSemver,
} from "./platforms.mjs";

function parsePackJson(stdout, pkgName) {
  const start = stdout.indexOf("[");
  const end = stdout.lastIndexOf("]");
  if (start === -1 || end === -1 || end < start) {
    throw new Error(`npm pack --dry-run --json for ${pkgName} did not return JSON:\n${stdout}`);
  }
  const parsed = JSON.parse(stdout.slice(start, end + 1));
  if (!Array.isArray(parsed) || parsed.length === 0) {
    throw new Error(`npm pack --dry-run --json for ${pkgName} returned an empty array`);
  }
  return parsed[0];
}

function validateManifestMetadata(pkg, entry, expectedVersion) {
  const errors = [];
  const label = `${entry.name} (${entry.dir}/package.json)`;

  if (pkg.name !== entry.name) errors.push(`${label}: name is "${pkg.name}", expected "${entry.name}"`);
  if (pkg.private === true) errors.push(`${label}: must not set "private": true`);
  if (!isValidSemver(pkg.version) || pkg.version !== expectedVersion) {
    errors.push(`${label}: version is "${pkg.version}", expected "${expectedVersion}"`);
  }
  if (typeof pkg.description !== "string" || pkg.description.trim() === "") {
    errors.push(`${label}: missing description`);
  }
  if (!Array.isArray(pkg.keywords) || pkg.keywords.length === 0) {
    errors.push(`${label}: missing keywords`);
  }
  if (pkg.license !== "MIT") errors.push(`${label}: license must be "MIT"`);
  if (!pkg.repository?.url || !pkg.repository?.directory) {
    errors.push(`${label}: repository.url and repository.directory are required`);
  }
  if (!pkg.homepage) errors.push(`${label}: missing homepage`);
  if (!pkg.bugs?.url) errors.push(`${label}: missing bugs.url`);
  if (pkg.engines?.node !== MIN_USER_NODE_ENGINE) {
    errors.push(`${label}: engines.node is "${pkg.engines?.node}", expected "${MIN_USER_NODE_ENGINE}"`);
  }
  if (pkg.sideEffects !== false) errors.push(`${label}: expected sideEffects: false`);

  if (entry.name.startsWith("@") && pkg.publishConfig?.access !== "public") {
    errors.push(`${label}: scoped package must set publishConfig.access = "public"`);
  }

  if (entry.name === "strange-quark") {
    if (pkg.bin?.squark !== "./dist/cli.js") {
      errors.push(`${label}: bin.squark must be "./dist/cli.js"`);
    }
    if (!pkg.exports?.["."]?.types || !pkg.exports?.["./config"]?.types) {
      errors.push(`${label}: exports must include types for "." and "./config"`);
    }
    const optDeps = pkg.optionalDependencies ?? {};
    for (const target of PLATFORM_TARGETS) {
      if (optDeps[target.packageName] !== expectedVersion) {
        errors.push(
          `${label}: optionalDependencies["${target.packageName}"] is "${optDeps[target.packageName]}", expected "${expectedVersion}"`,
        );
      }
    }
    if (Object.keys(optDeps).length !== PLATFORM_TARGETS.length) {
      errors.push(`${label}: unexpected extra entries in optionalDependencies`);
    }
  } else if (entry.name === "@strange-quark/vite-plugin") {
    if (!pkg.exports?.["."]?.types || !pkg.exports?.["./launcher"]?.types) {
      errors.push(`${label}: exports must include types for "." and "./launcher"`);
    }
  } else if (entry.kind === "platform") {
    const target = PLATFORM_TARGETS.find((t) => t.packageName === entry.name);
    if (!target) {
      errors.push(`${label}: unknown platform target`);
    } else {
      if (JSON.stringify(pkg.os) !== JSON.stringify([target.os])) {
        errors.push(`${label}: os must be ["${target.os}"]`);
      }
      if (JSON.stringify(pkg.cpu) !== JSON.stringify([target.cpu])) {
        errors.push(`${label}: cpu must be ["${target.cpu}"]`);
      }
      if (target.libc) {
        if (JSON.stringify(pkg.libc) !== JSON.stringify([target.libc])) {
          errors.push(`${label}: libc must be ["${target.libc}"]`);
        }
      } else if (pkg.libc !== undefined) {
        errors.push(`${label}: non-Linux platform package must not declare libc`);
      }
    }
  }

  return errors;
}

function defaultEntriesFromRepo(repoRoot) {
  const mainPkg = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "packages/strange-quark/package.json"), "utf8"),
  );
  const version = mainPkg.version;
  const packages = [
    ...JS_PACKAGES.map((p) => ({
      id: p.id,
      name: p.packageName,
      kind: "js",
      version,
      dir: path.join(repoRoot, p.dir),
      staged: true,
      expectedFiles: [...p.expectedFiles].sort(),
    })),
    ...PLATFORM_TARGETS.map((t) => {
      const dir = path.join(repoRoot, t.dir);
      const hasBins =
        fs.existsSync(path.join(dir, `bin/squark${t.exeSuffix}`)) &&
        fs.existsSync(path.join(dir, `bin/squark-runtime${t.exeSuffix}`));
      return {
        id: t.id,
        name: t.packageName,
        kind: "platform",
        version,
        dir,
        staged: hasBins,
        os: t.os,
        cpu: t.cpu,
        libc: t.libc,
        expectedFiles: expectedFilesForPlatform(t),
      };
    }),
  ];
  return { version, packages };
}

export function verifyPackages({
  repoRoot = REPO_ROOT,
  manifestPath = null,
  requireAll = false,
  npmBin = null,
} = {}) {
  const source =
    manifestPath && fs.existsSync(manifestPath)
      ? JSON.parse(fs.readFileSync(manifestPath, "utf8"))
      : defaultEntriesFromRepo(repoRoot);

  const errors = [];
  const checked = [];
  const skipped = [];
  const npmCacheDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-pack-cache-"));

  try {
    for (const entry of source.packages) {
      const pkgJsonPath = path.join(entry.dir, "package.json");
      if (!fs.existsSync(pkgJsonPath)) {
        errors.push(`Missing package.json at ${pkgJsonPath}`);
        continue;
      }
      const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
      errors.push(...validateManifestMetadata(pkg, entry, source.version));

      if (!entry.staged) {
        if (requireAll) {
          errors.push(`${entry.name}: binaries not staged (--require-all)`);
        } else {
          skipped.push(entry.name);
        }
        continue;
      }

      const pack = npmBin
        ? { command: npmBin, args: ["pack", "--dry-run", "--json"] }
        : npmCommand("npm", ["pack", "--dry-run", "--json"]);
      const res = spawnSync(pack.command, pack.args, {
        cwd: entry.dir,
        env: { ...process.env, npm_config_cache: npmCacheDir },
        encoding: "utf8",
        shell: false,
        timeout: 60_000,
      });
      if (res.error) {
        errors.push(`${entry.name}: npm pack failed to run: ${res.error.message}`);
        continue;
      }
      if (res.status !== 0) {
        errors.push(
          `${entry.name}: npm pack --dry-run exited ${res.status}:\n${res.stderr || res.stdout}`,
        );
        continue;
      }

      const info = parsePackJson(res.stdout, entry.name);
      const actualFiles = info.files.map((f) => f.path).sort();
      const expectedFiles = [...entry.expectedFiles].sort();

      const actualSet = new Set(actualFiles);
      const expectedSet = new Set(expectedFiles);
      const missing = expectedFiles.filter((f) => !actualSet.has(f));
      const unexpected = actualFiles.filter((f) => !expectedSet.has(f));

      if (missing.length > 0) {
        errors.push(`${entry.name}: missing expected file(s) in tarball: ${missing.join(", ")}`);
      }
      if (unexpected.length > 0) {
        errors.push(`${entry.name}: unexpected file(s) in tarball: ${unexpected.join(", ")}`);
      }

      checked.push({
        name: entry.name,
        version: pkg.version,
        fileCount: actualFiles.length,
        size: info.size,
        unpackedSize: info.unpackedSize,
        files: actualFiles,
      });
    }
  } finally {
    fs.rmSync(npmCacheDir, { recursive: true, force: true });
  }

  if (errors.length > 0) {
    throw new Error(`npm pack verification failed:\n  - ${errors.join("\n  - ")}`);
  }

  return { version: source.version, checked, skipped };
}

function formatBytes(bytes) {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${bytes} B`;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const args = process.argv.slice(2);
  let manifestPath = path.join(REPO_ROOT, "target/npm/pack-manifest.json");
  let requireAll = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--manifest") manifestPath = path.resolve(args[++i]);
    else if (args[i] === "--require-all") requireAll = true;
    else if (args[i] === "--no-manifest") manifestPath = null;
    else if (args[i] === "--help" || args[i] === "-h") {
      console.log(
        "Usage: node scripts/npm/verify.mjs [--manifest <file> | --no-manifest] [--require-all]",
      );
      process.exit(0);
    } else {
      console.error(`Unknown argument: ${args[i]}`);
      process.exit(2);
    }
  }

  try {
    const result = verifyPackages({ manifestPath, requireAll });
    console.log(
      `Verified ${result.checked.length} npm package(s) at version ${result.version}:`,
    );
    for (const pkg of result.checked) {
      console.log(
        `  - ${pkg.name}@${pkg.version}: ${pkg.fileCount} files, tarball ${formatBytes(pkg.size)}, unpacked ${formatBytes(pkg.unpackedSize)}`,
      );
    }
    if (result.skipped.length > 0) {
      console.log(`Metadata verified (tarball skipped, no binaries staged): ${result.skipped.join(", ")}`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
