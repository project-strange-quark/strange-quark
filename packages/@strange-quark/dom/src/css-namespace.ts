// SPDX-License-Identifier: MIT

/** CSSOM's serialize-an-identifier algorithm. */
function escapeIdentifier(value: string): string {
  const text = String(value);
  let escaped = "";
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    const digit = code >= 48 && code <= 57;
    if (code === 0) escaped += "\ufffd";
    else if (code <= 31 || code === 127 || digit && (i === 0 || i === 1 && text[0] === "-")) {
      escaped += "\\" + code.toString(16) + " ";
    } else if (i === 0 && text === "-") escaped += "\\-";
    else if (code >= 128 || digit || code >= 65 && code <= 90 || code >= 97 && code <= 122 || code === 45 || code === 95) {
      escaped += text[i];
    } else escaped += "\\" + text[i];
  }
  return escaped;
}

export function installCSSNamespace(global: Record<PropertyKey, unknown>, host: { cssSupports(condition: string): boolean }): void {
  global.CSS = {
    escape: escapeIdentifier,
    supports(condition: string, value?: string): boolean {
      if (arguments.length === 0) throw new TypeError("CSS.supports requires a condition");
      return host.cssSupports(arguments.length === 1 ? String(condition) : `(${condition}: ${value})`);
    },
    [Symbol.toStringTag]: "CSS",
  };
}
