// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { missingTestFonts } from "./fonts.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

test("missingTestFonts names each family no file provides", () => {
  const root = path.join(repoRoot, "target", "tmp");
  fs.mkdirSync(root, { recursive: true });
  const dir = fs.mkdtempSync(path.join(root, "reftest-fonts-"));
  try {
    assert.deepEqual(missingTestFonts([dir]), ["Liberation Sans", "DejaVu Sans"]);

    fs.writeFileSync(path.join(dir, "LiberationSans-Regular.ttf"), "");
    assert.deepEqual(missingTestFonts([dir]), ["DejaVu Sans"]);

    fs.mkdirSync(path.join(dir, "dejavu"));
    fs.writeFileSync(path.join(dir, "dejavu", "DejaVuSans.ttf"), "");
    assert.deepEqual(missingTestFonts([dir]), []);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
