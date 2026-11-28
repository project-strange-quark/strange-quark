// SPDX-License-Identifier: MIT
// Unit tests for scripts/npm/set-version.mjs, stage.mjs, and verify.mjs.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { PLATFORM_TARGETS, REPO_ROOT } from "../../../scripts/npm/platforms.mjs";
import { syncVersions } from "../../../scripts/npm/set-version.mjs";
import { stagePackages } from "../../../scripts/npm/stage.mjs";
import { verifyPackages } from "../../../scripts/npm/verify.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
assert.equal(path.resolve(__dirname, "../../.."), REPO_ROOT);

function writeSyntheticBinaries(targetDir, targets, { useHostDist = false } = {}) {
  for (const target of targets) {
    const dir = useHostDist
      ? path.join(targetDir, "dist")
      : path.join(targetDir, target.rustTriple, "dist");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, `squark${target.exeSuffix}`), `synthetic-squark-${target.id}`);
    fs.writeFileSync(
      path.join(dir, `squark-runtime${target.exeSuffix}`),
      `synthetic-runtime-${target.id}`,
    );
  }
}

test("syncVersions --check passes on the repository manifests, workspace pins, and LICENSE copies", () => {
  const result = syncVersions({ repoRoot: REPO_ROOT, check: true });
  assert.equal(result.packages, 8);
  assert.equal(result.version, "0.1.0");
  assert.ok(result.workspaceRefsChecked >= 1, "expected tests/dev workspace ref to be checked");
});

test("syncVersions keeps internal workspace dependency pins in sync and catches drift in --check", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-sync-ws-"));
  try {
    fs.copyFileSync(path.join(REPO_ROOT, "LICENSE"), path.join(tmp, "LICENSE"));
    const dirsToCopy = [
      "packages/strange-quark",
      "packages/@strange-quark/vite-plugin",
      "tests/dev",
      ...PLATFORM_TARGETS.map((t) => t.dir),
    ];
    for (const rel of dirsToCopy) {
      const dstDir = path.join(tmp, rel);
      fs.mkdirSync(dstDir, { recursive: true });
      fs.copyFileSync(path.join(REPO_ROOT, rel, "package.json"), path.join(dstDir, "package.json"));
      const lic = path.join(REPO_ROOT, rel, "LICENSE");
      if (fs.existsSync(lic)) {
        fs.copyFileSync(lic, path.join(dstDir, "LICENSE"));
      }
    }

    // The Rust crates follow the same version: the workspace version, internal
    // version requirements and Cargo.lock.
    fs.copyFileSync(path.join(REPO_ROOT, "Cargo.toml"), path.join(tmp, "Cargo.toml"));
    fs.copyFileSync(path.join(REPO_ROOT, "Cargo.lock"), path.join(tmp, "Cargo.lock"));
    for (const crate of ["squark-layout", "squark-style"]) {
      fs.mkdirSync(path.join(tmp, "crates", crate), { recursive: true });
      fs.copyFileSync(
        path.join(REPO_ROOT, "crates", crate, "Cargo.toml"),
        path.join(tmp, "crates", crate, "Cargo.toml"),
      );
    }

    // Drift tests/dev/package.json back to 0.0.0 and confirm --check catches it.
    const devPkgPath = path.join(tmp, "tests/dev/package.json");
    const devPkg = JSON.parse(fs.readFileSync(devPkgPath, "utf8"));
    devPkg.dependencies["@strange-quark/vite-plugin"] = "0.0.0";
    fs.writeFileSync(devPkgPath, `${JSON.stringify(devPkg, null, 2)}\n`);

    assert.throws(
      () => syncVersions({ repoRoot: tmp, check: true }),
      /tests\/dev\/package\.json dependencies\["@strange-quark\/vite-plugin"\] is "0\.0\.0", expected "0\.1\.0"/,
    );

    // Syncing to a new version updates all 8 packages AND tests/dev/package.json.
    const updated = syncVersions({ repoRoot: tmp, version: "0.2.0", check: false });
    assert.equal(updated.version, "0.2.0");
    assert.ok(updated.updated.includes("tests/dev/package.json"));
    const syncedDevPkg = JSON.parse(fs.readFileSync(devPkgPath, "utf8"));
    assert.equal(syncedDevPkg.dependencies["@strange-quark/vite-plugin"], "0.2.0");
    const cargoToml = fs.readFileSync(path.join(tmp, "Cargo.toml"), "utf8");
    assert.match(cargoToml, /\[workspace\.package\]\nversion = "0\.2\.0"/);
    assert.match(cargoToml, /squark-journal = \{ path = "crates\/squark-journal", version = "0\.2\.0" \}/);
    const layout = fs.readFileSync(path.join(tmp, "crates/squark-layout/Cargo.toml"), "utf8");
    assert.match(layout, /squark-assets = \{ path = "\.\.\/squark-assets", version = "=0\.2\.0" \}/);
    assert.doesNotMatch(layout, /version = "=?0\.1\.0"/);
    const lock = fs.readFileSync(path.join(tmp, "Cargo.lock"), "utf8");
    assert.match(lock, /name = "squark-host"\nversion = "0\.2\.0"/);
    assert.doesNotMatch(lock, /name = "squark-[a-z-]+"\nversion = "0\.1\.0"/);

    const recheck = syncVersions({ repoRoot: tmp, check: true });
    assert.equal(recheck.version, "0.2.0");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("syncVersions handles CRLF Cargo.lock and Cargo.toml and preserves their line endings", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-sync-crlf-"));
  try {
    fs.copyFileSync(path.join(REPO_ROOT, "LICENSE"), path.join(tmp, "LICENSE"));
    const dirsToCopy = [
      "packages/strange-quark",
      "packages/@strange-quark/vite-plugin",
      ...PLATFORM_TARGETS.map((t) => t.dir),
    ];
    for (const rel of dirsToCopy) {
      const dstDir = path.join(tmp, rel);
      fs.mkdirSync(dstDir, { recursive: true });
      fs.copyFileSync(path.join(REPO_ROOT, rel, "package.json"), path.join(dstDir, "package.json"));
      const lic = path.join(REPO_ROOT, rel, "LICENSE");
      if (fs.existsSync(lic)) {
        fs.copyFileSync(lic, path.join(dstDir, "LICENSE"));
      }
    }

    const toCrlf = (text) => text.replace(/\r?\n/g, "\r\n");
    const cargoTomlPath = path.join(tmp, "Cargo.toml");
    const cargoLockPath = path.join(tmp, "Cargo.lock");
    fs.writeFileSync(cargoTomlPath, toCrlf(fs.readFileSync(path.join(REPO_ROOT, "Cargo.toml"), "utf8")));
    fs.writeFileSync(cargoLockPath, toCrlf(fs.readFileSync(path.join(REPO_ROOT, "Cargo.lock"), "utf8")));

    // Matching CRLF Cargo.lock passes --check.
    const initial = syncVersions({ repoRoot: tmp, check: true });
    assert.equal(initial.version, "0.1.0");
    assert.ok(initial.cratesChecked > 0);

    // Drift one Cargo.lock entry and verify --check detects it even with CRLF.
    const driftedLock = fs
      .readFileSync(cargoLockPath, "utf8")
      .replace(
        /\[\[package\]\]\r\nname = "squark-host"\r\nversion = "0\.1\.0"/,
        '[[package]]\r\nname = "squark-host"\r\nversion = "0.0.0"',
      );
    fs.writeFileSync(cargoLockPath, driftedLock);
    assert.throws(
      () => syncVersions({ repoRoot: tmp, check: true }),
      /Cargo\.lock Cargo\.lock entry is "0\.0\.0", expected "0\.1\.0"/,
    );

    // Syncing to 0.2.0 updates CRLF Cargo.lock and Cargo.toml while keeping CRLF endings intact.
    const updated = syncVersions({ repoRoot: tmp, version: "0.2.0", check: false });
    assert.ok(updated.updated.includes("Cargo.lock"));
    assert.ok(updated.updated.includes("Cargo.toml"));

    const lockAfter = fs.readFileSync(cargoLockPath, "utf8");
    assert.match(lockAfter, /\[\[package\]\]\r\nname = "squark-host"\r\nversion = "0\.2\.0"/);
    assert.doesNotMatch(lockAfter, /name = "squark-[a-z-]+"\r?\nversion = "0\.1\.0"/);
    assert.doesNotMatch(lockAfter, /(?<!\r)\n/, "Cargo.lock must keep CRLF line endings");

    const tomlAfter = fs.readFileSync(cargoTomlPath, "utf8");
    assert.match(tomlAfter, /\[workspace\.package\]\r\nversion = "0\.2\.0"/);
    assert.doesNotMatch(tomlAfter, /(?<!\r)\n/, "Cargo.toml must keep CRLF line endings");

    assert.equal(syncVersions({ repoRoot: tmp, check: true }).version, "0.2.0");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("stagePackages and verifyPackages succeed for all 6 synthetic targets in an isolated stageDir", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-stage-all-"));
  try {
    const fakeTarget = path.join(tmp, "target");
    const stageDir = path.join(tmp, "stage");
    const manifestPath = path.join(tmp, "pack-manifest.json");

    writeSyntheticBinaries(fakeTarget, PLATFORM_TARGETS);

    const manifest = stagePackages({
      repoRoot: REPO_ROOT,
      targetDir: fakeTarget,
      stageDir,
      manifestPath,
      version: "0.2.0-rc.1",
      requireAll: true,
    });

    assert.equal(manifest.version, "0.2.0-rc.1");
    assert.equal(manifest.packages.length, 8);
    assert.ok(manifest.packages.every((p) => p.staged));
    assert.ok(fs.existsSync(manifestPath));

    // Confirm stagePackages injects exact-version optionalDependencies into Strange Quark.
    const stagedMainPkg = JSON.parse(
      fs.readFileSync(path.join(stageDir, "strange-quark/package.json"), "utf8"),
    );
    for (const target of PLATFORM_TARGETS) {
      assert.equal(stagedMainPkg.optionalDependencies[target.packageName], "0.2.0-rc.1");
    }

    // Confirm POSIX executable bit on Unix binaries.
    if (process.platform !== "win32") {
      const darwinSquark = path.join(stageDir, "cli-darwin-arm64/bin/squark");
      const mode = fs.statSync(darwinSquark).mode & 0o111;
      assert.notEqual(mode, 0, "darwin squark binary should be executable");
    }

    // Confirm verifyPackages runs npm pack --dry-run --json on every package and passes.
    const verified = verifyPackages({ repoRoot: REPO_ROOT, manifestPath, requireAll: true });
    assert.equal(verified.version, "0.2.0-rc.1");
    assert.equal(verified.checked.length, 8);
    assert.equal(verified.skipped.length, 0);

    // Inject an unexpected file into one staged package and verify it is caught.
    fs.writeFileSync(path.join(stageDir, "cli-linux-x64/bin/stray.txt"), "oops");
    assert.throws(
      () => verifyPackages({ repoRoot: REPO_ROOT, manifestPath, requireAll: true }),
      /unexpected file\(s\) in tarball: bin\/stray\.txt/,
    );
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("stagePackages falls back to target/dist/ for the host target and rejects missing --require-all", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-stage-host-"));
  try {
    const fakeTarget = path.join(tmp, "target");
    const stageDir = path.join(tmp, "stage");
    const manifestPath = path.join(tmp, "pack-manifest.json");

    const host = PLATFORM_TARGETS.find((t) => t.id === "darwin-arm64");
    writeSyntheticBinaries(fakeTarget, [host], { useHostDist: true });

    const manifest = stagePackages({
      repoRoot: REPO_ROOT,
      targetDir: fakeTarget,
      stageDir,
      manifestPath,
      hostPlatform: "darwin",
      hostArch: "arm64",
      requireAll: false,
    });

    const darwinEntry = manifest.packages.find((p) => p.name === "@strange-quark/cli-darwin-arm64");
    const linuxEntry = manifest.packages.find((p) => p.name === "@strange-quark/cli-linux-x64");
    assert.equal(darwinEntry?.staged, true);
    assert.equal(linuxEntry?.staged, false);

    assert.throws(
      () =>
        stagePackages({
          repoRoot: REPO_ROOT,
          targetDir: fakeTarget,
          stageDir,
          manifestPath,
          hostPlatform: "darwin",
          hostArch: "arm64",
          requireAll: true,
        }),
      /Missing dist binaries for platform\(s\)/,
    );
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
