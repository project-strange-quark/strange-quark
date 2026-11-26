// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";

import { PE_SUBSYSTEM_CONSOLE, PE_SUBSYSTEM_GUI, parsePe } from "./pe.mjs";

function image({ machine = 0xaa64, magic = 0x20b, subsystem = PE_SUBSYSTEM_GUI, characteristics = 0x0022 } = {}) {
  const pe = 0x80;
  const bytes = Buffer.alloc(pe + 24 + 240);
  bytes.writeUInt16LE(0x5a4d, 0);
  bytes.writeUInt32LE(pe, 0x3c);
  bytes.writeUInt32LE(0x00004550, pe);
  bytes.writeUInt16LE(machine, pe + 4);
  bytes.writeUInt16LE(characteristics, pe + 22);
  bytes.writeUInt16LE(magic, pe + 24);
  bytes.writeUInt16LE(subsystem, pe + 24 + 68);
  return bytes;
}

test("parsePe reads machine, format and a GUI subsystem", () => {
  assert.deepEqual(parsePe(image()), {
    machine: "ARM64",
    format: "PE32+",
    executableImage: true,
    dll: false,
    subsystem: "GUI",
    subsystemId: 2,
  });
});

test("parsePe tells a console x86-64 executable apart", () => {
  const info = parsePe(image({ machine: 0x8664, subsystem: PE_SUBSYSTEM_CONSOLE }));
  assert.equal(info.machine, "x86-64");
  assert.equal(info.subsystem, "console");
});

test("parsePe rejects files that are not PE images", () => {
  assert.equal(parsePe(Buffer.from("\x7fELF not a pe file at all, but long enough to read past the stub")), null);
  const broken = image();
  broken.writeUInt32LE(0, 0x80);
  assert.equal(parsePe(broken), null);
});
