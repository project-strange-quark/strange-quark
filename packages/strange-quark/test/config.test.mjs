// SPDX-License-Identifier: MIT
// Unit tests for packages/strange-quark defineConfig and resolveConfig.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { defineConfig, resolveConfig } from "../dist/index.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const loadConfigBin = path.resolve(__dirname, "../dist/load-config.js");

test("defineConfig returns valid config and rejects malformed inputs", () => {
  const cfg = defineConfig({
    name: "my-app",
    window: { width: 1024, height: 768 },
    build: { env: { NODE_ENV: "production" } },
  });
  assert.equal(cfg.name, "my-app");
  assert.throws(() => defineConfig({ window: { width: -10 } }), RangeError);
  assert.throws(() => defineConfig({ name: "" }), TypeError);
});

test("resolveConfig infers defaults from package.json when no squark.config exists", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-cfg-default-"));
  try {
    fs.writeFileSync(
      path.join(dir, "package.json"),
      JSON.stringify({ name: "@scope/demo-app" }),
    );
    const resolved = await resolveConfig(dir);
    assert.equal(resolved.name, "demo-app");
    assert.equal(resolved.configPath, null);
    assert.deepEqual(resolved.window, {
      title: "demo-app",
      width: 1280,
      height: 800,
      resizable: true,
    });
    assert.equal(resolved.build.entryHtml, "index.html");
    assert.equal(resolved.build.includeSourcemaps, false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("resolveConfig loads squark.config.ts and load-config.js emits JSON", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-cfg-ts-"));
  try {
    fs.writeFileSync(
      path.join(dir, "squark.config.ts"),
      `export default {
        name: "excalidraw-demo",
        window: { title: "Excalidraw", width: 1440, height: 900 },
        build: {
          command: "echo custom-build",
          distDir: "build",
          env: { VITE_APP_ENABLE_PWA: "false" },
        },
        network: { allow: ["https://example.com"] },
      };\n`,
    );
    const resolved = await resolveConfig(dir);
    assert.equal(resolved.name, "excalidraw-demo");
    assert.equal(resolved.window.width, 1440);
    assert.equal(resolved.build.distDir, "build");
    assert.equal(resolved.build.env.VITE_APP_ENABLE_PWA, "false");
    assert.deepEqual(resolved.network.allow, ["https://example.com"]);

    const stdout = execFileSync(process.execPath, [loadConfigBin, dir], { encoding: "utf8" });
    const parsed = JSON.parse(stdout);
    assert.equal(parsed.name, "excalidraw-demo");
    assert.equal(parsed.build.command, "echo custom-build");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("resolveConfig loads squark.config.json and rejects missing explicit path", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-cfg-json-"));
  try {
    fs.writeFileSync(
      path.join(dir, "squark.config.json"),
      JSON.stringify({ name: "json-app", build: { includeSourcemaps: true } }),
    );
    const resolved = await resolveConfig(dir);
    assert.equal(resolved.name, "json-app");
    assert.equal(resolved.build.includeSourcemaps, true);
    await assert.rejects(
      () => resolveConfig(dir, { configPath: "does-not-exist.ts" }),
      /Config file not found/,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("defineConfig accepts native modules and Tauri compatibility, and rejects malformed ones", () => {
  const cfg = defineConfig({
    native: { crate: "native", modules: ["demo"], squarkSource: "../strange-quark" },
    compat: { tauri: { module: "demo" } },
  });
  assert.deepEqual(cfg.native?.modules, ["demo"]);
  assert.throws(() => defineConfig({ native: { crate: "" } }), TypeError);
  assert.throws(() => defineConfig({ native: { modules: ["a", 1] } }), TypeError);
  assert.throws(() => defineConfig({ compat: { tauri: { fixture: "" } } }), TypeError);
  assert.throws(() => defineConfig({ compat: { tauri: null } }), TypeError);
});
