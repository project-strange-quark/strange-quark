// SPDX-License-Identifier: MIT

import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = new URL("./", import.meta.url);
const { PATTERN_FILE_ENV, testNamePattern } = createRequire(import.meta.url)("./name-pattern.cjs");
const root = fileURLToPath(new URL("../../", here));
const upstream = "7aa5dda3b3e4c2baa905a59b922ae7ec14734b24";
const suites = [
  "Root", "Attribute", "Component", "TextComponent", "EventListener",
  "EventPropagation", "NestedEvents", "Input", "Select", "Textarea",
].map(name => `packages/react-dom/src/__tests__/ReactDOM${name}-test.js`);
const { values } = parseArgs({ options: {
  source: { type: "string" },
  baseline: { type: "boolean" },
  all: { type: "boolean" },
  report: { type: "string" },
} });

const source = resolve(values.source ?? process.env.SQUARK_REACT_SOURCE ?? resolve(root, "target/react-19.0.0"));
const commit = spawnSync("git", ["rev-parse", "HEAD"], { cwd: source, encoding: "utf8" });
if (commit.status !== 0 || commit.stdout.trim() !== upstream) {
  throw new Error(`Expected an unmodified React v19.0.0 checkout at ${source} (${upstream}). See tests/react-dom/README.md.`);
}
const changes = spawnSync("git", ["status", "--porcelain", "--untracked-files=no"], { cwd: source, encoding: "utf8" });
if (changes.status !== 0 || changes.stdout.trim() !== "") throw new Error("The tracked React sources must be unmodified.");
if (!statSync(resolve(source, "node_modules/jest-cli/bin/jest.js"), { throwIfNoEntry: false })) {
  throw new Error(`Install the pinned React test dependencies in ${source}. See tests/react-dom/README.md.`);
}
const args = [
  // Ten suites in one Jest process outgrow Node's default heap on 2-vCPU CI
  // runners (about 2 GiB there), so give this process an explicit ceiling.
  "--max-old-space-size=6144",
  "--experimental-vm-modules", "scripts/jest/jest.js", "--runInBand",
  "--config", values.baseline ? "scripts/jest/config.source.js" : fileURLToPath(new URL("jest.config.cjs", here)),
  "--runTestsByPath", ...suites,
];
let reportPath = values.report === undefined ? undefined : resolve(values.report);
let passing;
const env = { ...process.env, SQUARK_REACT_SOURCE: source, NODE_ENV: "development", RELEASE_CHANNEL: "stable" };
delete env[PATTERN_FILE_ENV];
if (!values.baseline && !values.all) {
  const inventory = JSON.parse(readFileSync(new URL("cases.json", here), "utf8"));
  passing = inventory.suites;
  // Too long for a Windows command line; jest.config.cjs reads it from here.
  const patternPath = resolve(root, "target/react-dom-test-names.txt");
  mkdirSync(dirname(patternPath), { recursive: true });
  writeFileSync(patternPath, testNamePattern(inventory));
  env[PATTERN_FILE_ENV] = patternPath;
  reportPath ??= resolve(root, "target/react-dom-passing.json");
}
if (reportPath !== undefined) {
  mkdirSync(dirname(reportPath), { recursive: true });
  args.push("--json", "--outputFile", reportPath);
}
const result = spawnSync(process.execPath, args, {
  cwd: source,
  stdio: "inherit",
  env,
});
if (result.error) throw result.error;
if (result.status !== 0) {
  process.exitCode = result.status ?? 1;
} else if (passing !== undefined) {
  const report = JSON.parse(readFileSync(reportPath, "utf8"));
  for (const [file, { passing: expected }] of Object.entries(passing)) {
    const suite = report.testResults.find(suite => basename(suite.name) === file);
    const executed = suite?.assertionResults.filter(test => test.status === "passed").map(test => test.fullName);
    if (JSON.stringify(executed?.sort()) !== JSON.stringify([...expected].sort())) {
      throw new Error(`The passing-case run did not match the inventory for ${file}.`);
    }
  }
}
