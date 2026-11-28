#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Keeps all published npm package versions, Strange Quark's optionalDependencies,
// internal workspace dependency pins on published packages (e.g. tests/dev),
// LICENSE copies, and the Rust crates' versions (Cargo.toml, crates/*/Cargo.toml
// and Cargo.lock) in sync.
//
// Usage:
//   node scripts/npm/set-version.mjs 0.1.0
//   node scripts/npm/set-version.mjs --check

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { JS_PACKAGES, PLATFORM_TARGETS, REPO_ROOT, isValidSemver } from "./platforms.mjs";

const PUBLISHED_JS_NAMES = new Set(JS_PACKAGES.map((p) => p.packageName));
const WORKSPACE_SCAN_BASES = ["packages", "packages/@strange-quark", "apps", "tests"];

function findWorkspacePackageJsonPaths(repoRoot) {
  const results = [];
  for (const base of WORKSPACE_SCAN_BASES) {
    const absBase = path.join(repoRoot, base);
    if (!fs.existsSync(absBase)) continue;
    for (const ent of fs.readdirSync(absBase, { withFileTypes: true })) {
      if (!ent.isDirectory() || ent.name.startsWith("@")) continue;
      const candidate = path.join(absBase, ent.name, "package.json");
      if (fs.existsSync(candidate)) {
        results.push(candidate);
      }
    }
  }
  return results.sort();
}

export function syncVersions({ repoRoot = REPO_ROOT, version = null, check = false } = {}) {
  // Reported paths are repository-relative with "/" on every platform.
  const repoPath = (file) => path.relative(repoRoot, file).split(path.sep).join("/");
  const rootLicense = fs.readFileSync(path.join(repoRoot, "LICENSE"), "utf8");
  const allPackages = [
    ...JS_PACKAGES.map((p) => ({ name: p.packageName, dir: p.dir })),
    ...PLATFORM_TARGETS.map((p) => ({ name: p.packageName, dir: p.dir })),
  ];

  const mainPkgPath = path.join(repoRoot, "packages/strange-quark/package.json");
  const mainPkg = JSON.parse(fs.readFileSync(mainPkgPath, "utf8"));
  const targetVersion = version ?? mainPkg.version;

  if (!isValidSemver(targetVersion)) {
    throw new Error(`Invalid semver version: "${targetVersion}"`);
  }

  const issues = [];
  const updated = [];

  for (const entry of allPackages) {
    const pkgDir = path.join(repoRoot, entry.dir);
    const pkgJsonPath = path.join(pkgDir, "package.json");
    const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
    let dirty = false;

    if (pkg.version !== targetVersion) {
      if (check) {
        issues.push(`${entry.dir}/package.json version is ${pkg.version}, expected ${targetVersion}`);
      } else {
        pkg.version = targetVersion;
        dirty = true;
      }
    }

    if (entry.name === "strange-quark" && pkg.optionalDependencies !== undefined) {
      // Platform packages live outside root npm workspaces and may not yet be
      // published at targetVersion; keeping optionalDependencies out of the
      // workspace manifest avoids breaking `npm ci`. `scripts/npm/stage.mjs`
      // injects all 6 platform optionalDependencies into the staged package.
      if (check) {
        issues.push(
          `${entry.dir}/package.json must not declare optionalDependencies in the workspace source (injected at stage time by scripts/npm/stage.mjs)`,
        );
      } else {
        delete pkg.optionalDependencies;
        dirty = true;
      }
    }

    if (dirty) {
      fs.writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);
      updated.push(repoPath(pkgJsonPath));
    }

    const licensePath = path.join(pkgDir, "LICENSE");
    const existingLicense = fs.existsSync(licensePath) ? fs.readFileSync(licensePath, "utf8") : null;
    if (existingLicense !== rootLicense) {
      if (check) {
        issues.push(`${entry.dir}/LICENSE is missing or differs from root LICENSE`);
      } else {
        fs.writeFileSync(licensePath, rootLicense);
        updated.push(repoPath(licensePath));
      }
    }
  }

  // Keep internal workspace dependencies on published JS packages (e.g.
  // tests/dev depending on @strange-quark/vite-plugin) pinned to targetVersion so
  // `npm ci` resolves them from the workspace instead of failing on a stale pin.
  let workspaceRefsChecked = 0;
  for (const pkgJsonPath of findWorkspacePackageJsonPaths(repoRoot)) {
    const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
    const relPath = repoPath(pkgJsonPath);
    let dirty = false;
    for (const field of ["dependencies", "devDependencies", "optionalDependencies"]) {
      const deps = pkg[field];
      if (!deps || typeof deps !== "object") continue;
      for (const depName of Object.keys(deps)) {
        if (!PUBLISHED_JS_NAMES.has(depName)) continue;
        workspaceRefsChecked++;
        if (deps[depName] !== targetVersion) {
          if (check) {
            issues.push(
              `${relPath} ${field}["${depName}"] is "${deps[depName]}", expected "${targetVersion}"`,
            );
          } else {
            deps[depName] = targetVersion;
            dirty = true;
          }
        }
      }
    }
    if (dirty) {
      fs.writeFileSync(pkgJsonPath, `${JSON.stringify(pkg, null, 2)}\n`);
      updated.push(relPath);
    }
  }

  const cratesChecked = syncCargoVersions({ repoRoot, targetVersion, check, issues, updated, repoPath });

  if (check && issues.length > 0) {
    throw new Error(`Version/LICENSE sync check failed:\n  - ${issues.join("\n  - ")}`);
  }

  return { version: targetVersion, updated, packages: allPackages.length, workspaceRefsChecked, cratesChecked };
}

/**
 * Keeps the Rust crates on the release version, so `squark --version` and
 * `squark-runtime --version` report it: `[workspace.package] version` in
 * Cargo.toml, every internal `squark-*` version requirement (an exact `=`
 * stays exact), and the crates' entries in Cargo.lock. Skipped when the
 * repository has no Cargo.toml.
 */
function syncCargoVersions({ repoRoot, targetVersion, check, issues, updated, repoPath }) {
  const rootManifest = path.join(repoRoot, "Cargo.toml");
  if (!fs.existsSync(rootManifest)) return 0;
  const cratesDir = path.join(repoRoot, "crates");
  const manifests = [
    rootManifest,
    ...(fs.existsSync(cratesDir)
      ? fs
          .readdirSync(cratesDir)
          .map((name) => path.join(cratesDir, name, "Cargo.toml"))
          .filter((file) => fs.existsSync(file))
      : []),
  ];
  const edits = [
    {
      files: [rootManifest],
      pattern: /(\[workspace\.package\][^[]*?\r?\nversion = ")([^"]+)(")/g,
      what: "[workspace.package] version",
    },
    {
      files: manifests,
      pattern: /^(squark-[a-z-]+\s*=\s*\{[^}\r\n]*\bversion\s*=\s*"=?)([^"]+)(")/gm,
      what: "internal crate version requirement",
    },
    {
      files: [path.join(repoRoot, "Cargo.lock")].filter((file) => fs.existsSync(file)),
      pattern: /(\[\[package\]\]\r?\nname = "squark-[a-z-]+"\r?\nversion = ")([^"]+)(")/g,
      what: "Cargo.lock entry",
    },
  ];
  let checked = 0;
  for (const { files, pattern, what } of edits) {
    for (const file of files) {
      const source = fs.readFileSync(file, "utf8");
      let dirty = false;
      const next = source.replace(pattern, (match, before, found, after) => {
        checked++;
        if (found === targetVersion) return match;
        if (check) {
          issues.push(`${repoPath(file)} ${what} is "${found}", expected "${targetVersion}"`);
          return match;
        }
        dirty = true;
        return `${before}${targetVersion}${after}`;
      });
      if (dirty) {
        fs.writeFileSync(file, next);
        if (!updated.includes(repoPath(file))) updated.push(repoPath(file));
      }
    }
  }
  return checked;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const args = process.argv.slice(2);
  const check = args.includes("--check");
  const versionArg = args.find((a) => !a.startsWith("-")) ?? null;

  if (!check && !versionArg) {
    console.error("Usage: node scripts/npm/set-version.mjs <version> | --check");
    process.exit(2);
  }

  try {
    const result = syncVersions({ version: versionArg, check });
    if (check) {
      console.log(`All ${result.packages} npm packages are synced at version ${result.version}.`);
    } else if (result.updated.length === 0) {
      console.log(`All ${result.packages} npm packages already at version ${result.version}.`);
    } else {
      console.log(`Synced ${result.packages} npm packages to ${result.version} (${result.updated.length} file(s) updated):`);
      for (const f of result.updated) {
        console.log(`  - ${f}`);
      }
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
