// SPDX-License-Identifier: MIT
// Mock NDJSON test-control server speaking the runtime's exact 8-method
// test-control protocol. Supports running counter scripts
// in-process or as a spawned child process for SquarkDriver integration tests.

import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";

function makeTinyPng(width = 800, height = 600) {
  const png = new PNG({ width, height });
  for (let i = 0; i < png.data.length; i += 4) {
    png.data[i] = 248;
    png.data[i + 1] = 250;
    png.data[i + 2] = 252;
    png.data[i + 3] = 255;
  }
  return PNG.sync.write(png);
}

/**
 * Starts a mock `--test-control` NDJSON server on `socketPath` that simulates
 * a stateful Counter app and records every RPC call received.
 */
export async function startMockTestControlServer(socketPath, options = {}) {
  const width = options.width ?? 800;
  const height = options.height ?? 600;
  const screenshotMode = options.screenshotMode ?? "base64"; // "base64" | "file"
  const calls = [];
  let frameId = 0;

  // Stateful mock Counter DOM for hit-testing & dom.eval.
  const state = {
    count: 0,
    step: 1,
    history: [],
    quit: false,
  };

  const rects = {
    '[data-testid="count"]': { x: 240, y: 120, width: 320, height: 80 },
    '[data-testid="dec"]': { x: 240, y: 220, width: 100, height: 40 },
    '[data-testid="inc"]': { x: 350, y: 220, width: 100, height: 40 },
    '[data-testid="reset"]': { x: 460, y: 220, width: 100, height: 40 },
    '[data-testid="step-1"]': { x: 280, y: 280, width: 40, height: 30 },
    '[data-testid="step-5"]': { x: 330, y: 280, width: 40, height: 30 },
    '[data-testid="step-10"]': { x: 380, y: 280, width: 40, height: 30 },
  };

  function hitTest(x, y) {
    for (const [sel, r] of Object.entries(rects)) {
      if (x >= r.x && x <= r.x + r.width && y >= r.y && y <= r.y + r.height) {
        return sel;
      }
    }
    return null;
  }

  function evalMockDom(script) {
    const fakeDocument = {
      querySelector(sel) {
        if (sel === '[data-testid="count"]') {
          return { textContent: String(state.count), getAttribute: () => null };
        }
        if (sel === '[data-testid="step-1"]' || sel === '[data-testid="step-5"]' || sel === '[data-testid="step-10"]') {
          const val = Number(sel.match(/step-(\d+)/)[1]);
          return {
            textContent: String(val),
            getAttribute: (name) => (name === "aria-pressed" ? String(state.step === val) : null),
          };
        }
        if (sel in rects) {
          return { textContent: "", getAttribute: () => null };
        }
        return null;
      },
      querySelectorAll(sel) {
        if (sel === '[data-testid="history-item"]') {
          return state.history.map((text) => ({ textContent: text }));
        }
        return [];
      },
    };
    const fn = new Function("document", `return (${script});`);
    return fn(fakeDocument);
  }

  function handleRpc(method, params) {
    calls.push({ method, params });

    if (options.failMethod === method) {
      const err = new Error(`forced error for ${method}`);
      err.code = -32001;
      throw err;
    }

    switch (method) {
      case "input.pointer": {
        if (params.kind === "up") {
          const hit = hitTest(params.x, params.y);
          if (hit === '[data-testid="inc"]') {
            state.count += state.step;
            state.history.push(`+${state.step} → ${state.count}`);
          } else if (hit === '[data-testid="dec"]') {
            state.count -= state.step;
            state.history.push(`-${state.step} → ${state.count}`);
          } else if (hit === '[data-testid="reset"]') {
            state.count = 0;
            state.history = [];
          } else if (hit === '[data-testid="step-1"]') {
            state.step = 1;
          } else if (hit === '[data-testid="step-5"]') {
            state.step = 5;
          } else if (hit === '[data-testid="step-10"]') {
            state.step = 10;
          }
        }
        return { ok: true };
      }
      case "input.wheel":
      case "input.key":
        return { ok: true };
      case "frame.waitIdle": {
        frameId += params.frames ?? 2;
        return { frameId };
      }
      case "frame.screenshot": {
        const png = makeTinyPng(width, height);
        if (screenshotMode === "file" && params.path) {
          fs.mkdirSync(path.dirname(params.path), { recursive: true });
          fs.writeFileSync(params.path, png);
          return { width, height, path: params.path };
        }
        return { width, height, pngBase64: png.toString("base64") };
      }
      case "dom.queryRect":
        return { rect: rects[params.selector] ?? null };
      case "dom.eval":
        return { value: evalMockDom(params.script) };
      case "app.quit":
        state.quit = true;
        return { ok: true };
      default: {
        const err = new Error(`Method not found: ${method}`);
        err.code = -32601;
        throw err;
      }
    }
  }

  try {
    fs.unlinkSync(socketPath);
  } catch {
    // Ignore if absent.
  }

  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.setEncoding("utf8");
    let buf = "";
    socket.on("data", (chunk) => {
      buf += chunk;
      while (true) {
        const nl = buf.indexOf("\n");
        if (nl === -1) break;
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line) continue;
        const req = JSON.parse(line);
        try {
          const result = handleRpc(req.method, req.params ?? {});
          socket.write(JSON.stringify({ jsonrpc: "2.0", id: req.id, result }) + "\n");
          if (req.method === "app.quit" && options.exitOnQuit) {
            setImmediate(() => {
              server.close(() => process.exit(0));
            });
          }
        } catch (err) {
          socket.write(
            JSON.stringify({
              jsonrpc: "2.0",
              id: req.id,
              error: { code: err.code ?? -32000, message: err.message },
            }) + "\n",
          );
        }
      }
    });
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });

  return {
    socketPath,
    calls,
    state,
    close: async () => {
      for (const s of sockets) s.destroy();
      await new Promise((resolve) => server.close(() => resolve()));
      try {
        fs.unlinkSync(socketPath);
      } catch {
        // Ignore cleanup failure.
      }
    },
  };
}

// Allow running as a standalone spawned mock runtime process:
//   node mock-server.mjs --test-control <sock> [--headless] [--width W] [--height H]
const isMain =
  process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  let socketPath = null;
  let width = 800;
  let height = 600;
  for (let i = 2; i < process.argv.length; i++) {
    if (process.argv[i] === "--test-control" && process.argv[i + 1]) {
      socketPath = process.argv[++i];
    } else if (process.argv[i] === "--width" && process.argv[i + 1]) {
      width = Number(process.argv[++i]);
    } else if (process.argv[i] === "--height" && process.argv[i + 1]) {
      height = Number(process.argv[++i]);
    }
  }
  if (!socketPath) {
    console.error("mock-server requires --test-control <socket>");
    process.exit(2);
  }
  await startMockTestControlServer(socketPath, {
    width,
    height,
    screenshotMode: "file",
    exitOnQuit: true,
  });
}
