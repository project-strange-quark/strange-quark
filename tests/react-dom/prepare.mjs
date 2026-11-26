// SPDX-License-Identifier: MIT
// Ensures the unmodified React v19.0.0 source checkout and its pinned Jest
// dependencies exist under target/react-19.0.0 before running tests/react-dom.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { npmCommand } from "../../scripts/npm/npm-command.mjs";

const here = new URL("./", import.meta.url);
const root = fileURLToPath(new URL("../../", here));
const UPSTREAM_TAG = "v19.0.0";
const UPSTREAM_COMMIT = "7aa5dda3b3e4c2baa905a59b922ae7ec14734b24";

function run(tool, toolArgs, opts = {}) {
  // npx is a .cmd shim on Windows; npmCommand runs its script with Node there.
  const { command: cmd, args } = npmCommand(tool, toolArgs);
  const res = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (res.error) throw res.error;
  if (res.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} exited with status ${res.status}`);
  }
}

function isPrepared(dir) {
  if (!existsSync(resolve(dir, "node_modules/jest-cli/bin/jest.js"))) return false;
  const rev = spawnSync("git", ["rev-parse", "HEAD"], { cwd: dir, encoding: "utf8" });
  return rev.status === 0 && rev.stdout.trim() === UPSTREAM_COMMIT;
}

/**
 * The `git clone` arguments for the pinned React checkout. On Windows the
 * clone turns on long paths, since React's compiler fixtures pass the
 * 260-character MAX_PATH under target/, and turns off CRLF conversion, so
 * the sources match upstream byte for byte whatever core.autocrlf says.
 * Other platforms use a plain clone.
 */
export function cloneArgs(source, platform = process.platform) {
  const windowsConfig =
    platform === "win32" ? ["-c", "core.longpaths=true", "-c", "core.autocrlf=false"] : [];
  return [
    "clone",
    ...windowsConfig,
    "--depth",
    "1",
    "--branch",
    UPSTREAM_TAG,
    "https://github.com/facebook/react.git",
    source,
  ];
}

export function ensureReactSource() {
  // Respect an explicit caller-provided checkout without mutating it.
  if (process.env.SQUARK_REACT_SOURCE) {
    return resolve(process.env.SQUARK_REACT_SOURCE);
  }
  const source = resolve(root, "target/react-19.0.0");
  if (isPrepared(source)) {
    return source;
  }

  mkdirSync(dirname(source), { recursive: true });
  const rev = existsSync(source)
    ? spawnSync("git", ["rev-parse", "HEAD"], { cwd: source, encoding: "utf8" })
    : { status: 1, stdout: "" };
  if (rev.status !== 0 || rev.stdout.trim() !== UPSTREAM_COMMIT) {
    if (existsSync(source)) {
      rmSync(source, { recursive: true, force: true });
    }
    console.log(`Cloning unmodified React ${UPSTREAM_TAG} into ${source}...`);
    run("git", cloneArgs(source));
  }

  if (!existsSync(resolve(source, "node_modules/jest-cli/bin/jest.js"))) {
    console.log(`Installing pinned React ${UPSTREAM_TAG} test dependencies via Yarn 1...`);
    run(
      "npx",
      [
        "--yes",
        "yarn@1.22.22",
        "--cwd",
        source,
        "install",
        "--frozen-lockfile",
        "--ignore-scripts",
        "--ignore-engines",
        "--non-interactive",
      ],
      { cwd: root },
    );
  }

  return source;
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  ensureReactSource();
}
