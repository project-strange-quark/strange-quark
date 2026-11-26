// SPDX-License-Identifier: MIT
// Minimal `.sqpak` reader for tests: header, sorted index, string pool, payloads.

import fs from "node:fs";

const HEADER_SIZE = 32;
const ENTRY_SIZE = 32;

/** Reads a standalone `.sqpak` file into `{ path, mime, data }` entries. */
export function readSqPak(filePath) {
  const bytes = fs.readFileSync(filePath);
  const magic = bytes.subarray(0, 8).toString("latin1");
  if (!magic.startsWith("SQPK")) {
    throw new Error(`${filePath}: not a SQPK archive (${JSON.stringify(magic)})`);
  }
  const entryCount = bytes.readUInt32LE(8);
  const indexOffset = bytes.readUInt32LE(12);
  const stringsOffset = bytes.readUInt32LE(16);
  const stringsLen = bytes.readUInt32LE(20);
  const dataOffset = bytes.readUInt32LE(24);
  const strings = bytes.subarray(stringsOffset, stringsOffset + stringsLen);

  const entries = [];
  for (let i = 0; i < entryCount; i += 1) {
    const at = indexOffset + i * ENTRY_SIZE;
    const pathOff = bytes.readUInt32LE(at + 8);
    const pathLen = bytes.readUInt16LE(at + 12);
    const mimeId = bytes.readUInt16LE(at + 14);
    const dataOff = bytes.readUInt32LE(at + 16);
    const dataLen = bytes.readUInt32LE(at + 20);
    const mimeOff = bytes.readUInt32LE(at + 24);
    const mimeLen = bytes.readUInt16LE(at + 28);
    const path = strings.subarray(pathOff, pathOff + pathLen).toString("utf8");
    const mime =
      mimeId === 0xffff
        ? strings.subarray(mimeOff, mimeOff + mimeLen).toString("utf8")
        : `builtin:${mimeId}`;
    entries.push({
      path,
      mime,
      data: bytes.subarray(dataOffset + dataOff, dataOffset + dataOff + dataLen),
    });
  }
  return entries;
}

/** Returns one entry's bytes, or null. */
export function sqpakEntry(entries, path) {
  return entries.find((entry) => entry.path === path)?.data ?? null;
}

/** Parses `/__squark/manifest.json`. */
export function sqpakManifest(entries) {
  const data = sqpakEntry(entries, "/__squark/manifest.json");
  return data ? JSON.parse(data.toString("utf8")) : null;
}
