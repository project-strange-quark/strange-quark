// SPDX-License-Identifier: MIT
// The release workflow's artifact placement (scripts/npm/collect-artifacts.mjs).

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { collectArtifacts } from "../../../scripts/npm/collect-artifacts.mjs";

function scratch() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "squark-collect-"));
}

test("places each platform's binaries under target/<triple>/dist and makes Unix ones executable", () => {
  const root = scratch();
  try {
    const artifacts = path.join(root, "artifacts");
    for (const [id, files] of [
      ["darwin-arm64", ["squark", "squark-runtime"]],
      ["win32-x64", ["squark.exe", "squark-runtime.exe"]],
    ]) {
      fs.mkdirSync(path.join(artifacts, `bin-${id}`), { recursive: true });
      for (const file of files) fs.writeFileSync(path.join(artifacts, `bin-${id}`, file), file);
    }
    const target = path.join(root, "target");
    const placed = collectArtifacts({ artifactsDir: artifacts, targetDir: target });
    assert.deepEqual(placed.map((entry) => entry.id), ["darwin-arm64", "win32-x64"]);

    const runtime = path.join(target, "aarch64-apple-darwin", "dist", "squark-runtime");
    assert.equal(fs.readFileSync(runtime, "utf8"), "squark-runtime");
    if (process.platform !== "win32") {
      assert.equal(fs.statSync(runtime).mode & 0o111, 0o111);
    }
    assert.equal(
      fs.readFileSync(path.join(target, "x86_64-pc-windows-msvc", "dist", "squark.exe"), "utf8"),
      "squark.exe",
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("an artifact missing one of its binaries is an error", () => {
  const root = scratch();
  try {
    const artifacts = path.join(root, "artifacts");
    fs.mkdirSync(path.join(artifacts, "bin-linux-x64"), { recursive: true });
    fs.writeFileSync(path.join(artifacts, "bin-linux-x64", "squark"), "squark");
    assert.throws(
      () => collectArtifacts({ artifactsDir: artifacts, targetDir: path.join(root, "target") }),
      /bin-linux-x64 is missing squark-runtime/,
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
