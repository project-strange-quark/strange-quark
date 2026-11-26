// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { bootstrapDocument, installPlatform } from "@strange-quark/dom";
import { Journal } from "@strange-quark/dom/journal";

function navigatorFor(host = {}) {
  const global = { Blob, File, reportError() {} };
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  installPlatform(global, document, {
    utf8Encode: text => new TextEncoder().encode(text),
    utf8Decode: bytes => new TextDecoder().decode(bytes),
    postTask() {},
    ...host,
  });
  return global.navigator;
}

test("navigator identifies Linux x86_64 and aarch64", () => {
  for (const [arch, suffix] of [["x86_64", "x86_64"], ["aarch64", "aarch64"]]) {
    const navigator = navigatorFor({ os: "linux", arch });
    assert.equal(navigator.userAgent, `Mozilla/5.0 (X11; Linux ${suffix}) StrangeQuark`);
    assert.equal(navigator.platform, `Linux ${suffix}`);
  }
});

test("navigator identifies macOS and Windows independent of architecture", () => {
  const mac = navigatorFor({ os: "macos", arch: "aarch64" });
  assert.equal(mac.userAgent, "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) StrangeQuark");
  assert.equal(mac.platform, "MacIntel");

  const windows = navigatorFor({ os: "windows", arch: "aarch64" });
  assert.equal(windows.userAgent, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) StrangeQuark");
  assert.equal(windows.platform, "Win32");
});

test("navigator defaults to Linux x86_64 when host identity is omitted", () => {
  const navigator = navigatorFor();
  assert.equal(navigator.userAgent, "Mozilla/5.0 (X11; Linux x86_64) StrangeQuark");
  assert.equal(navigator.platform, "Linux x86_64");
});
