// SPDX-License-Identifier: MIT
// Configuration types, defineConfig helper, and config loader for squark.config.{ts,mjs,js,json}.

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export interface SquarkWindowConfig {
  title?: string;
  width?: number;
  height?: number;
  resizable?: boolean;
}

export interface SquarkBuildConfig {
  /** Shell command to run the app's own build (default: auto-detected yarn or npm run build). */
  command?: string;
  /** Extra environment variables passed to the app's build command. */
  env?: Record<string, string>;
  /** Built web output directory relative to project root (default: auto-detect dist/ then build/). */
  distDir?: string;
  /** Entry HTML file relative to distDir (default: index.html). */
  entryHtml?: string;
  /** Include .map sourcemap files in the .sqpak archive (default: false). */
  includeSourcemaps?: boolean;
  /** Path to the prebuilt squark-runtime binary. */
  runtime?: string;
  /** Output binary path relative to project root (default: dist/<name>). */
  out?: string;
}

export interface SquarkAssetConfig {
  /** Absolute pack path the app requests, e.g. "/assets/Font-Bold.woff2". */
  path: string;
  /** File to read the bytes from, relative to the config file's directory. */
  file: string;
}

export interface SquarkNetworkConfig {
  /** Allowed external origins or host patterns for runtime fetch/WebSocket. */
  allow?: string[];
}

export interface SquarkConfig {
  name?: string;
  /** Extra files packed at exact paths the app build did not emit. */
  assets?: SquarkAssetConfig[];
  window?: SquarkWindowConfig;
  build?: SquarkBuildConfig;
  network?: SquarkNetworkConfig;
}

export interface ResolvedSquarkConfig {
  name: string;
  configPath: string | null;
  window: {
    title: string;
    width: number;
    height: number;
    resizable: boolean;
  };
  build: {
    command: string | null;
    env: Record<string, string>;
    distDir: string | null;
    entryHtml: string;
    includeSourcemaps: boolean;
    runtime: string | null;
    out: string | null;
  };
  network: {
    allow: string[];
  };
  assets: SquarkAssetConfig[];
}

const CONFIG_FILENAMES = [
  "squark.config.ts",
  "squark.config.mjs",
  "squark.config.js",
  "squark.config.json",
] as const;

/**
 * Identity helper providing strict TypeScript autocompletion and runtime validation
 * for `squark.config.ts` files.
 */
export function defineConfig(config: SquarkConfig): SquarkConfig {
  validateConfig(config);
  return config;
}

export function validateConfig(config: unknown): asserts config is SquarkConfig {
  if (typeof config !== "object" || config === null || Array.isArray(config)) {
    throw new TypeError("squark.config must export a plain object");
  }
  const c = config as SquarkConfig;
  if (c.name !== undefined && (typeof c.name !== "string" || c.name.trim() === "")) {
    throw new TypeError("squark.config.name must be a non-empty string");
  }
  if (c.window !== undefined) {
    if (typeof c.window !== "object" || c.window === null) {
      throw new TypeError("squark.config.window must be an object");
    }
    if (
      c.window.width !== undefined &&
      (!Number.isFinite(c.window.width) || c.window.width <= 0)
    ) {
      throw new RangeError("squark.config.window.width must be a positive number");
    }
    if (
      c.window.height !== undefined &&
      (!Number.isFinite(c.window.height) || c.window.height <= 0)
    ) {
      throw new RangeError("squark.config.window.height must be a positive number");
    }
  }
  if (c.assets !== undefined) {
    if (!Array.isArray(c.assets)) {
      throw new TypeError("squark.config.assets must be an array");
    }
    for (const asset of c.assets) {
      if (typeof asset?.path !== "string" || !asset.path.startsWith("/")) {
        throw new TypeError("squark.config.assets[].path must start with '/'");
      }
      if (typeof asset.file !== "string" || asset.file.trim() === "") {
        throw new TypeError("squark.config.assets[].file must be a non-empty string");
      }
    }
  }
  if (c.network?.allow !== undefined) {
    if (
      !Array.isArray(c.network.allow) ||
      c.network.allow.some((entry) => typeof entry !== "string")
    ) {
      throw new TypeError("squark.config.network.allow must be an array of strings");
    }
  }
}

function sanitizeBinaryName(raw: string): string {
  const unscoped = raw.includes("/") ? raw.slice(raw.lastIndexOf("/") + 1) : raw;
  const cleaned = unscoped.replace(/[^a-zA-Z0-9._-]/g, "-").replace(/^-+|-+$/g, "");
  return cleaned || "squark-app";
}

function inferProjectName(projectDir: string): string {
  const pkgPath = path.join(projectDir, "package.json");
  if (fs.existsSync(pkgPath)) {
    try {
      const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8")) as { name?: unknown };
      if (typeof pkg.name === "string" && pkg.name.trim() !== "") {
        return sanitizeBinaryName(pkg.name.trim());
      }
    } catch {
      // Fall back to directory name.
    }
  }
  return sanitizeBinaryName(path.basename(projectDir));
}

export function findConfigFile(projectDir: string, explicitPath?: string | null): string | null {
  if (explicitPath) {
    const resolved = path.resolve(projectDir, explicitPath);
    if (!fs.existsSync(resolved)) {
      throw new Error(`Config file not found: ${resolved}`);
    }
    return resolved;
  }
  for (const file of CONFIG_FILENAMES) {
    const candidate = path.join(projectDir, file);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

async function loadRawConfig(configPath: string): Promise<SquarkConfig> {
  if (configPath.endsWith(".json")) {
    const parsed: unknown = JSON.parse(fs.readFileSync(configPath, "utf8"));
    validateConfig(parsed);
    return parsed;
  }
  const url = pathToFileURL(configPath);
  url.searchParams.set("t", String(fs.statSync(configPath).mtimeMs));
  const mod = (await import(url.href)) as { default?: unknown };
  const exported = mod.default ?? mod;
  const raw = typeof exported === "function" ? await exported() : exported;
  validateConfig(raw);
  return raw;
}

/**
 * Loads `squark.config.{ts,mjs,js,json}` from `projectDir` (or `explicitConfigPath`)
 * and merges defaults inferred from `package.json`.
 */
export async function resolveConfig(
  projectDir: string,
  options: { configPath?: string | null } = {},
): Promise<ResolvedSquarkConfig> {
  const absDir = path.resolve(projectDir);
  const configPath = findConfigFile(absDir, options.configPath);
  const raw: SquarkConfig = configPath ? await loadRawConfig(configPath) : {};

  const name = raw.name ? sanitizeBinaryName(raw.name) : inferProjectName(absDir);
  const title = raw.window?.title ?? name;

  return {
    name,
    configPath,
    window: {
      title,
      width: raw.window?.width ?? 1280,
      height: raw.window?.height ?? 800,
      resizable: raw.window?.resizable ?? true,
    },
    build: {
      command: raw.build?.command ?? null,
      env: { ...(raw.build?.env ?? {}) },
      distDir: raw.build?.distDir ?? null,
      entryHtml: raw.build?.entryHtml ?? "index.html",
      includeSourcemaps: raw.build?.includeSourcemaps ?? false,
      runtime: raw.build?.runtime ?? null,
      out: raw.build?.out ?? null,
    },
    network: {
      allow: [...(raw.network?.allow ?? [])],
    },
    assets: (raw.assets ?? []).map((asset) => ({ path: asset.path, file: asset.file })),
  };
}
