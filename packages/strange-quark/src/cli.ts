#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Thin CLI shim forwarding `npx squark ...` to the native Rust `squark` binary.

import { spawnSync } from "node:child_process";
import os from "node:os";

import { SquarkBinaryNotFoundError, resolveSquarkBinary } from "./resolve-bin.js";

let resolved;
try {
  resolved = resolveSquarkBinary();
} catch (error) {
  if (error instanceof SquarkBinaryNotFoundError) {
    console.error(error.message);
  } else {
    console.error(
      `strange-quark: failed to locate native squark binary: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  process.exit(1);
}

// Ignore SIGINT while the child runs so Ctrl-C reaches the native process first
// and we propagate its final exit status or signal cleanly.
const ignoreSigint = (): void => {};
process.on("SIGINT", ignoreSigint);

const result = spawnSync(resolved.binaryPath, process.argv.slice(2), {
  stdio: "inherit",
  shell: false,
});

process.off("SIGINT", ignoreSigint);

if (result.error) {
  console.error(`strange-quark: failed to execute ${resolved.binaryPath}: ${result.error.message}`);
  process.exit(1);
}

if (result.signal) {
  try {
    process.kill(process.pid, result.signal);
  } catch {
    // Re-raising a signal can fail on Windows or restricted environments.
  }
  const signalNumber = os.constants.signals[result.signal];
  process.exit(typeof signalNumber === "number" ? 128 + signalNumber : 1);
}

process.exit(result.status ?? 1);
