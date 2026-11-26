// SPDX-License-Identifier: MIT

/** V8 completes async compilation through its foreground message loop. */
export function installWasm(global: Record<PropertyKey, unknown>, host: { wasmPending(value: boolean): void }): void {
  const wasm = global.WebAssembly as Record<string, unknown>;
  for (const name of ["compile", "instantiate"]) {
    const native = wasm[name];
    if (typeof native !== "function") continue;
    Object.defineProperty(wasm, name, {
      value: async (...args: unknown[]) => {
        host.wasmPending(true);
        try { return await Reflect.apply(native, wasm, args); }
        finally { host.wasmPending(false); }
      },
    });
  }
}
