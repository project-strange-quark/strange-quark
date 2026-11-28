// SPDX-License-Identifier: MIT
// Converts an import-free @strange-quark/tauri ES module into the classic
// script that `squark build` and `squark dev` run before the app's own scripts.

export function toClassicScript(source) {
  const normalized = source.replace(/\r\n?/g, "\n");
  if (/^\s*import\s/m.test(normalized)) throw new Error("src/index.ts must not import anything");
  const body = normalized
    .replace(/^export (?=(?:async )?function |const |class |let )/gm, "")
    .replace(/^export \{[^}]*\};?$/gm, "");
  return `// SPDX-License-Identifier: MIT\n// Generated from @strange-quark/tauri by scripts/classic.mjs.\n(() => {\n${body}\ninstallTauriFromConfig(globalThis);\n})();\n`;
}
