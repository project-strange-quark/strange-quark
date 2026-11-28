// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { once } from "node:events";
import { setTimeout as sleep } from "node:timers/promises";
import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack] = process.argv.slice(2);
const launch = () => SquarkDriver.launch({ binaryPath: runtime, args: [pack], width: 300, height: 200, headless: false });
let driver = await launch();

async function waitFor(expression) {
  const deadline = performance.now() + 5000;
  while (performance.now() < deadline) {
    if (await driver.eval(expression)) return;
    await sleep(20);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

try {
  await waitFor("log.includes('menu-set')");
  await driver.eval(`(() => {
    window.cancelQuit = true;
    addEventListener("keydown", event => {
      if (event.metaKey && event.code === "KeyQ" && window.cancelQuit) {
        event.preventDefault();
        log.push("page:quit");
      }
    });
    const native = globalThis[Symbol.for("strange-quark.native")];
    native.window.setMenu([
      { id: "custom.hide", label: "Custom Hide", accelerator: "CmdOrCtrl+H" },
      { id: "custom.minimize", label: "Custom Minimize", accelerator: "CmdOrCtrl+M" },
    ]).then(() => log.push("custom-menu-set"));
    return true;
  })()`);
  await waitFor("log.includes('custom-menu-set')");
  await driver.press("q", { primary: true });
  await waitFor("log.includes('page:quit')");

  await driver.press("h", { primary: true });
  await waitFor("log.includes('menu:custom.hide')");
  await driver.press("m", { primary: true });
  await waitFor("log.includes('menu:custom.minimize')");

  await driver.eval(`(() => {
    window.cancelQuit = false;
    const status = document.body.appendChild(document.createElement("p"));
    let frame = 0;
    const animate = () => {
      status.textContent = String(++frame);
      requestAnimationFrame(animate);
    };
    requestAnimationFrame(animate);
    return true;
  })()`);
  await driver.nextPresented();
  const exitSignal = AbortSignal.timeout(5000);
  // This is a keydown only because an accepted quit can close the socket
  // before keyup. A clean child exit verifies that the default ran.
  const [exit] = await Promise.all([
    once(driver._child, "exit", { signal: exitSignal }),
    driver.request("input.key", { kind: "down", key: "q", code: "KeyQ", modifiers: 8 }),
  ]);
  assert.deepEqual(exit, [0, null]);
} finally {
  await driver.close();
}

for (const key of ["h", "m"]) {
  driver = await launch();
  try {
    await waitFor("document.visibilityState === 'visible'");
    await driver.press(key, { primary: true });
    await waitFor("document.visibilityState === 'hidden'");
  } finally {
    await driver.close();
  }
}
