// SPDX-License-Identifier: MIT
// Unit and process-level tests for the `squark` CLI shim and binary resolution.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  SquarkBinaryNotFoundError,
  PLATFORM_PACKAGES,
  getPlatformPackage,
  resolveSquarkBinary,
} from "../dist/resolve-bin.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const cliEntry = path.resolve(__dirname, "../dist/cli.js");

test("getPlatformPackage maps all six supported os+cpu targets", () => {
  const expected = [
    ["darwin", "arm64", "@strange-quark/cli-darwin-arm64", "bin/squark"],
    ["darwin", "x64", "@strange-quark/cli-darwin-x64", "bin/squark"],
    ["linux", "x64", "@strange-quark/cli-linux-x64", "bin/squark"],
    ["linux", "arm64", "@strange-quark/cli-linux-arm64", "bin/squark"],
    ["win32", "x64", "@strange-quark/cli-win32-x64", "bin/squark.exe"],
    ["win32", "arm64", "@strange-quark/cli-win32-arm64", "bin/squark.exe"],
  ];
  assert.equal(PLATFORM_PACKAGES.length, expected.length);
  for (const [platform, arch, packageName, binRelativePath] of expected) {
    const pkg = getPlatformPackage(platform, arch);
    assert.ok(pkg, `missing target for ${platform}-${arch}`);
    assert.equal(pkg.packageName, packageName);
    assert.equal(pkg.binRelativePath, binRelativePath);
  }
  assert.equal(getPlatformPackage("freebsd", "x64"), null);
});

test("resolveSquarkBinary prefers platform package over SQUARK_BIN and PATH", () => {
  const resolved = resolveSquarkBinary({
    platform: "darwin",
    arch: "arm64",
    env: { SQUARK_BIN: "/custom/squark", PATH: "/usr/local/bin" },
    resolvePackage: (specifier) => {
      assert.equal(specifier, "@strange-quark/cli-darwin-arm64/bin/squark");
      return "/node_modules/@strange-quark/cli-darwin-arm64/bin/squark";
    },
    existsSync: (p) => p === "/node_modules/@strange-quark/cli-darwin-arm64/bin/squark",
    isExecutableFile: () => true,
    realpathSync: (p) => p,
  });
  assert.deepEqual(resolved, {
    binaryPath: "/node_modules/@strange-quark/cli-darwin-arm64/bin/squark",
    source: "platform-package",
    packageName: "@strange-quark/cli-darwin-arm64",
  });
});

test("resolveSquarkBinary falls back to package.json resolution for Windows .exe", () => {
  const resolved = resolveSquarkBinary({
    platform: "win32",
    arch: "x64",
    env: {},
    resolvePackage: (specifier) => {
      if (specifier === "@strange-quark/cli-win32-x64/package.json") {
        return "C:\\app\\node_modules\\@strange-quark\\cli-win32-x64\\package.json";
      }
      throw new Error("not directly exported");
    },
    existsSync: (p) => p === "C:\\app\\node_modules\\@strange-quark\\cli-win32-x64\\bin\\squark.exe",
    isExecutableFile: () => false,
    realpathSync: (p) => p,
  });
  assert.deepEqual(resolved, {
    binaryPath: "C:\\app\\node_modules\\@strange-quark\\cli-win32-x64\\bin\\squark.exe",
    source: "platform-package",
    packageName: "@strange-quark/cli-win32-x64",
  });
});

test("resolveSquarkBinary falls back to SQUARK_BIN when platform package is absent", () => {
  const resolved = resolveSquarkBinary({
    platform: "linux",
    arch: "x64",
    env: { SQUARK_BIN: "/opt/squark/bin/squark", PATH: "/usr/bin" },
    resolvePackage: () => {
      throw new Error("MODULE_NOT_FOUND");
    },
    existsSync: (p) => p === "/opt/squark/bin/squark",
    isExecutableFile: () => false,
    realpathSync: (p) => p,
  });
  assert.deepEqual(resolved, {
    binaryPath: "/opt/squark/bin/squark",
    source: "env",
  });

  assert.throws(
    () =>
      resolveSquarkBinary({
        platform: "linux",
        arch: "x64",
        env: { SQUARK_BIN: "/does/not/exist/squark" },
        resolvePackage: () => {
          throw new Error("MODULE_NOT_FOUND");
        },
        existsSync: () => false,
        isExecutableFile: () => false,
        realpathSync: (p) => p,
      }),
    /SQUARK_BIN is set to "\/does\/not\/exist\/squark", but that file does not exist/,
  );
});

test("resolveSquarkBinary falls back to PATH and skips self-symlink shim", () => {
  const resolved = resolveSquarkBinary({
    platform: "darwin",
    arch: "arm64",
    env: { PATH: "/proj/node_modules/.bin:/usr/local/bin" },
    currentScriptPath: "/proj/node_modules/strange-quark/dist/cli.js",
    resolvePackage: () => {
      throw new Error("MODULE_NOT_FOUND");
    },
    existsSync: () => false,
    isExecutableFile: (p) => p === "/proj/node_modules/.bin/squark" || p === "/usr/local/bin/squark",
    realpathSync: (p) =>
      p === "/proj/node_modules/.bin/squark" ? "/proj/node_modules/strange-quark/dist/cli.js" : p,
  });
  assert.deepEqual(resolved, {
    binaryPath: "/usr/local/bin/squark",
    source: "path",
  });
});

test("resolveSquarkBinary looks for squark.exe on Windows PATH", () => {
  const resolved = resolveSquarkBinary({
    platform: "win32",
    arch: "arm64",
    env: { Path: "C:\\tools\\squark;C:\\Windows\\System32" },
    currentScriptPath: "C:\\proj\\node_modules\\strange-quark\\dist\\cli.js",
    resolvePackage: () => {
      throw new Error("MODULE_NOT_FOUND");
    },
    existsSync: () => false,
    isExecutableFile: (p) => p === "C:\\tools\\squark\\squark.exe",
    realpathSync: (p) => p,
  });
  assert.deepEqual(resolved, {
    binaryPath: "C:\\tools\\squark\\squark.exe",
    source: "path",
  });
});

test("resolveSquarkBinary reports expected platform package and optional-dep hint when missing", () => {
  assert.throws(
    () =>
      resolveSquarkBinary({
        platform: "linux",
        arch: "x64",
        env: { PATH: "" },
        resolvePackage: () => {
          throw new Error("MODULE_NOT_FOUND");
        },
        existsSync: () => false,
        isExecutableFile: () => false,
        realpathSync: (p) => p,
        isLinuxGlibc: () => false,
      }),
    (err) => {
      assert.ok(err instanceof SquarkBinaryNotFoundError);
      assert.equal(err.expectedPackage, "@strange-quark/cli-linux-x64");
      assert.match(err.message, /@strange-quark\/cli-linux-x64/);
      assert.match(err.message, /--omit=optional/);
      assert.match(err.message, /--no-optional/);
      assert.match(err.message, /glibc/);
      return true;
    },
  );
});

test("resolveSquarkBinary explains unsupported platforms", () => {
  assert.throws(
    () =>
      resolveSquarkBinary({
        platform: "freebsd",
        arch: "x64",
        env: { PATH: "" },
        resolvePackage: () => {
          throw new Error("MODULE_NOT_FOUND");
        },
        existsSync: () => false,
        isExecutableFile: () => false,
        realpathSync: (p) => p,
      }),
    (err) => {
      assert.ok(err instanceof SquarkBinaryNotFoundError);
      assert.equal(err.expectedPackage, null);
      assert.match(err.message, /freebsd-x64/);
      assert.match(err.message, /darwin-arm64/);
      return true;
    },
  );
});

test("cli shim forwards arguments, exit codes, and signals from SQUARK_BIN", { skip: process.platform === "win32" }, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-cli-shim-"));
  try {
    const fakeBin = path.join(dir, "fake-squark");
    fs.writeFileSync(
      fakeBin,
      `#!/bin/sh
if [ "$1" = "--exit" ]; then
  printf "args:%s\\n" "$*"
  exit "$2"
fi
if [ "$1" = "--kill-self" ]; then
  kill -TERM $$
  sleep 2
fi
echo "unexpected"
exit 99
`,
      { mode: 0o755 },
    );

    const okRun = spawnSync(process.execPath, [cliEntry, "--exit", "0"], {
      env: { ...process.env, SQUARK_BIN: fakeBin },
      encoding: "utf8",
    });
    assert.equal(okRun.status, 0);
    assert.equal(okRun.signal, null);
    assert.match(okRun.stdout, /args:--exit 0/);

    const errRun = spawnSync(process.execPath, [cliEntry, "--exit", "42"], {
      env: { ...process.env, SQUARK_BIN: fakeBin },
      encoding: "utf8",
    });
    assert.equal(errRun.status, 42);
    assert.equal(errRun.signal, null);

    const sigRun = spawnSync(process.execPath, [cliEntry, "--kill-self"], {
      env: { ...process.env, SQUARK_BIN: fakeBin },
      encoding: "utf8",
    });
    assert.equal(sigRun.signal, "SIGTERM");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
