#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Inventory a React codebase for a Strange Quark migration.
//
//   node scan.mjs <repo> [--out inventory.json] [--max-hits 20]
//
// Detects each package's source kind (meta-framework, desktop shell or plain
// React), its React version and bundler, and every server-only or shell-only
// marker and unsupported web feature listed in compat.json. No dependencies.
// It reports patterns; deciding what they mean is the reader's job.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const compat = JSON.parse(fs.readFileSync(path.join(here, "compat.json"), "utf8"));

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};
const repo = path.resolve(args.find((a, i) => !a.startsWith("--") && !(i > 0 && args[i - 1].startsWith("--"))) || ".");
const outPath = opt("--out", null);
const maxHits = Number(opt("--max-hits", "20"));

const SKIP_DIRS = new Set([
  "node_modules", ".git", "dist", "build", "out", ".next", ".output", ".vinxi", ".turbo",
  ".svelte-kit", "coverage", "target", ".vercel", ".netlify", ".cache", ".parcel-cache",
  "strange-quark-migration", ".react-router", "release", "artifacts",
]);
const CODE_EXT = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts", ".html", ".vue", ".svelte", ".astro", ".mdx"]);
const STYLE_EXT = new Set([".css", ".scss", ".sass", ".less", ".pcss", ".styl"]);
const MAX_FILE_BYTES = 1_000_000;

function walk(dir, files = []) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return files;
  }
  for (const e of entries) {
    if (e.name.startsWith(".") && e.name !== ".storybook") {
      if (e.isDirectory()) continue;
    }
    const full = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) walk(full, files);
    } else if (e.isFile()) {
      files.push(full);
    }
  }
  return files;
}

const rel = (p) => path.relative(repo, p) || ".";
const files = walk(repo);

// ---- packages and source kinds ------------------------------------------------

function readJson(p) {
  try {
    return JSON.parse(fs.readFileSync(p, "utf8"));
  } catch {
    return null;
  }
}

function installedVersion(pkgDir, name) {
  let dir = pkgDir;
  while (true) {
    const pj = readJson(path.join(dir, "node_modules", name, "package.json"));
    if (pj?.version) return pj.version;
    const parent = path.dirname(dir);
    if (parent === dir || !parent.startsWith(repo)) return null;
    dir = parent;
  }
}

function detectPackage(pjPath) {
  const pj = readJson(pjPath);
  if (!pj) return null;
  const dir = path.dirname(pjPath);
  const deps = { ...pj.dependencies, ...pj.devDependencies, ...pj.peerDependencies };
  const has = (n) => Object.prototype.hasOwnProperty.call(deps, n);
  const hasPrefix = (p) => Object.keys(deps).some((n) => n.startsWith(p));
  const exists = (p) => fs.existsSync(path.join(dir, p));
  const kinds = [];
  if (has("next")) kinds.push("nextjs");
  if (has("@tanstack/react-start") || has("@tanstack/start")) kinds.push("tanstack-start");
  if (has("@react-router/dev") || exists("react-router.config.ts") || exists("react-router.config.js")) kinds.push("react-router-framework");
  if (hasPrefix("@remix-run/")) kinds.push("remix");
  if (has("gatsby")) kinds.push("gatsby");
  if (has("astro")) kinds.push("astro");
  if (has("electron") || has("electron-vite") || has("electron-builder") || hasPrefix("@electron-forge/")) kinds.push("electron");
  if (has("@tauri-apps/api") || has("@tauri-apps/cli") || exists("src-tauri/tauri.conf.json") || exists("src-tauri/Cargo.toml")) kinds.push("tauri");
  if (has("electrobun")) kinds.push("electrobun");
  const usesReact = has("react") || has("react-dom");
  if (kinds.length === 0 && usesReact) kinds.push("react-spa");
  const bundlers = ["vite", "webpack", "react-scripts", "parcel", "@rsbuild/core", "esbuild", "rollup", "@rspack/core"].filter(has);
  if (has("next")) bundlers.push("next (webpack or turbopack)");
  return {
    path: rel(dir),
    name: pj.name ?? null,
    kinds,
    react: {
      declared: deps.react ?? null,
      installed: installedVersion(dir, "react"),
      reactDomDeclared: deps["react-dom"] ?? null,
    },
    bundlers,
    routers: ["react-router", "react-router-dom", "@tanstack/react-router", "wouter", "@reach/router"].filter(has),
    deps: Object.keys(deps),
  };
}

const packages = files
  .filter((f) => path.basename(f) === "package.json")
  .map(detectPackage)
  .filter((p) => p && (p.kinds.length > 0 || p.react.declared));

// ---- pattern scan --------------------------------------------------------------

function kindOf(file) {
  const ext = path.extname(file).toLowerCase();
  if (CODE_EXT.has(ext)) return "code";
  if (STYLE_EXT.has(ext)) return "style";
  return null;
}

function compile(entry) {
  return {
    ...entry,
    re: entry.pattern ? new RegExp(entry.pattern, "m") : null,
    pathRe: entry.pathPattern ? new RegExp(entry.pathPattern) : null,
  };
}
const markers = compat.markers.map(compile);
const features = compat.features.map(compile);
const results = new Map();
const record = (entry, hit) => {
  if (!results.has(entry.id)) {
    results.set(entry.id, { id: entry.id, status: entry.status, label: entry.label, note: entry.note ?? null, hits: [], count: 0, deps: [] });
  }
  const r = results.get(entry.id);
  r.count += 1;
  if (r.hits.length < maxHits) r.hits.push(hit);
};

for (const file of files) {
  const kind = kindOf(file);
  const relPath = rel(file);
  for (const entry of [...markers, ...features]) {
    if (entry.pathRe && entry.pathRe.test(relPath)) record(entry, { file: relPath, line: null, text: null });
  }
  if (!kind) continue;
  let text;
  try {
    if (fs.statSync(file).size > MAX_FILE_BYTES) continue;
    text = fs.readFileSync(file, "utf8");
  } catch {
    continue;
  }
  const lines = text.split(/\r?\n/);
  for (const entry of [...markers, ...features]) {
    if (!entry.re) continue;
    const where = entry.where ?? "code";
    if (where !== "any" && where !== kind) continue;
    if (!entry.re.test(text)) continue;
    for (let i = 0; i < lines.length; i++) {
      if (entry.re.test(lines[i])) {
        record(entry, { file: relPath, line: i + 1, text: lines[i].trim().slice(0, 160) });
      }
    }
  }
}

// Dependencies that imply a feature even when no call site matched.
for (const pkg of packages) {
  for (const entry of features) {
    for (const dep of entry.deps ?? []) {
      if (pkg.deps.includes(dep)) {
        record(entry, { file: `${pkg.path}/package.json`, line: null, text: `depends on ${dep}` });
        results.get(entry.id).deps.push(dep);
      }
    }
  }
}

// ---- the seam: every channel the UI uses to reach a server or a shell backend --------

const ENDPOINT_RULES = [
  { kind: "electron-ipc-handler", ext: CODE_EXT, re: /ipcMain\.(?:handle|handleOnce|on|once)\(\s*['"`]([^'"`]+)['"`]/g },
  { kind: "electron-ipc-call", ext: CODE_EXT, re: /ipcRenderer\.(?:invoke|send|sendSync|on|once)\(\s*['"`]([^'"`]+)['"`]/g },
  { kind: "electron-exposed-api", ext: CODE_EXT, re: /exposeInMainWorld\(\s*['"`]([^'"`]+)['"`]/g },
  { kind: "tauri-command-call", ext: CODE_EXT, when: /@tauri-apps|__TAURI/, re: /(?<![.\w])invoke\s*(?:<[^>()]*>)?\(\s*['"`]([^'"`]+)['"`]/g },
  { kind: "tauri-plugin", ext: CODE_EXT, re: /@tauri-apps\/plugin-([a-z-]+)/g },
  { kind: "tauri-command-definition", ext: new Set([".rs"]), re: /#\[tauri::command\][^\n]*\n\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/g },
  { kind: "electrobun-rpc-call", ext: CODE_EXT, re: /rpc\.(?:request|send)\.(\w+)/g },
  { kind: "server-function", ext: CODE_EXT, re: /(?:export\s+)?const\s+(\w+)\s*=\s*createServerFn\b/g },
];
const endpoints = new Map();
for (const file of files) {
  const ext = path.extname(file).toLowerCase();
  const rules = ENDPOINT_RULES.filter((r) => r.ext.has(ext));
  if (!rules.length) continue;
  let text;
  try {
    if (fs.statSync(file).size > MAX_FILE_BYTES) continue;
    text = fs.readFileSync(file, "utf8");
  } catch {
    continue;
  }
  for (const r of rules) {
    if (r.when && !r.when.test(text)) continue;
    for (const m of text.matchAll(r.re)) {
      if (!m[1]) continue;
      const key = `${r.kind}:${m[1]}`;
      const line = text.slice(0, m.index).split("\n").length;
      if (!endpoints.has(key)) endpoints.set(key, { kind: r.kind, name: m[1], locations: [] });
      const e = endpoints.get(key);
      if (e.locations.length < 5) e.locations.push(`${rel(file)}:${line}`);
    }
  }
  // Server actions: exported functions of a 'use server' module.
  if (CODE_EXT.has(ext) && /^\s*['"]use server['"]/m.test(text)) {
    for (const m of text.matchAll(/export\s+(?:async\s+)?function\s+(\w+)/g)) {
      endpoints.set(`server-action:${m[1]}`, { kind: "server-action", name: m[1], locations: [rel(file)] });
    }
  }
  // Route handlers: app/**/route.ts and pages/api/** map to URL paths.
  const relPath = rel(file).replace(/\\/g, "/");
  const route = /(?:^|\/)app\/(.*?)\/?route\.(?:ts|js|tsx|jsx|mjs)$/.exec(relPath) ?? /(?:^|\/)pages\/(api\/.*)\.(?:ts|js|tsx|jsx)$/.exec(relPath);
  if (route) {
    const methods = [...text.matchAll(/export\s+(?:async\s+)?(?:function|const)\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b/g)].map((m) => m[1]);
    const urlPath = "/" + route[1].replace(/\(.*?\)\//g, "").replace(/\/index$/, "");
    endpoints.set(`route:${urlPath}`, { kind: "route-handler", name: `${methods.join(",") || "default"} ${urlPath}`, locations: [relPath] });
  }
}

const findings = [...results.values()].sort((a, b) => {
  const order = ["server", "shell", "node", "not-planned", "tier2", "limited", "framework"];
  return order.indexOf(a.status) - order.indexOf(b.status) || b.count - a.count;
});

const inventory = {
  repo,
  scannedFiles: files.length,
  compatSnapshot: compat.snapshot,
  packages: packages.map(({ deps, ...p }) => p),
  endpoints: [...endpoints.values()].sort((a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name)),
  findings,
};

if (outPath) {
  fs.mkdirSync(path.dirname(path.resolve(outPath)), { recursive: true });
  fs.writeFileSync(outPath, JSON.stringify(inventory, null, 2) + "\n");
}

// ---- human summary ----------------------------------------------------------------

const lines = [];
lines.push(`strange-quark inventory of ${repo} (${files.length} files scanned)`);
for (const p of packages) {
  const react = p.react.installed ? `react ${p.react.installed}` : p.react.declared ? `react ${p.react.declared}` : "no react";
  lines.push(`  package ${p.path} [${p.kinds.join(", ") || "no react"}] ${react}; bundler: ${p.bundlers.join(", ") || "none found"}; router: ${p.routers.join(", ") || "none"}`);
}
if (findings.length === 0) {
  lines.push("No server, shell or unsupported-feature patterns found.");
} else {
  lines.push("Findings (status: what, count, first locations):");
  for (const f of findings) {
    const where = f.hits.slice(0, 3).map((h) => (h.line ? `${h.file}:${h.line}` : h.file)).join(", ");
    lines.push(`  [${f.status}] ${f.label}: ${f.count} (${where})`);
    if (f.note) lines.push(`      ${f.note}`);
  }
}
if (inventory.endpoints.length) {
  lines.push("The seam (calls from the UI to a server or shell backend, and their definitions):");
  for (const e of inventory.endpoints) lines.push(`  ${e.kind}: ${e.name} (${e.locations.join(", ")})`);
}
lines.push(`Statuses: ${Object.entries(compat.statuses).map(([k, v]) => `${k} = ${v}`).join(" | ")}`);
console.log(lines.join("\n"));
