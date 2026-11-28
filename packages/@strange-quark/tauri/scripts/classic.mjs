// SPDX-License-Identifier: MIT
// Turns dist/index.js (an import-free module) into the classic script that
// `squark build` and `squark dev` run before the app's own scripts.

import fs from "node:fs";

import { toClassicScript } from "./classic-script.mjs";

const source = fs.readFileSync(new URL("../dist/index.js", import.meta.url), "utf8");
fs.writeFileSync(new URL("../dist/tauri.classic.js", import.meta.url), toClassicScript(source));
