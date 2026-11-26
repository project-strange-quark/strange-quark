// SPDX-License-Identifier: MIT
// WPT Node runner for @strange-quark/dom.
// Loads unmodified upstream resources/testharness.js (pinned at f4d3f2eb,
// BSD-3-Clause) inside each test's isolated squark-dom realm with a
// testharnessreport hook, and builds authored HTML fixtures through squark-dom's
// own DOM API from a parse5 parse.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { parse as parseHtml } from "parse5";

export const WPT_REVISION = "f4d3f2eba09697ebcb2f338b8f2d709a3ce85217";

const here = new URL("./", import.meta.url);
const defaultDist = fileURLToPath(new URL("../../packages/@strange-quark/dom/dist/", here));
const defaultVendorDir = fileURLToPath(new URL("./vendor/", here));
const defaultCasesPath = fileURLToPath(new URL("./cases.json", here));
const defaultExpectedFailuresPath = fileURLToPath(new URL("./expected-failures.json", here));
const defaultTestharnessPath = fileURLToPath(new URL("./vendor/resources/testharness.js", here));

const HTML_NS = "http://www.w3.org/1999/xhtml";

function loadDomModule(context, identifier, modules) {
  let module = modules.get(identifier);
  if (module !== undefined) return module;
  let source;
  try {
    source = readFileSync(identifier, "utf8");
  } catch (error) {
    throw new Error(
      `Cannot read ${identifier}. Build squark-dom with npm run build --workspace @strange-quark/dom.`,
      { cause: error },
    );
  }
  module = new vm.SourceTextModule(source, { context, identifier });
  modules.set(identifier, module);
  return module;
}

function getAttr(astNode, name) {
  if (!astNode.attrs) return null;
  for (const attr of astNode.attrs) {
    if (!attr.namespace && attr.name === name) return attr.value;
  }
  return null;
}

function applyAttrs(domElement, astNode) {
  if (!astNode.attrs) return;
  for (const attr of astNode.attrs) {
    if (attr.namespace) {
      const qname = attr.prefix ? `${attr.prefix}:${attr.name}` : attr.name;
      domElement.setAttributeNS(attr.namespace, qname, attr.value);
    } else {
      domElement.setAttribute(attr.name, attr.value);
    }
  }
}

function collectText(astNode) {
  if (!astNode.childNodes) return "";
  let out = "";
  for (const child of astNode.childNodes) {
    if (child.nodeName === "#text") out += child.value;
  }
  return out;
}

/**
 * Builds an authored HTML fixture into `document` using `squark-dom`'s own DOM API,
 * returning the ordered list of scripts to execute for the WPT file.
 */
function populateDocumentFromHtml(document, htmlSource, casePath, vendorDir) {
  const ast = parseHtml(htmlSource);
  const scriptsToRun = [];

  const htmlEl = document.documentElement;
  const headEl = document.head;
  const bodyEl = document.body;

  // Clear initial empty <head> and <body> while preserving the Document skeleton.
  while (headEl.firstChild) headEl.removeChild(headEl.firstChild);
  while (bodyEl.firstChild) bodyEl.removeChild(bodyEl.firstChild);

  function buildNode(astNode, targetParent) {
    if (astNode.nodeName === "#text") {
      targetParent.appendChild(document.createTextNode(astNode.value));
      return;
    }
    if (astNode.nodeName === "#comment") {
      targetParent.appendChild(document.createComment(astNode.data ?? ""));
      return;
    }
    if (astNode.nodeName === "#documentType") {
      return;
    }
    if (!astNode.tagName) return;

    const ns = astNode.namespaceURI || HTML_NS;
    const domEl =
      ns === HTML_NS
        ? document.createElement(astNode.tagName)
        : document.createElementNS(ns, astNode.tagName);
    applyAttrs(domEl, astNode);
    targetParent.appendChild(domEl);

    if (ns === HTML_NS && astNode.tagName === "script") {
      const typeAttr = (getAttr(astNode, "type") || "").trim().toLowerCase();
      const isJs =
        typeAttr === "" ||
        typeAttr === "text/javascript" ||
        typeAttr === "application/javascript";
      if (isJs) {
        const src = getAttr(astNode, "src");
        if (src === null) {
          scriptsToRun.push({ filename: casePath, code: collectText(astNode) });
        } else if (!/^\/resources\/testharness(?:report)?\.js$/.test(src)) {
          const resolved = resolveWptScriptPath(src, casePath, vendorDir);
          scriptsToRun.push({ filename: src, code: readFileSync(resolved, "utf8") });
        }
      }
    }

    if (astNode.childNodes) {
      for (const child of astNode.childNodes) {
        buildNode(child, domEl);
      }
    }
    if (astNode.content && astNode.content.childNodes && domEl.content) {
      for (const child of astNode.content.childNodes) {
        buildNode(child, domEl.content);
      }
    }
  }

  for (const topChild of ast.childNodes || []) {
    if (topChild.tagName === "html") {
      applyAttrs(htmlEl, topChild);
      for (const section of topChild.childNodes || []) {
        if (section.tagName === "head") {
          applyAttrs(headEl, section);
          for (const child of section.childNodes || []) buildNode(child, headEl);
        } else if (section.tagName === "body") {
          applyAttrs(bodyEl, section);
          for (const child of section.childNodes || []) buildNode(child, bodyEl);
        } else {
          buildNode(section, htmlEl);
        }
      }
    }
  }

  return scriptsToRun;
}

function resolveWptScriptPath(src, casePath, vendorDir) {
  if (src.startsWith("/")) {
    const abs = resolve(vendorDir, src.slice(1));
    const rel = relative(vendorDir, abs);
    assert(!rel.startsWith(".." + sep) && rel !== "..", `Script escapes WPT vendor dir: ${src}`);
    return abs;
  }
  const abs = resolve(vendorDir, dirname(casePath), src);
  const rel = relative(vendorDir, abs);
  assert(!rel.startsWith(".." + sep) && rel !== "..", `Script escapes WPT vendor dir: ${src}`);
  return abs;
}

function extractAnyJsScripts(source, casePath, vendorDir) {
  const result = [];
  // Support WPT `// META: script=...` headers in `.any.js` files.
  for (const line of source.split("\n")) {
    const m = /^\/\/\s*META:\s*script=(.+)$/.exec(line.trim());
    if (!m) {
      if (line.trim() !== "" && !line.trim().startsWith("//")) break;
      continue;
    }
    const dep = m[1].trim();
    const resolved = resolveWptScriptPath(dep, casePath, vendorDir);
    result.push({ filename: dep, code: readFileSync(resolved, "utf8") });
  }
  result.push({ filename: casePath, code: source });
  return result;
}

let cachedTestharnessSource = null;
function getTestharnessSource(testharnessPath) {
  if (testharnessPath === defaultTestharnessPath && cachedTestharnessSource !== null) {
    return cachedTestharnessSource;
  }
  const source = readFileSync(testharnessPath, "utf8");
  if (testharnessPath === defaultTestharnessPath) {
    cachedTestharnessSource = source;
  }
  return source;
}

const TESTHARNESS_STATUS = {
  0: "PASS",
  1: "FAIL",
  2: "TIMEOUT",
  3: "NOTRUN",
  4: "PRECONDITION_FAILED",
};

export async function runWptCase(casePath, {
  source = null,
  vendorDir = defaultVendorDir,
  domDistDir = defaultDist,
  testharnessPath = defaultTestharnessPath,
  timeoutMs = 5_000,
} = {}) {
  const timers = new Set();
  const safeSetTimeout = (fn, delay = 0, ...args) => {
    const handle = setTimeout(() => {
      timers.delete(handle);
      if (typeof fn === "function") fn(...args);
    }, Math.min(Math.max(0, Number(delay) || 0), timeoutMs));
    timers.add(handle);
    return handle;
  };
  const safeClearTimeout = (handle) => {
    timers.delete(handle);
    clearTimeout(handle);
  };

  let completionResolve;
  const completionPromise = new Promise((resolve) => {
    completionResolve = resolve;
  });

  const locationObj = {
    href: `http://web-platform.test:8000/${casePath}`,
    origin: "http://web-platform.test:8000",
    protocol: "http:",
    host: "web-platform.test:8000",
    hostname: "web-platform.test",
    port: "8000",
    pathname: `/${casePath}`,
    search: "",
    hash: "",
    toString() {
      return this.href;
    },
  };

  const context = vm.createContext({
    performance,
    console,
    setTimeout: safeSetTimeout,
    clearTimeout: safeClearTimeout,
    queueMicrotask,
    location: locationObj,
    __squarkWptComplete(tests, harnessStatus) {
      completionResolve({ tests, harnessStatus });
    },
  });

  let passed = 0;
  let failed = 0;
  let subtests = 0;
  const failures = [];

  try {
    const modules = new Map();
    const main = loadDomModule(context, resolve(domDistDir, "index.js"), modules);
    await main.link((specifier, referrer) => {
      const identifier = resolve(referrer.identifier, "..", specifier);
      const rel = relative(domDistDir, identifier);
      assert(!rel.startsWith(".." + sep), "Unexpected module outside squark-dom");
      return loadDomModule(context, identifier, modules);
    });
    await main.evaluate();

    const globalObj = vm.runInContext("globalThis", context);
    const doc = main.namespace.installDOM(globalObj);
    globalObj.location = locationObj;
    // Remaining guarded bridges until squark-dom provides window.parent/top/opener and document.readyState.
    if (globalObj.parent === undefined) globalObj.parent = globalObj;
    if (globalObj.top === undefined) globalObj.top = globalObj;
    if (globalObj.opener === undefined) globalObj.opener = null;
    if (!("readyState" in doc)) {
      Object.defineProperty(doc, "readyState", {
        configurable: true,
        get: () => "loading",
      });
    }

    // Load unmodified upstream WPT resources/testharness.js into the realm.
    const testharnessCode = getTestharnessSource(testharnessPath);
    vm.runInContext(testharnessCode, context, {
      filename: "/resources/testharness.js",
      timeout: timeoutMs,
    });

    // testharnessreport hook: collect subtest + harness status, disable DOM report output.
    vm.runInContext(
      `
      setup({ output: false, explicit_timeout: true });
      add_completion_callback((tests, harnessStatus) => {
        __squarkWptComplete(
          tests.map((t) => ({
            name: t.name,
            status: t.status,
            message: t.message ?? null,
            stack: t.stack ?? null,
          })),
          {
            status: harnessStatus.status,
            message: harnessStatus.message ?? null,
            stack: harnessStatus.stack ?? null,
          },
        );
      });
      `,
      context,
      { filename: "/resources/testharnessreport.js", timeout: timeoutMs },
    );

    const fileSource = source ?? readFileSync(resolve(vendorDir, casePath), "utf8");
    const scriptsToRun = casePath.endsWith(".html")
      ? populateDocumentFromHtml(doc, fileSource, casePath, vendorDir)
      : extractAnyJsScripts(fileSource, casePath, vendorDir);

    for (const { filename, code } of scriptsToRun) {
      vm.runInContext(code, context, { filename, timeout: timeoutMs });
    }

    // Signal document load completion so testharness.js finishes synchronous and async tests.
    Object.defineProperty(doc, "readyState", {
      configurable: true,
      get: () => "complete",
    });
    vm.runInContext(`window.dispatchEvent(new Event("load"));`, context, {
      filename: "wpt-load-event.js",
      timeout: timeoutMs,
    });

    const timeoutToken = Symbol("timeout");
    const outcome = await Promise.race([
      completionPromise,
      new Promise((resolve) => {
        safeSetTimeout(() => resolve(timeoutToken), timeoutMs);
      }),
    ]);

    if (outcome === timeoutToken) {
      failed++;
      failures.push({
        file: casePath,
        subtest: "(harness)",
        status: "TIMEOUT",
        message: `WPT file timed out after ${timeoutMs}ms waiting for testharness completion`,
        stack: "",
      });
    } else {
      const { tests, harnessStatus } = outcome;
      if (harnessStatus.status !== 0) {
        failed++;
        failures.push({
          file: casePath,
          subtest: "(harness)",
          status: TESTHARNESS_STATUS[harnessStatus.status] ?? String(harnessStatus.status),
          message: harnessStatus.message ?? "Harness error",
          stack: harnessStatus.stack ?? "",
        });
      }
      for (const t of tests) {
        subtests++;
        if (t.status === 0) {
          passed++;
        } else {
          failed++;
          failures.push({
            file: casePath,
            subtest: t.name,
            status: TESTHARNESS_STATUS[t.status] ?? String(t.status),
            message: t.message ?? "Subtest failed",
            stack: t.stack ?? "",
          });
        }
      }
      if (subtests === 0 && failed === 0) {
        failed++;
        failures.push({
          file: casePath,
          subtest: "(harness)",
          status: "FAIL",
          message: "No testharness subtests ran",
          stack: "",
        });
      }
    }
  } catch (error) {
    failed++;
    failures.push({
      file: casePath,
      subtest: "(setup)",
      status: "FAIL",
      message: error?.message ?? String(error),
      stack: error?.stack ?? String(error),
    });
  } finally {
    for (const handle of timers) clearTimeout(handle);
    timers.clear();
  }

  return { file: casePath, subtests, passed, failed, failures };
}

function loadExpectedFailures(expectedFailuresPath) {
  if (!expectedFailuresPath || !existsSync(expectedFailuresPath)) return {};
  return JSON.parse(readFileSync(expectedFailuresPath, "utf8"));
}

export async function runWptSuite({
  casesPath = defaultCasesPath,
  expectedFailuresPath = defaultExpectedFailuresPath,
  cases = null,
  filter = null,
  vendorDir = defaultVendorDir,
  domDistDir = defaultDist,
  testharnessPath = defaultTestharnessPath,
  quiet = false,
} = {}) {
  const allCases = cases ?? JSON.parse(readFileSync(casesPath, "utf8"));
  const expectedFailures = loadExpectedFailures(expectedFailuresPath);
  const selected = filter ? allCases.filter((c) => c.includes(filter)) : allCases;

  const files = [];
  const allFailures = [];
  let passed = 0;
  let failed = 0;

  for (const casePath of selected) {
    const res = await runWptCase(casePath, { vendorDir, domDistDir, testharnessPath });
    passed += res.passed;
    failed += res.failed;
    files.push({
      file: res.file,
      subtests: res.subtests,
      passed: res.passed,
      failed: res.failed,
    });
    for (const f of res.failures) {
      const key = `${f.file}::${f.subtest}`;
      const missingFeature = expectedFailures[key] ?? expectedFailures[f.file] ?? null;
      const entry = { ...f, missingFeature };
      allFailures.push(entry);
      if (!quiet) {
        const tag = missingFeature ? ` [missing: ${missingFeature}]` : "";
        console.error(`FAIL ${f.file} / ${f.subtest}${tag}\n${f.message}`);
      }
    }
  }

  const totalSubtests = passed + failed;
  return {
    revision: WPT_REVISION,
    totalFiles: selected.length,
    totalSubtests,
    passed,
    failed,
    passRate: totalSubtests === 0 ? 1 : passed / totalSubtests,
    files,
    failures: allFailures,
  };
}

function parseArgs(argv) {
  const opts = { json: false, out: null, filter: null };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--json") opts.json = true;
    else if (arg === "--out" && argv[i + 1]) opts.out = argv[++i];
    else if (arg === "--filter" && argv[i + 1]) opts.filter = argv[++i];
  }
  return opts;
}

const isMain =
  process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const opts = parseArgs(process.argv.slice(2));
  const report = await runWptSuite({ filter: opts.filter });
  if (opts.out) {
    mkdirSync(dirname(opts.out), { recursive: true });
    writeFileSync(opts.out, JSON.stringify(report, null, 2) + "\n");
  }
  if (opts.json) {
    console.log(JSON.stringify(report, null, 2));
  } else {
    console.log(
      `WPT subset: ${report.totalFiles} files, ${report.passed} passed, ${report.failed} failed`,
    );
  }
  if (report.failed !== 0) process.exitCode = 1;
}
