// SPDX-License-Identifier: MIT
// Resolves the native Rust `squark` executable for the current host platform.
// Precedence:
//   1. Platform optionalDependency package (@strange-quark/cli-<os>-<cpu>)
//   2. $SQUARK_BIN environment variable
//   3. `squark` (or `squark.exe` on Windows) on PATH, excluding this JS shim itself.

import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

export interface PlatformPackageInfo {
  readonly platform: string;
  readonly arch: string;
  readonly packageName: string;
  readonly binRelativePath: string;
  readonly runtimeRelativePath: string;
  readonly libc?: "glibc";
}

export const PLATFORM_PACKAGES: readonly PlatformPackageInfo[] = [
  {
    platform: "darwin",
    arch: "arm64",
    packageName: "@strange-quark/cli-darwin-arm64",
    binRelativePath: "bin/squark",
    runtimeRelativePath: "bin/squark-runtime",
  },
  {
    platform: "darwin",
    arch: "x64",
    packageName: "@strange-quark/cli-darwin-x64",
    binRelativePath: "bin/squark",
    runtimeRelativePath: "bin/squark-runtime",
  },
  {
    platform: "linux",
    arch: "x64",
    packageName: "@strange-quark/cli-linux-x64",
    binRelativePath: "bin/squark",
    runtimeRelativePath: "bin/squark-runtime",
    libc: "glibc",
  },
  {
    platform: "linux",
    arch: "arm64",
    packageName: "@strange-quark/cli-linux-arm64",
    binRelativePath: "bin/squark",
    runtimeRelativePath: "bin/squark-runtime",
    libc: "glibc",
  },
  {
    platform: "win32",
    arch: "x64",
    packageName: "@strange-quark/cli-win32-x64",
    binRelativePath: "bin/squark.exe",
    runtimeRelativePath: "bin/squark-runtime.exe",
  },
  {
    platform: "win32",
    arch: "arm64",
    packageName: "@strange-quark/cli-win32-arm64",
    binRelativePath: "bin/squark.exe",
    runtimeRelativePath: "bin/squark-runtime.exe",
  },
] as const;

export interface ResolveSquarkBinaryResult {
  binaryPath: string;
  source: "platform-package" | "env" | "path";
  packageName?: string;
}

export interface ResolveSquarkBinaryOptions {
  platform?: string;
  arch?: string;
  env?: NodeJS.ProcessEnv;
  currentScriptPath?: string;
  resolvePackage?: (specifier: string) => string;
  existsSync?: (filePath: string) => boolean;
  isExecutableFile?: (filePath: string) => boolean;
  realpathSync?: (filePath: string) => string;
  isLinuxGlibc?: () => boolean | null;
}

export class SquarkBinaryNotFoundError extends Error {
  readonly platform: string;
  readonly arch: string;
  readonly expectedPackage: string | null;

  constructor(
    message: string,
    details: { platform: string; arch: string; expectedPackage: string | null },
  ) {
    super(message);
    this.name = "SquarkBinaryNotFoundError";
    this.platform = details.platform;
    this.arch = details.arch;
    this.expectedPackage = details.expectedPackage;
  }
}

export function getPlatformPackage(
  platform: string = process.platform,
  arch: string = process.arch,
): PlatformPackageInfo | null {
  return (
    PLATFORM_PACKAGES.find((entry) => entry.platform === platform && entry.arch === arch) ?? null
  );
}

const defaultRequire = createRequire(import.meta.url);

function defaultResolvePackage(specifier: string): string {
  try {
    return defaultRequire.resolve(specifier);
  } catch {
    const resolvedUrl = import.meta.resolve(specifier);
    return fileURLToPath(resolvedUrl);
  }
}

function defaultIsExecutableFile(filePath: string): boolean {
  try {
    const stat = fs.statSync(filePath);
    if (!stat.isFile()) return false;
    if (process.platform !== "win32") {
      fs.accessSync(filePath, fs.constants.X_OK);
    }
    return true;
  } catch {
    return false;
  }
}

function defaultIsLinuxGlibc(): boolean | null {
  try {
    const report = process.report?.getReport?.() as
      | { header?: { glibcVersionRuntime?: string } }
      | undefined;
    if (!report || !report.header) return null;
    return Boolean(report.header.glibcVersionRuntime);
  } catch {
    return null;
  }
}

function safeRealpath(
  filePath: string | undefined,
  realpathSync: (p: string) => string,
): string | null {
  if (!filePath) return null;
  try {
    return realpathSync(filePath);
  } catch {
    return null;
  }
}

function findOnPath(
  binaryName: string,
  platform: string,
  env: NodeJS.ProcessEnv,
  selfRealpath: string | null,
  isExecutableFile: (p: string) => boolean,
  realpathSync: (p: string) => string,
): string | null {
  const pathValue = platform === "win32" ? (env.PATH ?? env.Path ?? "") : (env.PATH ?? "");
  if (!pathValue) return null;
  const sep = platform === "win32" ? ";" : ":";
  const pathImpl = platform === "win32" ? path.win32 : path.posix;

  for (const rawDir of pathValue.split(sep)) {
    const dir = rawDir.trim().replace(/^"(.*)"$/, "$1");
    if (!dir) continue;
    const candidate = pathImpl.join(dir, binaryName);
    if (!isExecutableFile(candidate)) continue;
    const candidateReal = safeRealpath(candidate, realpathSync);
    if (selfRealpath && candidateReal === selfRealpath) {
      continue;
    }
    return candidate;
  }
  return null;
}

function formatMissingBinaryError(
  platform: string,
  arch: string,
  pkg: PlatformPackageInfo | null,
  isLinuxGlibc: () => boolean | null,
): string {
  if (!pkg) {
    const supported = PLATFORM_PACKAGES.map((p) => `${p.platform}-${p.arch}`).join(", ");
    return [
      `strange-quark: no prebuilt native binary is available for ${platform}-${arch}.`,
      `Supported targets: ${supported}.`,
      `Build squark and squark-runtime from source and set SQUARK_BIN=/path/to/squark or place squark on your PATH.`,
    ].join("\n");
  }

  const lines = [
    `strange-quark: could not find the native "squark" binary for ${platform}-${arch}.`,
    `Expected platform package: ${pkg.packageName}`,
    ``,
    `This usually happens when:`,
    `  - dependencies were installed with --omit=optional or --no-optional`,
    `  - node_modules was copied from a different OS or CPU architecture`,
    `  - your package manager skipped optionalDependencies`,
  ];

  if (platform === "linux" && isLinuxGlibc() === false) {
    lines.push(
      `  - this Linux system does not appear to use glibc (prebuilt Linux binaries require glibc)`,
    );
  }

  lines.push(
    ``,
    `How to fix:`,
    `  - Reinstall without --omit=optional / --no-optional, or install ${pkg.packageName} explicitly.`,
    `  - Or point SQUARK_BIN=/path/to/squark at a locally built binary (or add it to PATH).`,
  );

  return lines.join("\n");
}

export function resolveSquarkBinary(options: ResolveSquarkBinaryOptions = {}): ResolveSquarkBinaryResult {
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const env = options.env ?? process.env;
  const resolvePackage = options.resolvePackage ?? defaultResolvePackage;
  const existsSync = options.existsSync ?? fs.existsSync;
  const isExecutableFile = options.isExecutableFile ?? defaultIsExecutableFile;
  const realpathSync = options.realpathSync ?? fs.realpathSync;
  const isLinuxGlibc = options.isLinuxGlibc ?? defaultIsLinuxGlibc;
  const currentScriptPath =
    options.currentScriptPath ??
    (process.argv[1] ? path.resolve(process.argv[1]) : fileURLToPath(import.meta.url));
  const selfRealpath = safeRealpath(currentScriptPath, realpathSync);

  const pkg = getPlatformPackage(platform, arch);

  // 1. Platform package via require.resolve / import.meta.resolve.
  if (pkg) {
    const binSpecifier = `${pkg.packageName}/${pkg.binRelativePath}`;
    try {
      const resolved = resolvePackage(binSpecifier);
      if (existsSync(resolved)) {
        return {
          binaryPath: resolved,
          source: "platform-package",
          packageName: pkg.packageName,
        };
      }
    } catch {
      // Fall through to package.json resolution or fallbacks.
    }

    try {
      const pkgJsonPath = resolvePackage(`${pkg.packageName}/package.json`);
      const pathImpl = platform === "win32" ? path.win32 : path.posix;
      const candidate = pathImpl.join(pathImpl.dirname(pkgJsonPath), pkg.binRelativePath);
      if (existsSync(candidate)) {
        return {
          binaryPath: candidate,
          source: "platform-package",
          packageName: pkg.packageName,
        };
      }
    } catch {
      // Package not installed; continue to $SQUARK_BIN and PATH.
    }
  }

  // 2. Explicit $SQUARK_BIN override / fallback.
  const envBin = env.SQUARK_BIN?.trim();
  if (envBin) {
    const hasPathSep = envBin.includes("/") || envBin.includes("\\");
    if (existsSync(envBin)) {
      return { binaryPath: envBin, source: "env" };
    }
    if (hasPathSep) {
      throw new SquarkBinaryNotFoundError(
        `strange-quark: SQUARK_BIN is set to "${envBin}", but that file does not exist.`,
        { platform, arch, expectedPackage: pkg?.packageName ?? null },
      );
    }
    const foundEnv = findOnPath(envBin, platform, env, selfRealpath, isExecutableFile, realpathSync);
    if (foundEnv) {
      return { binaryPath: foundEnv, source: "env" };
    }
    throw new SquarkBinaryNotFoundError(
      `strange-quark: SQUARK_BIN is set to "${envBin}", which was not found on PATH.`,
      { platform, arch, expectedPackage: pkg?.packageName ?? null },
    );
  }

  // 3. `squark` (or `squark.exe` on Windows) on PATH, skipping this shim to prevent recursion.
  const pathBinaryName = platform === "win32" ? "squark.exe" : "squark";
  const foundOnPath = findOnPath(
    pathBinaryName,
    platform,
    env,
    selfRealpath,
    isExecutableFile,
    realpathSync,
  );
  if (foundOnPath) {
    return { binaryPath: foundOnPath, source: "path" };
  }

  throw new SquarkBinaryNotFoundError(formatMissingBinaryError(platform, arch, pkg, isLinuxGlibc), {
    platform,
    arch,
    expectedPackage: pkg?.packageName ?? null,
  });
}
