// SPDX-License-Identifier: MIT
// Single source of truth for all published npm packages and platform targets.

import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(__dirname, "../..");

export const MIN_USER_NODE_ENGINE = ">=22.18.0";

export const PLATFORM_TARGETS = [
  {
    id: "darwin-arm64",
    packageName: "@strange-quark/cli-darwin-arm64",
    dir: "npm/cli-darwin-arm64",
    os: "darwin",
    cpu: "arm64",
    libc: null,
    rustTriple: "aarch64-apple-darwin",
    exeSuffix: "",
    label: "macOS arm64 (Apple Silicon)",
  },
  {
    id: "darwin-x64",
    packageName: "@strange-quark/cli-darwin-x64",
    dir: "npm/cli-darwin-x64",
    os: "darwin",
    cpu: "x64",
    libc: null,
    rustTriple: "x86_64-apple-darwin",
    exeSuffix: "",
    label: "macOS x64 (Intel)",
  },
  {
    id: "linux-x64",
    packageName: "@strange-quark/cli-linux-x64",
    dir: "npm/cli-linux-x64",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
    rustTriple: "x86_64-unknown-linux-gnu",
    exeSuffix: "",
    label: "Linux x64 (glibc)",
  },
  {
    id: "linux-arm64",
    packageName: "@strange-quark/cli-linux-arm64",
    dir: "npm/cli-linux-arm64",
    os: "linux",
    cpu: "arm64",
    libc: "glibc",
    rustTriple: "aarch64-unknown-linux-gnu",
    exeSuffix: "",
    label: "Linux arm64 (glibc)",
  },
  {
    id: "win32-x64",
    packageName: "@strange-quark/cli-win32-x64",
    dir: "npm/cli-win32-x64",
    os: "win32",
    cpu: "x64",
    libc: null,
    rustTriple: "x86_64-pc-windows-msvc",
    exeSuffix: ".exe",
    label: "Windows x64",
  },
  {
    id: "win32-arm64",
    packageName: "@strange-quark/cli-win32-arm64",
    dir: "npm/cli-win32-arm64",
    os: "win32",
    cpu: "arm64",
    libc: null,
    rustTriple: "aarch64-pc-windows-msvc",
    exeSuffix: ".exe",
    label: "Windows arm64",
  },
];

export const JS_PACKAGES = [
  {
    id: "strange-quark",
    packageName: "strange-quark",
    dir: "packages/strange-quark",
    scoped: false,
    expectedFiles: [
      "LICENSE",
      "README.md",
      "dist/cli.d.ts",
      "dist/cli.js",
      "dist/config.d.ts",
      "dist/config.js",
      "dist/index.d.ts",
      "dist/index.js",
      "dist/load-config.d.ts",
      "dist/load-config.js",
      "dist/resolve-bin.d.ts",
      "dist/resolve-bin.js",
      "package.json",
    ],
  },
  {
    id: "vite-plugin",
    packageName: "@strange-quark/vite-plugin",
    dir: "packages/@strange-quark/vite-plugin",
    scoped: true,
    expectedFiles: [
      "LICENSE",
      "README.md",
      "dist/dev-main.d.ts",
      "dist/dev-main.js",
      "dist/index.d.ts",
      "dist/index.js",
      "dist/launcher.d.ts",
      "dist/launcher.js",
      "dist/manifest.d.ts",
      "dist/manifest.js",
      "package.json",
    ],
  },
];

export function expectedFilesForPlatform(target) {
  return [
    "LICENSE",
    "README.md",
    "THIRD_PARTY_NOTICES.txt",
    `bin/squark${target.exeSuffix}`,
    `bin/squark-runtime${target.exeSuffix}`,
    "package.json",
  ].sort();
}

export function hostPlatformTarget(platform = process.platform, arch = process.arch) {
  return PLATFORM_TARGETS.find((target) => target.os === platform && target.cpu === arch) ?? null;
}

export function isValidSemver(value) {
  return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(value);
}
