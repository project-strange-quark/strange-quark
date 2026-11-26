#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Verifies that every installed npm package in the workspace uses a licence
// permitted by AGENTS.md ("Licences"). Scans node_modules/ excluding vendor/.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ALLOWED_LICENSES = new Set([
  "MIT",
  "Apache-2.0",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "Zlib",
  "Unicode-3.0",
  "CC0-1.0",
  "0BSD",
  "MPL-2.0",
]);

// Parse simple SPDX expressions supporting OR, AND, WITH, parentheses.
// Returns true iff the licence expression can be satisfied using only ALLOWED_LICENSES.
export function isSpdxAllowed(raw) {
  if (!raw || typeof raw !== "string") return false;
  const trimmed = raw.trim();
  if (!trimmed) return false;

  // Tokenize parentheses, AND, OR, WITH, and identifiers.
  const tokens = trimmed.match(/\(|\)|\bAND\b|\bOR\b|\bWITH\b|[A-Za-z0-9.+_-]+/g);
  if (!tokens || tokens.length === 0) return false;
  let pos = 0;

  function parseOr() {
    let val = parseAnd();
    while (tokens[pos] === "OR") {
      pos++;
      const rhs = parseAnd();
      val = val || rhs;
    }
    return val;
  }

  function parseAnd() {
    let val = parseAtom();
    while (tokens[pos] === "AND") {
      pos++;
      const rhs = parseAtom();
      val = val && rhs;
    }
    return val;
  }

  function parseAtom() {
    if (tokens[pos] === "(") {
      pos++;
      const val = parseOr();
      if (tokens[pos] !== ")") return false;
      pos++;
      return val;
    }
    const id = tokens[pos++];
    if (!id) return false;
    // Strip optional "WITH <exception>" (e.g., Apache-2.0 WITH LLVM-exception)
    if (tokens[pos] === "WITH") {
      pos += 2;
    }
    return ALLOWED_LICENSES.has(id);
  }

  const ok = parseOr();
  return ok && pos === tokens.length;
}

function normalizeLicenseField(pkg) {
  if (typeof pkg.license === "string") return pkg.license;
  if (pkg.license && typeof pkg.license.type === "string") return pkg.license.type;
  if (Array.isArray(pkg.licenses) && pkg.licenses.length > 0) {
    return pkg.licenses.map((l) => (typeof l === "string" ? l : l.type)).filter(Boolean).join(" OR ");
  }
  return null;
}

function collectNodeModulesDirs(repoRoot) {
  const dirs = [];
  const rootNm = path.join(repoRoot, "node_modules");
  if (fs.existsSync(rootNm)) dirs.push(rootNm);

  for (const base of ["packages", "apps", "tests"]) {
    const abs = path.join(repoRoot, base);
    if (!fs.existsSync(abs)) continue;
    for (const ent of fs.readdirSync(abs, { withFileTypes: true })) {
      if (!ent.isDirectory()) continue;
      const candidate = path.join(abs, ent.name, "node_modules");
      if (fs.existsSync(candidate)) dirs.push(candidate);
      if (ent.name.startsWith("@")) {
        for (const sub of fs.readdirSync(path.join(abs, ent.name), { withFileTypes: true })) {
          if (!sub.isDirectory()) continue;
          const scopedNm = path.join(abs, ent.name, sub.name, "node_modules");
          if (fs.existsSync(scopedNm)) dirs.push(scopedNm);
        }
      }
    }
  }
  return dirs;
}

function scanNodeModules(nmDir, visited, violations, checked) {
  for (const ent of fs.readdirSync(nmDir, { withFileTypes: true })) {
    if (ent.name.startsWith(".")) continue;
    const full = path.join(nmDir, ent.name);
    if (ent.name.startsWith("@") && ent.isDirectory()) {
      scanNodeModules(full, visited, violations, checked);
      continue;
    }
    const pkgPath = path.join(full, "package.json");
    if (!fs.existsSync(pkgPath)) continue;
    const real = fs.realpathSync(full);
    if (visited.has(real)) continue;
    visited.add(real);

    // Skip local workspace symlinks (they are MIT in this repo).
    const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8"));
    const lic = normalizeLicenseField(pkg);
    const id = `${pkg.name ?? ent.name}@${pkg.version ?? "?"}`;
    checked.push({ id, license: lic });
    if (!isSpdxAllowed(lic)) {
      violations.push({ id, license: lic ?? "(missing)", path: full });
    }

    const nested = path.join(full, "node_modules");
    if (fs.existsSync(nested)) {
      scanNodeModules(nested, visited, violations, checked);
    }
  }
}

export function checkWorkspaceLicenses(repoRoot) {
  const visited = new Set();
  const violations = [];
  const checked = [];
  for (const nmDir of collectNodeModulesDirs(repoRoot)) {
    scanNodeModules(nmDir, visited, violations, checked);
  }
  return { checked, violations };
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const { checked, violations } = checkWorkspaceLicenses(repoRoot);
  if (violations.length > 0) {
    console.error(`npm licence check FAILED (${violations.length} violation(s) out of ${checked.length} packages):`);
    for (const v of violations) {
      console.error(`  - ${v.id}: ${v.license} (${path.relative(repoRoot, v.path)})`);
    }
    process.exit(1);
  }
  console.log(`npm licence check passed (${checked.length} packages checked).`);
}
