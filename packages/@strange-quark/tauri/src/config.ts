// SPDX-License-Identifier: MIT

// squark.config helper: `defineConfig({ ...tauri({ module: "app" }), ... })`
// runs this package's shim before the app through squark.config's generic
// `preludes` option. Strange Quark itself knows nothing about Tauri.

export interface TauriOptions {
  /** The native module Tauri commands go to. */
  module?: string;
  /**
   * A project-relative classic script that defines
   * `globalThis.__STRANGE_QUARK_TAURI_FIXTURE__` (canned replies), to run the
   * frontend without native modules, for example to compare it with Chromium.
   */
  fixture?: string;
  /** The window label Tauri apps see (default "main"). */
  label?: string;
}

export interface TauriPrelude {
  file: string;
  globals?: Record<string, unknown>;
}

export const SHIM_FILE = "@strange-quark/tauri/dist/tauri.classic.js";

function fixturePath(fixture: string): string {
  if (
    fixture.startsWith(".") ||
    fixture.startsWith("/") ||
    fixture.startsWith("\\\\") ||
    /^[a-zA-Z]:[\\/]/.test(fixture)
  ) {
    return fixture;
  }
  return `./${fixture}`;
}

export function tauri(options: TauriOptions = {}): { preludes: TauriPrelude[] } {
  const config: Record<string, unknown> = {};
  if (options.module !== undefined) config.module = options.module;
  if (options.label !== undefined) config.label = options.label;
  const preludes: TauriPrelude[] = [];
  if (options.fixture !== undefined) {
    config.fixture = true;
    preludes.push({ file: fixturePath(options.fixture) });
  }
  preludes.push({ file: SHIM_FILE, globals: { __STRANGE_QUARK_TAURI__: config } });
  return { preludes };
}
