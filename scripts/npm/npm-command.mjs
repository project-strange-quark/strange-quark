// SPDX-License-Identifier: MIT
// The command line that runs npm or npx. On Windows they are .cmd shims,
// which spawn and spawnSync refuse without a shell since Node's fix for
// CVE-2024-27980 (EINVAL), so their JavaScript entry points run with this
// Node instead. Elsewhere the commands run by name.

import fs from "node:fs";
import path from "node:path";

/**
 * Returns `{ command, args }` for running `tool` with `args`. Only npm and npx
 * are rewritten; any other tool runs by name.
 */
export function npmCommand(tool, args, { platform = process.platform, env = process.env, execPath = process.execPath } = {}) {
  if (platform !== "win32" || (tool !== "npm" && tool !== "npx")) return { command: tool, args };
  return { command: execPath, args: [cliScript(tool, env, execPath), ...args] };
}

function cliScript(tool, env, execPath) {
  const name = `${tool}-cli.js`;
  const candidates = [];
  // npm sets npm_execpath to its own npm-cli.js for the scripts it runs.
  if (env.npm_execpath) candidates.push(path.join(path.dirname(env.npm_execpath), name));
  // Node's Windows installs ship npm beside node.exe.
  candidates.push(path.join(path.dirname(execPath), "node_modules", "npm", "bin", name));
  const found = candidates.find((candidate) => fs.existsSync(candidate));
  if (!found) throw new Error(`cannot find ${name}; looked in ${candidates.join(", ")}`);
  return found;
}
