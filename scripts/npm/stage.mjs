#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Stages built Rust binaries (`squark` and `squark-runtime`) into the npm platform
// packages, syncs versions and LICENSE copies, and writes a JSON pack manifest.
//
// Usage:
//   node scripts/npm/stage.mjs [--version <ver>] [--target-dir <dir>] \
//     [--stage-dir <dir>] [--manifest <file>] [--require-all] [--platform <id>]

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  JS_PACKAGES,
  PLATFORM_TARGETS,
  REPO_ROOT,
  expectedFilesForPlatform,
  hostPlatformTarget,
  isValidSemver,
} from "./platforms.mjs";

function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

function copyDirRecursive(src, dst) {
  fs.mkdirSync(dst, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    const srcPath = path.join(src, entry.name);
    const dstPath = path.join(dst, entry.name);
    if (entry.isDirectory()) {
      copyDirRecursive(srcPath, dstPath);
    } else if (entry.isFile()) {
      fs.copyFileSync(srcPath, dstPath);
    }
  }
}

export function locatePlatformBinaries(targetDir, target, hostTarget) {
  const squarkName = `squark${target.exeSuffix}`;
  const runtimeName = `squark-runtime${target.exeSuffix}`;

  const candidates = [path.join(targetDir, target.rustTriple, "dist")];
  if (hostTarget && hostTarget.id === target.id) {
    candidates.push(path.join(targetDir, "dist"));
  }

  for (const dir of candidates) {
    const squarkPath = path.join(dir, squarkName);
    const runtimePath = path.join(dir, runtimeName);
    if (fs.existsSync(squarkPath) && fs.existsSync(runtimePath)) {
      return { sourceDir: dir, squarkPath, runtimePath, squarkName, runtimeName };
    }
  }
  return null;
}

export function stagePackages({
  repoRoot = REPO_ROOT,
  targetDir = path.join(repoRoot, "target"),
  stageDir = path.join(repoRoot, "target/npm/stage"),
  manifestPath = path.join(repoRoot, "target/npm/pack-manifest.json"),
  version = null,
  requireAll = false,
  platformFilter = null,
  hostPlatform = process.platform,
  hostArch = process.arch,
} = {}) {
  const hostTarget = hostPlatformTarget(hostPlatform, hostArch);
  const rootLicense = fs.readFileSync(path.join(repoRoot, "LICENSE"), "utf8");
  // The licences of the third-party software in the binaries (see
  // scripts/notices/README.md), shipped beside them in each platform package.
  const notices = fs.readFileSync(path.join(repoRoot, "THIRD_PARTY_NOTICES.txt"), "utf8");

  const mainPkg = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "packages/strange-quark/package.json"), "utf8"),
  );
  const resolvedVersion = version ?? mainPkg.version;
  if (!isValidSemver(resolvedVersion)) {
    throw new Error(`Invalid semver version: "${resolvedVersion}"`);
  }

  if (platformFilter && !PLATFORM_TARGETS.some((t) => t.id === platformFilter)) {
    const known = PLATFORM_TARGETS.map((t) => t.id).join(", ");
    throw new Error(`Unknown platform "${platformFilter}". Expected one of: ${known}`);
  }

  fs.mkdirSync(stageDir, { recursive: true });

  const expectedOptDeps = Object.fromEntries(
    PLATFORM_TARGETS.map((t) => [t.packageName, resolvedVersion]).sort(([a], [b]) =>
      a.localeCompare(b),
    ),
  );

  const manifestPackages = [];

  // 1. JS packages (`strange-quark` and `@strange-quark/vite-plugin`).
  for (const jsPkg of JS_PACKAGES) {
    const srcDir = path.join(repoRoot, jsPkg.dir);
    const distDir = path.join(srcDir, "dist");
    if (!fs.existsSync(distDir)) {
      throw new Error(
        `Missing compiled dist/ for ${jsPkg.packageName} at ${distDir}. Run "npm run build" first.`,
      );
    }

    const outDir = path.join(stageDir, jsPkg.id);
    fs.rmSync(outDir, { recursive: true, force: true });
    fs.mkdirSync(outDir, { recursive: true });
    const pkg = JSON.parse(fs.readFileSync(path.join(srcDir, "package.json"), "utf8"));
    pkg.version = resolvedVersion;
    if (jsPkg.packageName === "strange-quark") {
      // Inject exact-version optionalDependencies at stage time so the
      // workspace manifest stays `npm ci`-compatible before packages publish.
      pkg.optionalDependencies = expectedOptDeps;
    }
    fs.writeFileSync(path.join(outDir, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
    fs.copyFileSync(path.join(srcDir, "README.md"), path.join(outDir, "README.md"));
    fs.writeFileSync(path.join(outDir, "LICENSE"), rootLicense);
    copyDirRecursive(distDir, path.join(outDir, "dist"));

    manifestPackages.push({
      id: jsPkg.id,
      name: jsPkg.packageName,
      kind: "js",
      version: resolvedVersion,
      dir: outDir,
      staged: true,
      expectedFiles: [...jsPkg.expectedFiles].sort(),
    });
  }

  // 2. Platform binary packages (`@strange-quark/cli-<os>-<cpu>`).
  const missingPlatforms = [];
  const targetsToProcess = platformFilter
    ? PLATFORM_TARGETS.filter((t) => t.id === platformFilter)
    : PLATFORM_TARGETS;

  for (const target of targetsToProcess) {
    const srcDir = path.join(repoRoot, target.dir);
    const found = locatePlatformBinaries(targetDir, target, hostTarget);
    const outDir = path.join(stageDir, `cli-${target.id}`);

    fs.rmSync(outDir, { recursive: true, force: true });
    fs.mkdirSync(outDir, { recursive: true });
    const pkg = JSON.parse(fs.readFileSync(path.join(srcDir, "package.json"), "utf8"));
    pkg.version = resolvedVersion;
    fs.writeFileSync(path.join(outDir, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
    fs.copyFileSync(path.join(srcDir, "README.md"), path.join(outDir, "README.md"));
    fs.writeFileSync(path.join(outDir, "LICENSE"), rootLicense);
    fs.writeFileSync(path.join(outDir, "THIRD_PARTY_NOTICES.txt"), notices);

    const binDir = path.join(outDir, "bin");
    fs.rmSync(binDir, { recursive: true, force: true });

    let binaries = null;
    if (found) {
      fs.mkdirSync(binDir, { recursive: true });
      const dstSquark = path.join(binDir, found.squarkName);
      const dstRuntime = path.join(binDir, found.runtimeName);
      fs.copyFileSync(found.squarkPath, dstSquark);
      fs.copyFileSync(found.runtimePath, dstRuntime);
      if (target.exeSuffix === "") {
        fs.chmodSync(dstSquark, 0o755);
        fs.chmodSync(dstRuntime, 0o755);
      }
      binaries = {
        sourceDir: found.sourceDir,
        squark: {
          path: `bin/${found.squarkName}`,
          bytes: fs.statSync(dstSquark).size,
          sha256: sha256File(dstSquark),
        },
        runtime: {
          path: `bin/${found.runtimeName}`,
          bytes: fs.statSync(dstRuntime).size,
          sha256: sha256File(dstRuntime),
        },
      };
    } else {
      missingPlatforms.push(target.id);
    }

    manifestPackages.push({
      id: target.id,
      name: target.packageName,
      kind: "platform",
      version: resolvedVersion,
      dir: outDir,
      staged: Boolean(found),
      os: target.os,
      cpu: target.cpu,
      libc: target.libc,
      rustTriple: target.rustTriple,
      expectedFiles: expectedFilesForPlatform(target),
      binaries,
    });
  }

  if ((requireAll || platformFilter) && missingPlatforms.length > 0) {
    throw new Error(
      `Missing dist binaries for platform(s): ${missingPlatforms.join(", ")}. ` +
        `Expected squark and squark-runtime under ${targetDir}/<triple>/dist/ (or ${targetDir}/dist/ for host).`,
    );
  }

  const manifest = {
    schemaVersion: 1,
    version: resolvedVersion,
    generatedAt: new Date().toISOString(),
    targetDir,
    packages: manifestPackages,
  };

  if (manifestPath) {
    fs.mkdirSync(path.dirname(manifestPath), { recursive: true });
    fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  }

  return manifest;
}

function parseCliArgs(argv) {
  const opts = {
    version: null,
    targetDir: path.join(REPO_ROOT, "target"),
    stageDir: path.join(REPO_ROOT, "target/npm/stage"),
    manifestPath: path.join(REPO_ROOT, "target/npm/pack-manifest.json"),
    requireAll: false,
    platformFilter: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--version") opts.version = argv[++i];
    else if (arg === "--target-dir") opts.targetDir = path.resolve(argv[++i]);
    else if (arg === "--stage-dir") opts.stageDir = path.resolve(argv[++i]);
    else if (arg === "--manifest") opts.manifestPath = path.resolve(argv[++i]);
    else if (arg === "--platform") opts.platformFilter = argv[++i];
    else if (arg === "--require-all") opts.requireAll = true;
    else if (arg === "--help" || arg === "-h") {
      console.log(
        "Usage: node scripts/npm/stage.mjs [--version <ver>] [--target-dir <dir>] [--stage-dir <dir>] [--manifest <file>] [--platform <id>] [--require-all]",
      );
      process.exit(0);
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return opts;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    const opts = parseCliArgs(process.argv.slice(2));
    const manifest = stagePackages(opts);
    const stagedCount = manifest.packages.filter((p) => p.staged).length;
    console.log(
      `Staged ${stagedCount}/${manifest.packages.length} npm packages at version ${manifest.version}.`,
    );
    for (const pkg of manifest.packages) {
      const status = pkg.staged ? "staged" : "skipped (no binaries)";
      console.log(`  - ${pkg.name}@${pkg.version}: ${status}`);
    }
    console.log(`Pack manifest written to ${opts.manifestPath}`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
