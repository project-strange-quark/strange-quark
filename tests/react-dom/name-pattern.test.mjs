// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const require = createRequire(import.meta.url);
const { PATTERN_FILE_ENV, testNamePattern, namePatternConfig } = require("./name-pattern.cjs");

// Windows rejects longer command lines with ENAMETOOLONG.
const WINDOWS_COMMAND_LINE_LIMIT = 32_767;

test("the pattern matches exactly the inventory's names, special characters included", () => {
  const pattern = new RegExp(testNamePattern({
    suites: {
      "a-test.js": { passing: ["a renders (x) [y]", "a + b"] },
      "b-test.js": { passing: ["a + b", "c.d?"] },
    },
  }));
  for (const name of ["a renders (x) [y]", "a + b", "c.d?"]) assert.ok(pattern.test(name), name);
  for (const name of ["a renders x y", "a  b", "cxd", "a + b and more"]) assert.ok(!pattern.test(name), name);
});

test("an empty inventory is an error", () => {
  assert.throws(() => testNamePattern({ suites: { "a-test.js": { passing: [] } } }), /empty/);
});

test("the real inventory's pattern does not fit a windows command line, so it goes through a file", () => {
  const inventory = JSON.parse(readFileSync(new URL("cases.json", import.meta.url), "utf8"));
  const pattern = testNamePattern(inventory);
  assert.ok(pattern.length > WINDOWS_COMMAND_LINE_LIMIT, `pattern is ${pattern.length} characters`);

  const dir = mkdtempSync(join(tmpdir(), "squark-react-pattern-"));
  try {
    const file = join(dir, "pattern.txt");
    writeFileSync(file, pattern);
    assert.deepEqual(namePatternConfig({ [PATTERN_FILE_ENV]: file }), { testNamePattern: pattern });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  assert.deepEqual(namePatternConfig({}), {});
});
