// SPDX-License-Identifier: MIT
// The passing-case filter for the React DOM suites. With 520 names its
// regular expression is about 56,000 characters, longer than a Windows
// command line may be (32,767), so run.mjs writes it to a file and the Jest
// config reads it from there instead of passing --testNamePattern.

const { readFileSync } = require("node:fs");

const PATTERN_FILE_ENV = "SQUARK_REACT_TEST_NAME_PATTERN_FILE";

/** An anchored pattern that matches exactly the inventory's passing names. */
function testNamePattern(inventory) {
  const names = [...new Set(Object.values(inventory.suites).flatMap((suite) => suite.passing))];
  if (names.length === 0) throw new Error("The passing-case inventory is empty.");
  return `^(?:${names.map(RegExp.escape).join("|")})$`;
}

/** The Jest config's testNamePattern entry, from the file named in `env`. */
function namePatternConfig(env = process.env) {
  const file = env[PATTERN_FILE_ENV];
  return file ? { testNamePattern: readFileSync(file, "utf8") } : {};
}

module.exports = { PATTERN_FILE_ENV, testNamePattern, namePatternConfig };
