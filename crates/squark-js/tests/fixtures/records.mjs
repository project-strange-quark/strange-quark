// SPDX-License-Identifier: MIT

export function journal() {
  const headerBytes = 16;
  const recordBytes = 16;
  const buffer = new ArrayBuffer(headerBytes + 3 * recordBytes + 4);
  const view = new DataView(buffer);
  view.setUint32(0, 3 * recordBytes, true);
  view.setUint32(12, 1, true);

  let offset = headerBytes;
  return {
    buffer,
    record(opcode, a, b, c) {
      view.setUint8(offset, opcode);
      view.setUint32(offset + 4, a, true);
      view.setUint32(offset + 8, b, true);
      view.setUint32(offset + 12, c, true);
      offset += recordBytes;
    },
  };
}
