// SPDX-License-Identifier: MIT

import { journal } from "./records.mjs";

// Fixture-local atoms for div (1), the HTML namespace (2), and span (3).
const batch = journal();
batch.record(0x01, 1, 1, 2);

let microtaskRan = false;
setTimeout(() => {
  if (!microtaskRan) {
    throw new Error("The timer ran before the microtask");
  }
  batch.record(0x03, 1, 2, 0);
  if (__squark.flush(batch.buffer, batch.buffer.byteLength) !== 3) {
    throw new Error("Rust did not acknowledge three records");
  }
}, 0);

Promise.resolve().then(() => {
  batch.record(0x01, 2, 3, 2);
  microtaskRan = true;
});
