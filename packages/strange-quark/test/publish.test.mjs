// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { PLATFORM_TARGETS } from "../../../scripts/npm/platforms.mjs";
import { distTag, isPublished, publishAll } from "../../../scripts/npm/publish.mjs";

function stage(version) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-npm-publish-"));
  const packages = [
    ...PLATFORM_TARGETS.map((target) => [`cli-${target.id}`, target.packageName]),
    ["vite-plugin", "@strange-quark/vite-plugin"],
    ["strange-quark", "strange-quark"],
  ];
  for (const [id, name] of packages) {
    fs.mkdirSync(path.join(dir, id));
    fs.writeFileSync(path.join(dir, id, "package.json"), JSON.stringify({ name, version }));
  }
  return dir;
}

/** A fake npm: `published` names answer `npm view`, everything else is a 404. */
function fakeNpm(published) {
  const calls = [];
  const run = (args, options = {}) => {
    calls.push({ args, cwd: options.cwd ? path.basename(options.cwd) : null });
    if (args[0] === "view") {
      const name = args[1].slice(0, args[1].lastIndexOf("@"));
      return published.has(name)
        ? { status: 0, stdout: '"0.1.0"\n', stderr: "" }
        : { status: 1, stdout: "", stderr: "npm error code E404\nnpm error 404 Not Found" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { run, calls };
}

test("publishAll publishes platform packages first and skips versions npm already has", () => {
  const stageDir = stage("0.1.0");
  try {
    const npm = fakeNpm(new Set(["@strange-quark/cli-darwin-arm64"]));
    const results = publishAll({ stageDir, run: npm.run, log: () => {} });

    const publishes = npm.calls.filter((call) => call.args[0] === "publish");
    assert.deepEqual(
      publishes.map((call) => call.cwd),
      [
        ...PLATFORM_TARGETS.filter((t) => t.id !== "darwin-arm64").map((t) => `cli-${t.id}`),
        "vite-plugin",
        "strange-quark",
      ],
    );
    for (const call of publishes) {
      assert.deepEqual(call.args, ["publish", "--access", "public", "--tag", "latest", "--provenance"]);
    }
    assert.deepEqual(results[0], {
      name: "@strange-quark/cli-darwin-arm64",
      version: "0.1.0",
      action: "skipped",
    });
    assert.equal(results.filter((r) => r.action === "published").length, 7);
  } finally {
    fs.rmSync(stageDir, { recursive: true, force: true });
  }
});

test("a prerelease goes to the next dist-tag, and a dry run passes --dry-run", () => {
  assert.equal(distTag("0.2.0-rc.1"), "next");
  assert.equal(distTag("0.2.0"), "latest");
  const stageDir = stage("0.2.0-rc.1");
  try {
    const npm = fakeNpm(new Set());
    publishAll({ stageDir, run: npm.run, log: () => {}, dryRun: true, provenance: false });
    const publish = npm.calls.find((call) => call.args[0] === "publish");
    assert.deepEqual(publish.args, ["publish", "--access", "public", "--tag", "next", "--dry-run"]);
  } finally {
    fs.rmSync(stageDir, { recursive: true, force: true });
  }
});

test("an npm view failure other than a 404 stops the release", () => {
  const run = () => ({ status: 1, stdout: "", stderr: "npm error code E401\nnpm error 401 Unauthorized" });
  assert.throws(() => isPublished("strange-quark", "0.1.0", run), /npm view strange-quark@0\.1\.0 failed/);
});

test("a missing staged package stops the release before anything is published", () => {
  const stageDir = stage("0.1.0");
  try {
    fs.rmSync(path.join(stageDir, "vite-plugin"), { recursive: true });
    const npm = fakeNpm(new Set());
    assert.throws(() => publishAll({ stageDir, run: npm.run, log: () => {} }), /missing staged package/);
    assert.deepEqual(npm.calls, []);
  } finally {
    fs.rmSync(stageDir, { recursive: true, force: true });
  }
});
