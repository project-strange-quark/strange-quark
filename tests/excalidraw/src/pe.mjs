// SPDX-License-Identifier: MIT
// Reads the fields of a Windows PE executable's headers that packaging checks.

import fs from "node:fs";

export const PE_SUBSYSTEM_GUI = 2;
export const PE_SUBSYSTEM_CONSOLE = 3;

const MACHINES = new Map([
  [0x8664, "x86-64"],
  [0xaa64, "ARM64"],
  [0x014c, "i386"],
]);

/**
 * Parses the DOS stub, PE signature, COFF header and the start of the
 * optional header. Returns null when `bytes` is not a PE image.
 */
export function parsePe(bytes) {
  if (bytes.length < 0x40 || bytes.readUInt16LE(0) !== 0x5a4d) return null; // "MZ"
  const pe = bytes.readUInt32LE(0x3c);
  const optional = pe + 24;
  if (optional + 70 > bytes.length || bytes.readUInt32LE(pe) !== 0x00004550) return null; // "PE\0\0"
  const machine = bytes.readUInt16LE(pe + 4);
  const characteristics = bytes.readUInt16LE(pe + 22);
  const magic = bytes.readUInt16LE(optional);
  // The subsystem sits at the same offset in PE32 and PE32+ optional headers.
  const subsystem = bytes.readUInt16LE(optional + 68);
  return {
    machine: MACHINES.get(machine) ?? `0x${machine.toString(16)}`,
    format: magic === 0x20b ? "PE32+" : magic === 0x10b ? "PE32" : `0x${magic.toString(16)}`,
    executableImage: (characteristics & 0x0002) !== 0,
    dll: (characteristics & 0x2000) !== 0,
    subsystem:
      subsystem === PE_SUBSYSTEM_GUI ? "GUI" : subsystem === PE_SUBSYSTEM_CONSOLE ? "console" : String(subsystem),
    subsystemId: subsystem,
  };
}

/** Reads the headers of the PE file at `file`, which lie in its first 4 KiB. */
export function readPe(file) {
  const handle = fs.openSync(file, "r");
  try {
    const bytes = Buffer.alloc(4096);
    const length = fs.readSync(handle, bytes, 0, bytes.length, 0);
    return parsePe(bytes.subarray(0, length));
  } finally {
    fs.closeSync(handle);
  }
}
