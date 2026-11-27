// SPDX-License-Identifier: MIT

// V8's built-in `console` reports to an inspector and prints nothing without
// one. When the host supplies `consoleWrite` (dev packs, test control, or
// SQUARK_CONSOLE=1), console methods also write a formatted line to stderr.
// Shipped apps get no `consoleWrite`, so their console stays silent, as
// Chromium's does for users.

type ConsoleHost = { consoleWrite?: (level: string, text: string) => void };

const LEVELS: Record<string, string> = {
  log: "log",
  info: "info",
  debug: "debug",
  warn: "warn",
  error: "error",
  trace: "trace",
  dir: "log",
  table: "log",
};

const MAX_DEPTH = 3;
const MAX_ITEMS = 50;
const MAX_STRING = 10_000;

function errorText(error: { name?: unknown; message?: unknown; stack?: unknown }): string {
  if (typeof error.stack === "string" && error.stack !== "") return error.stack;
  return `${String(error.name ?? "Error")}: ${String(error.message ?? "")}`;
}

export function inspect(value: unknown, depth = 0, seen: Set<object> = new Set()): string {
  switch (typeof value) {
    case "string":
      return depth === 0 ? value : JSON.stringify(value.length > MAX_STRING ? `${value.slice(0, MAX_STRING)}…` : value);
    case "number":
      return Object.is(value, -0) ? "-0" : String(value);
    case "bigint":
      return `${value}n`;
    case "symbol":
      return value.toString();
    case "undefined":
      return "undefined";
    case "boolean":
      return String(value);
    case "function":
      return `[Function: ${(value as { name?: string }).name || "(anonymous)"}]`;
  }
  if (value === null) return "null";
  const object = value as object;
  if (object instanceof Error) return depth === 0 ? errorText(object) : `[${String(object.name)}: ${String(object.message)}]`;
  if (seen.has(object)) return "[Circular]";
  if (depth >= MAX_DEPTH) return Array.isArray(object) ? "[Array]" : "[Object]";
  seen.add(object);
  try {
    if (Array.isArray(object)) {
      const items = object.slice(0, MAX_ITEMS).map(item => inspect(item, depth + 1, seen));
      if (object.length > MAX_ITEMS) items.push(`… ${object.length - MAX_ITEMS} more`);
      return `[ ${items.join(", ")} ]`;
    }
    const tag = Object.prototype.toString.call(object).slice(8, -1);
    if (typeof (object as { nodeName?: unknown }).nodeName === "string") {
      const node = object as { nodeName: string; id?: string; className?: unknown };
      const id = node.id ? `#${node.id}` : "";
      const classes = typeof node.className === "string" && node.className ? `.${node.className.trim().split(/\s+/).join(".")}` : "";
      return `<${node.nodeName.toLowerCase()}${id}${classes}>`;
    }
    if (object instanceof Map) {
      const entries = [...object].slice(0, MAX_ITEMS).map(([key, item]) => `${inspect(key, depth + 1, seen)} => ${inspect(item, depth + 1, seen)}`);
      return `Map(${object.size}) { ${entries.join(", ")} }`;
    }
    if (object instanceof Set) {
      const entries = [...object].slice(0, MAX_ITEMS).map(item => inspect(item, depth + 1, seen));
      return `Set(${object.size}) { ${entries.join(", ")} }`;
    }
    let keys: string[];
    try {
      keys = Object.keys(object);
    } catch {
      return `[${tag}]`;
    }
    const entries = keys.slice(0, MAX_ITEMS).map(key => {
      let item: unknown;
      try {
        item = (object as Record<string, unknown>)[key];
      } catch {
        item = "[Getter threw]";
      }
      return `${/^[A-Za-z_$][\w$]*$/.test(key) ? key : JSON.stringify(key)}: ${inspect(item, depth + 1, seen)}`;
    });
    if (keys.length > MAX_ITEMS) entries.push(`… ${keys.length - MAX_ITEMS} more`);
    const prefix = tag === "Object" ? "" : `${tag} `;
    return entries.length === 0 ? `${prefix}{}` : `${prefix}{ ${entries.join(", ")} }`;
  } finally {
    seen.delete(object);
  }
}

/** Format console arguments the way browsers do, including %s/%d/%i/%f/%o/%O/%c. */
export function formatConsole(args: readonly unknown[]): string {
  if (args.length === 0) return "";
  const parts: string[] = [];
  let rest = args;
  const [first] = args;
  if (typeof first === "string" && first.includes("%")) {
    let index = 1;
    const text = first.replace(/%([sdifoOc%])/g, (match, directive: string) => {
      if (directive === "%") return "%";
      if (index >= args.length) return match;
      const value = args[index++];
      switch (directive) {
        case "s":
          return typeof value === "string" ? value : inspect(value, 1);
        case "d":
        case "i": {
          const number = typeof value === "bigint" ? value : Number(value);
          return typeof number === "bigint" ? `${number}n` : String(Math.trunc(number));
        }
        case "f":
          return String(Number(value));
        case "c":
          return "";
        default:
          return inspect(value, 1);
      }
    });
    parts.push(text);
    rest = args.slice(index);
  } else {
    parts.push(inspect(first));
    rest = args.slice(1);
  }
  for (const value of rest) parts.push(inspect(value));
  return parts.join(" ");
}

export function installConsole(global: Record<string, unknown>, host: ConsoleHost): void {
  const write = host.consoleWrite;
  if (typeof write !== "function") return;
  const existing = (global.console ?? {}) as Record<string, unknown>;
  const console = existing;
  const counts = new Map<string, number>();
  const timers = new Map<string, number>();
  const now = () => (globalThis as { performance?: { now(): number } }).performance?.now() ?? Date.now();
  let indent = "";
  const emit = (level: string, text: string) => {
    try {
      write(level, indent === "" ? text : text.split("\n").map(line => indent + line).join("\n"));
    } catch {
      // A console must never throw into the page.
    }
  };
  for (const [method, level] of Object.entries(LEVELS)) {
    const original = existing[method];
    console[method] = function (...args: unknown[]) {
      if (method === "trace") {
        const stack = new Error().stack?.split("\n").slice(2).join("\n") ?? "";
        emit(level, `Trace: ${formatConsole(args)}${stack ? `\n${stack}` : ""}`);
      } else {
        emit(level, formatConsole(args));
      }
      if (typeof original === "function") Reflect.apply(original, existing, args);
    };
  }
  console.assert = (condition?: unknown, ...args: unknown[]) => {
    if (!condition) emit("error", `Assertion failed${args.length ? `: ${formatConsole(args)}` : ""}`);
  };
  console.count = (label = "default") => {
    const count = (counts.get(String(label)) ?? 0) + 1;
    counts.set(String(label), count);
    emit("log", `${String(label)}: ${count}`);
  };
  console.countReset = (label = "default") => void counts.delete(String(label));
  console.time = (label = "default") => void timers.set(String(label), now());
  console.timeLog = (label = "default", ...args: unknown[]) => {
    const start = timers.get(String(label));
    if (start === undefined) return;
    emit("log", `${String(label)}: ${(now() - start).toFixed(3)} ms${args.length ? ` ${formatConsole(args)}` : ""}`);
  };
  console.timeEnd = (label = "default") => {
    (console.timeLog as (label: string) => void)(String(label));
    timers.delete(String(label));
  };
  console.group = console.groupCollapsed = (...args: unknown[]) => {
    if (args.length) emit("log", formatConsole(args));
    indent += "  ";
  };
  console.groupEnd = () => {
    indent = indent.slice(2);
  };
  global.console = console;
}
