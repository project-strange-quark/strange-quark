#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Portable replacement for GNU timeout (macOS and Windows have neither):
//
//   node scripts/with-timeout.mjs <seconds> <command> [args...]
//
// Runs the command without a shell (a Windows .cmd or .bat shim goes through
// cmd.exe with its arguments escaped), forwards stdio, and kills the whole
// process tree when the deadline passes: the process group on Unix, and
// `taskkill /T /F` on Windows. Like GNU timeout it exits 124 when the
// deadline is reached and otherwise with the child's exit status.

import { spawn, spawnSync } from "node:child_process";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_GRACE_MS = 5000;

/** Parses "180", "180s", "1.5m", "2h" or "1d" into milliseconds. */
export function parseDuration(text) {
  const match = /^([0-9]+(?:\.[0-9]+)?)([smhd]?)$/.exec(String(text).trim());
  if (!match) throw new Error(`invalid duration: ${text}`);
  const value = Number(match[1]);
  const multiplier = { "": 1, s: 1, m: 60, h: 3600, d: 86400 }[match[2]];
  const ms = value * multiplier * 1000;
  if (!(ms > 0)) throw new Error(`duration must be positive: ${text}`);
  return ms;
}

function killTree(child, signal) {
  if (!child.pid) return;
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/pid", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
    return;
  }
  try {
    // The child is detached, so it leads a process group of its own; a
    // negative pid signals every process in that group.
    process.kill(-child.pid, signal);
  } catch {
    // ESRCH: the group already exited.
  }
}

/**
 * Runs `command` with `args`, killing the process tree after `timeoutMs`.
 *
 * Resolves to `{ code, signal, timedOut }`, where `code` is the process exit
 * status (124 on timeout) and mirrors GNU timeout's signal convention.
 */
export function runWithTimeout(command, args, timeoutMs, options = {}) {
  const { graceMs = DEFAULT_GRACE_MS, ...spawnOptions } = options;
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: "inherit",
      detached: process.platform !== "win32",
      windowsHide: true,
      ...spawnOptions,
    });

    let timedOut = false;
    let settled = false;
    let killTimer;

    const finish = (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(killTimer);
      resolve(code);
    };

    const timer = setTimeout(() => {
      timedOut = true;
      killTree(child, "SIGTERM");
      killTimer = setTimeout(() => killTree(child, "SIGKILL"), graceMs);
    }, timeoutMs);

    const forward = (signal) => {
      killTree(child, signal);
    };
    const onSigint = () => forward("SIGINT");
    const onSigterm = () => forward("SIGTERM");
    process.on("SIGINT", onSigint);
    process.on("SIGTERM", onSigterm);

    child.on("error", (error) => {
      settled = true;
      clearTimeout(timer);
      clearTimeout(killTimer);
      process.off("SIGINT", onSigint);
      process.off("SIGTERM", onSigterm);
      reject(error);
    });

    child.on("exit", (code, signal) => {
      process.off("SIGINT", onSigint);
      process.off("SIGTERM", onSigterm);
      if (timedOut) {
        finish(124);
      } else if (signal) {
        finish(128 + (os.constants.signals[signal] ?? 0));
      } else {
        finish(code ?? 1);
      }
    });
  });
}

/**
 * Whether `command` must go through cmd.exe. Node refuses to spawn a `.cmd`
 * or `.bat` shim such as `npm.cmd` without a shell (EINVAL), so on Windows
 * those run through one, as check.mjs does. Everything else spawns direct.
 */
export function needsShell(command, platform = process.platform) {
  return platform === "win32" && /\.(cmd|bat)$/i.test(command);
}

// cmd.exe metacharacters. A caret before one makes it literal.
const CMD_META = /([()\][%!^"`<>&|;, *?])/g;

/**
 * The cmd.exe command line that runs the shim `command` with `args` passed
 * through unchanged. `shell: true` would only join the arguments with spaces,
 * so one holding a space or a metacharacter would be split or run by cmd.
 * Each argument is quoted for the program's argv parser, then every
 * metacharacter is caret-escaped twice, because cmd parses the line once for
 * `/c` and again when the shim expands `%*`. The command itself is parsed
 * once, so it is escaped once. This is cross-spawn's rule.
 */
export function cmdShimLine(command, args) {
  const quoted = args.map((arg) => {
    const escaped = String(arg)
      .replace(/(\\*)"/g, '$1$1\\"')
      .replace(/(\\*)$/, "$1$1");
    return `"${escaped}"`.replace(CMD_META, "^^^$1");
  });
  return [command.replace(CMD_META, "^$1"), ...quoted].join(" ");
}

/** What to spawn for `command`: a `.cmd` or `.bat` shim runs through cmd.exe. */
export function spawnPlan(command, args, platform = process.platform) {
  if (!needsShell(command, platform)) return { command, args, options: {} };
  return {
    command: process.env.ComSpec || "cmd.exe",
    args: ["/d", "/s", "/c", `"${cmdShimLine(command, args)}"`],
    options: { windowsVerbatimArguments: true },
  };
}

async function main(argv) {
  const [duration, command, ...args] = argv;
  if (!duration || !command) {
    console.error("usage: node scripts/with-timeout.mjs <seconds> <command> [args...]");
    process.exit(125);
  }
  let timeoutMs;
  try {
    timeoutMs = parseDuration(duration);
  } catch (error) {
    console.error(`with-timeout: ${error.message}`);
    process.exit(125);
  }
  try {
    const plan = spawnPlan(command, args);
    process.exitCode = await runWithTimeout(plan.command, plan.args, timeoutMs, plan.options);
  } catch (error) {
    console.error(`with-timeout: ${error.message}`);
    process.exitCode = 125;
  }
}

const isMain =
  process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  await main(process.argv.slice(2));
}
