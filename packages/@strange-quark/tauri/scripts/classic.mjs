// SPDX-License-Identifier: MIT
// Turns dist/index.js (an import-free module) into the classic script that
// `squark build` and `squark dev` run before the app's own scripts.

import fs from "node:fs";

const source = fs.readFileSync(new URL("../dist/index.js", import.meta.url), "utf8");
if (/^\s*import\s/m.test(source)) throw new Error("src/index.ts must not import anything");
const body = source.replace(/^export (?=(?:async )?function |const |class |let )/gm, "").replace(/^export \{[^}]*\};?$/gm, "");
const script = `// SPDX-License-Identifier: MIT\n// Generated from @strange-quark/tauri by scripts/classic.mjs.\n(() => {\n${body}\ninstallTauriFromConfig(globalThis);\n})();\n`;
fs.writeFileSync(new URL("../dist/tauri.classic.js", import.meta.url), script);
