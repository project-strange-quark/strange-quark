// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import test from "node:test";

import { cleanChildEnv } from "../../../scripts/npm/smoke-test.mjs";

test("cleanChildEnv drops every PATH and squark override spelling before setting PATH", () => {
  const input = {
    Path: "C:\\host-path",
    PATH: "C:\\host-upper",
    SQUARK_BIN: "C:\\squark.exe",
    squark_runtime_bin: "C:\\squark-runtime.exe",
    SystemRoot: "C:\\Windows",
    HOME: "/home/test",
  };

  const clean = cleanChildEnv(input, "C:\\clean;C:\\node");

  assert.equal(clean.PATH, "C:\\clean;C:\\node");
  assert.equal(clean.Path, undefined);
  assert.equal(clean.SQUARK_BIN, undefined);
  assert.equal(clean.squark_runtime_bin, undefined);
  assert.equal(clean.SystemRoot, "C:\\Windows");
  assert.equal(clean.HOME, "/home/test");
  assert.deepEqual(
    Object.keys(clean).filter((key) => /^(path|squark_bin|squark_runtime_bin)$/i.test(key)),
    ["PATH"],
  );
  assert.equal(input.Path, "C:\\host-path", "the input environment is not mutated");
});
