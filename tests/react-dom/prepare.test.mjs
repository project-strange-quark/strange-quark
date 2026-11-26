// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import test from "node:test";

import { cloneArgs } from "./prepare.mjs";

const upstream = ["--depth", "1", "--branch", "v19.0.0", "https://github.com/facebook/react.git", "dest"];

test("the React clone is unchanged off Windows", () => {
  assert.deepEqual(cloneArgs("dest", "linux"), ["clone", ...upstream]);
  assert.deepEqual(cloneArgs("dest", "darwin"), ["clone", ...upstream]);
});

test("the React clone on Windows allows long paths and keeps LF endings", () => {
  assert.deepEqual(cloneArgs("dest", "win32"), [
    "clone",
    "-c",
    "core.longpaths=true",
    "-c",
    "core.autocrlf=false",
    ...upstream,
  ]);
});
