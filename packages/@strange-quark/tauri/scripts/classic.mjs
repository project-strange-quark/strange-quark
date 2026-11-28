// SPDX-License-Identifier: MIT
// Turns dist/index.js (an import-free module) into the classic script that
// `squark build` and `squark dev` run before the app's own scripts.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function toClassicScript(source) {
  const normalized = source.replace(/\r\n?/g, "\n");
  if (/^\s*import\s/m.test(normalized)) throw new Error("src/index.ts must not import anything");
  const body = normalized
    .replace(/^export (?=(?:async )?function |const |class |let )/gm, "")
    .replace(/^export \{[^}]*\};?$/gm, "");
  return `// SPDX-License-Identifier: MIT\n// Generated from @strange-quark/tauri by scripts/classic.mjs.\n(() => {\n${body}\ninstallTauriFromConfig(globalThis);\n})();\n`;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const source = fs.readFileSync(new URL("../dist/index.js", import.meta.url), "utf8");
  fs.writeFileSync(new URL("../dist/tauri.classic.js", import.meta.url), toClassicScript(source));
}
