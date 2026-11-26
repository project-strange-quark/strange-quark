// SPDX-License-Identifier: MIT

const { resolve } = require("node:path");
const root = resolve(process.env.SQUARK_REACT_SOURCE ?? process.cwd());
const sourceConfig = require(resolve(root, "scripts/jest/config.source.js"));
const { namePatternConfig } = require("./name-pattern.cjs");

module.exports = {
  ...sourceConfig,
  rootDir: root,
  testEnvironment: require.resolve("./environment.cjs"),
  testEnvironmentOptions: { customExportConditions: ["browser"] },
  ...namePatternConfig(),
};
