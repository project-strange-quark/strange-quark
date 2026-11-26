<!-- SPDX-License-Identifier: MIT -->
# DOM Web Platform Tests in Node (`@strange-quark/wpt`)

The files in `vendor/` (including `vendor/resources/testharness.js`) are
unmodified upstream WPT files pinned at
[f4d3f2eba09697ebcb2f338b8f2d709a3ce85217](https://github.com/web-platform-tests/wpt/tree/f4d3f2eba09697ebcb2f338b8f2d709a3ce85217).
The [BSD 3-Clause licence](vendor/LICENSE.md) applies to those files.
[cases.json](cases.json) lists all entry points in the subset, and
[expected-failures.json](expected-failures.json) maps any known failing file or
`file::subtest` to its missing feature for test reports.

## Canvas 2D subset

[canvas/](canvas/) imports the Canvas 2D subset that covers the Canvas 2D
methods Excalidraw uses, at the same pinned revision, in `cases.json`
(66 files, Chromium-verified). It runs in the same `npm test`:
Chromium is the reference, and `--engine native` runs the same files in the
packed runtime. See
[canvas/README.md](canvas/README.md).

## How it works

1. Each WPT file runs in an isolated Node `vm.SourceTextModule` realm with
   `installDOM` loaded from `packages/@strange-quark/dom/dist/`.
2. Unmodified upstream `vendor/resources/testharness.js` is evaluated inside
   the realm, paired with a `testharnessreport` hook (`add_completion_callback`)
   that collects per-subtest results.
3. For `.html` files, authored DOM fixtures are parsed with `parse5` (MIT,
   test-only) and constructed through `squark-dom`'s own DOM API (`createElement`,
   `createElementNS`, `setAttribute`, `createTextNode`, `createComment`,
   `appendChild`), then `<script>` tags execute in document order and `window`
   receives a `load` event.

## Running

```bash
# Run via workspace tests (requires @strange-quark/dom built)
npm test --workspace @strange-quark/wpt

# Run directly (also invoked by @strange-quark/dom's npm test)
node --experimental-vm-modules tests/wpt/run.mjs

# Emit machine-readable JSON report or filter cases
node --experimental-vm-modules tests/wpt/run.mjs --json --out tests/results/wpt/summary.json
node --experimental-vm-modules tests/wpt/run.mjs --filter Event-constructors
```
