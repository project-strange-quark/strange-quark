// SPDX-License-Identifier: MIT

import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export function controlEndpoint(platform = process.platform) {
  const name = `sqm-${process.pid}-${randomBytes(5).toString("hex")}`;
  if (platform === "win32") return `\\\\.\\pipe\\${name}`;
  // macOS has a 104-byte sun_path, and TMPDIR may already be deep.
  const endpoint = path.join(os.tmpdir(), `${name}.sock`);
  return Buffer.byteLength(endpoint) < 104 ? endpoint : `/tmp/${name}.sock`;
}

export function removeControlEndpoint(endpoint) {
  if (endpoint && process.platform !== "win32") fs.rmSync(endpoint, { force: true });
}

export function appExecutable(input) {
  if (process.platform !== "darwin" || !input.endsWith(".app") ||
      !fs.statSync(input, { throwIfNoEntry: false })?.isDirectory()) return input;
  const contents = path.join(input, "Contents");
  const executable = execFileSync("/usr/bin/plutil", [
    "-extract", "CFBundleExecutable", "raw", "-o", "-", path.join(contents, "Info.plist"),
  ], { encoding: "utf8", timeout: 5000 }).trim();
  return path.join(contents, "MacOS", executable);
}

export function appArtifactBytes(input) {
  if (process.platform === "darwin") {
    const binaryDir = path.dirname(input);
    const contents = path.dirname(binaryDir);
    const bundle = path.basename(binaryDir) === "MacOS" && path.basename(contents) === "Contents"
      ? path.dirname(contents)
      : input;
    if (bundle.endsWith(".app") && fs.statSync(bundle).isDirectory()) {
      return fs.readdirSync(bundle, { recursive: true, withFileTypes: true })
        .filter((entry) => entry.isFile())
        .reduce((bytes, entry) => bytes + fs.statSync(path.join(entry.parentPath, entry.name)).size, 0);
    }
  }
  return fs.statSync(input).size;
}

export function processTreeRssKb(rows, rootPid) {
  const processes = new Map();
  const children = new Map();
  for (const row of rows.split(/\r?\n/)) {
    if (!/^\s*\d+\s+\d+\s+\d+\s*$/.test(row)) continue;
    const [pid, parent, rss] = row.trim().split(/\s+/).map(Number);
    processes.set(pid, rss);
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push(pid);
  }
  if (!processes.has(rootPid)) return null;
  let total = 0;
  const visited = new Set();
  const pending = [rootPid];
  while (pending.length) {
    const pid = pending.pop();
    if (visited.has(pid)) continue;
    visited.add(pid);
    total += processes.get(pid);
    for (const child of children.get(pid) ?? []) pending.push(child);
  }
  return total;
}

export function rssKbOfTree(pid) {
  const options = { encoding: "utf8", timeout: 5000, windowsHide: true };
  try {
    // Both commands emit PID, parent PID and resident memory in KiB.
    const rows = process.platform === "win32"
      ? execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
        'Get-CimInstance Win32_Process | ForEach-Object { "$($_.ProcessId) $($_.ParentProcessId) $([Math]::Floor($_.WorkingSetSize / 1KB))" }',
      ], options)
      : execFileSync("ps", ["-A", "-o", "pid=,ppid=,rss="], options);
    return processTreeRssKb(rows, pid);
  } catch {
    return null;
  }
}
