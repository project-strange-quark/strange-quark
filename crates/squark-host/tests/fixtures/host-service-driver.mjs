// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, output] = process.argv.slice(2);
const log = path.join(output, "external-urls.txt");
const command = path.join(output, process.platform === "win32" ? "open-external.cmd" : "open-external");
if (process.platform === "win32") {
  // CreateProcess cannot run a #!/bin/sh script, so the opener is a .cmd
  // that hands the URL to Node, which appends it to the log the same way.
  const recorder = path.join(output, "open-external.mjs");
  fs.writeFileSync(
    recorder,
    `import fs from "node:fs";\nfs.appendFileSync(${JSON.stringify(log)}, process.argv[2] + "\\n");\n`,
  );
  fs.writeFileSync(command, `@"${process.execPath}" "${recorder}" "%~1"\r\n`);
} else {
  const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
  fs.writeFileSync(command, `#!/bin/sh\nprintf '%s\\n' "$1" >> ${quote(log)}\n`);
  fs.chmodSync(command, 0o755);
}

const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("host-service") : path.join(output, "squark.sock"),
  width: 120,
  height: 90,
  env: { SQUARK_OPEN_EXTERNAL_COMMAND: command },
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

async function lines() {
  try {
    // The opener's `>>` creates the log before it writes the URL, and
    // "".split() is [""], so an empty log must count as no lines.
    const text = fs.readFileSync(log, "utf8").trimEnd();
    return text ? text.split(/\r?\n/) : [];
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

async function click(x, y) {
  const pointer = { x, y, button: 0, modifiers: 0, pointerId: 1 };
  await driver.request("input.pointer", { ...pointer, kind: "down", buttons: 1 });
  await driver.request("input.pointer", { ...pointer, kind: "up", buttons: 0 });
}

try {
  assert.deepEqual(await driver.eval("window.openResult"), null);
  // The opener runs as its own process; wait for it to write, as the later
  // steps do, instead of assuming it finished before the first eval.
  for (let attempt = 0; attempt < 200 && (await lines()).length < 1; attempt++) await sleep(25);
  assert.deepEqual(await lines(), ["https://example.com/a"]);

  await click(10, 10);
  await waitFor("window.linkClicks.includes('external')");
  await waitFor("window.linkClicks.includes('external') && location.pathname === '/' && window.openResult === null");
  for (let attempt = 0; attempt < 200 && (await lines()).length < 2; attempt++) await sleep(25);
  assert.deepEqual(await lines(), ["https://example.com/a", "https://example.com/b"]);

  await click(10, 36);
  await waitFor("location.pathname === '/same-origin'");
  assert.deepEqual(await driver.eval("window.linkClicks"), ["external", "same-origin"]);

  await driver.eval("window.fileOpenResult = window.open('file:///etc/passwd')");
  await click(10, 62);
  await sleep(200);
  assert.deepEqual(await driver.eval("[window.fileOpenResult, window.javascriptRan, location.pathname, window.linkClicks]"), [
    null,
    false,
    "/same-origin",
    ["external", "same-origin", "javascript"],
  ]);
  assert.deepEqual(await lines(), ["https://example.com/a", "https://example.com/b"]);
} finally {
  await driver.close();
}