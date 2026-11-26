// SPDX-License-Identifier: MIT
// Tests for the @strange-quark/wpt runner using upstream resources/testharness.js
// and parse5 HTML fixture construction through squark-dom.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import cases from "./cases.json" with { type: "json" };
import { WPT_REVISION, runWptCase, runWptSuite } from "./run.mjs";

test("pinned WPT DOM/events subset passes 100% under upstream testharness.js", async () => {
  const report = await runWptSuite({ quiet: true });
  assert.equal(report.revision, WPT_REVISION);
  assert.equal(report.totalFiles, cases.length);
  assert.ok(report.totalSubtests >= 100, "the pinned DOM subset must run at least the original 100 subtests");
  assert.equal(report.passed, report.totalSubtests);
  assert.equal(report.failed, 0, JSON.stringify(report.failures, null, 2));
  assert.equal(report.passRate, 1);
});

test("upstream testharness.js supports promise_test, async_test, add_cleanup, and assert_throws_dom", async () => {
  const res = await runWptCase("synthetic-testharness.any.js", {
    source: `
      let cleaned = false;
      test((t) => {
        t.add_cleanup(() => { cleaned = true; });
        assert_throws_dom("NotFoundError", () => {
          document.body.removeChild(document.createElement("div"));
        });
      }, "add_cleanup and assert_throws_dom");

      test(() => {
        assert_true(cleaned, "previous test add_cleanup ran");
      }, "cleanup ran before next test");

      async_test((t) => {
        const target = new EventTarget();
        target.addEventListener("ping", t.step_func_done((e) => {
          assert_equals(e.type, "ping");
        }));
        target.dispatchEvent(new Event("ping"));
      }, "async_test step_func_done");

      promise_test(async () => {
        const val = await Promise.resolve(42);
        assert_equals(val, 42);
      }, "promise_test resolves microtasks");
    `,
  });
  assert.equal(res.subtests, 4);
  assert.equal(res.passed, 4);
  assert.equal(res.failed, 0, JSON.stringify(res.failures, null, 2));
});

test("authored HTML fixtures are built via parse5 through squark-dom API", async () => {
  const res = await runWptCase("synthetic-fixture.html", {
    source: `<!DOCTYPE html>
      <html>
        <head>
          <meta charset="utf-8">
          <title>Fixture Test</title>
          <script src="/resources/testharness.js"></script>
          <script src="/resources/testharnessreport.js"></script>
        </head>
        <body>
          <div id="host" data-role="card"><!-- note --><span>hello</span></div>
          <script>
            test(() => {
              const div = document.body.firstElementChild;
              assert_equals(div.localName, "div");
              assert_equals(div.getAttribute("id"), "host");
              assert_equals(div.dataset.role, "card");
              assert_equals(div.firstChild.nodeType, Node.COMMENT_NODE);
              assert_equals(div.firstChild.data, " note ");
              assert_equals(div.lastChild.localName, "span");
              assert_equals(div.lastChild.textContent, "hello");
            }, "parse5 builds DOM fixture through squark-dom");
          </script>
        </body>
      </html>`,
  });
  assert.equal(res.subtests, 1);
  assert.equal(res.failed, 0, JSON.stringify(res.failures, null, 2));
  assert.equal(res.passed, 1);
});

test("failing subtests report status, message, and missingFeature annotation", async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-wpt-fail-"));
  try {
    fs.writeFileSync(
      path.join(tmpDir, "fail.any.js"),
      `test(() => { assert_equals(1, 1); }, "passes");
       test(() => { assert_equals(1, 2, "expected mismatch"); }, "fails");`,
    );
    const expPath = path.join(tmpDir, "expected-failures.json");
    fs.writeFileSync(
      expPath,
      JSON.stringify({ "fail.any.js::fails": "Range API not implemented" }),
    );
    const report = await runWptSuite({
      cases: ["fail.any.js"],
      vendorDir: tmpDir,
      expectedFailuresPath: expPath,
      quiet: true,
    });
    assert.equal(report.passed, 1);
    assert.equal(report.failed, 1);
    assert.equal(report.failures[0].subtest, "fails");
    assert.equal(report.failures[0].status, "FAIL");
    assert.equal(report.failures[0].missingFeature, "Range API not implemented");
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});
