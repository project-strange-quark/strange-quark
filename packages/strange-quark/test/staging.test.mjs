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

    const recheck = syncVersions({ repoRoot: tmp, check: true });
    assert.equal(recheck.version, "0.2.0");
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
