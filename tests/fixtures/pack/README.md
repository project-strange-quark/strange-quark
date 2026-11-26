<!-- SPDX-License-Identifier: MIT -->
# Golden Pack Fixtures (`tests/fixtures/pack/`)

Shared version 1 `.sqpak`, appended runtime binary, and `document.bin` fixtures
conforming to the pack and initial-document formats in
[`crates/squark-types/src/pack.rs`](../../../crates/squark-types/src/pack.rs).

Both `crates/squark-cli` (writer/inspector tests) and `crates/squark-assets` /
`crates/squark-js` (reader/replay tests) read these files to verify byte-order,
alignment, CRC32, MIME table, UTF-16 string table, and journal batch agreement.

## Contents

| Path | Description |
| --- | --- |
| `basic-site/` | Source web directory (HTML5 standards mode, inline `<style>`, `<link rel="stylesheet">`, inline & external classic/module `<script>`s, non-executable JSON script, comment, non-BMP UTF-16 text, inline `<svg>` with `xlink:href`, `.css`, `.js`, `.js.map` omitted by default, `.png`, `.woff2`, `.json`). |
| `basic.sqpak` | Standalone `.sqpak` archive produced from `basic-site/` with `app_name = "basic-site"`. |
| `basic.document.bin` | Exact `/__squark/document.bin` entry from `basic.sqpak`. |
| `basic.appended.bin` | Synthetic 128-byte runtime prefix (`SQUARK-FAKE-RUNTIME` repeated) padded to 4096 bytes + `basic.sqpak` + 32-byte `SQPACK\0` trailer with valid IEEE CRC32. |
| `basic.inspect.json` | Deterministic JSON report emitted by `squark inspect --dump-pack basic.appended.bin --json`. |
| `quirks.document.bin` | Compiled from `<html><body>quirks</body></html>` (`QuirksMode::Quirks`, flags = `2`). |
| `limited-quirks.document.bin` | Compiled from HTML 4.01 Transitional doctype (`QuirksMode::LimitedQuirks`, flags = `1`). |
| `template.document.bin` | Compiled from HTML containing `<template id="tpl"><div class="inner">hello</div></template>` (`CREATE_NODE` fragment with operand `c = <template> NodeId`, as `crates/squark-types/src/journal.rs` specifies). |
| `custom-mime.sqpak` | Minimal `.sqpak` containing `/data/sample.custom` with `mime_id = 0xFFFF` and custom MIME `"application/vnd.squark.test"`. |

## Regenerating

`crates/squark-cli/tests/pack_integration.rs` verifies every checked-in fixture
byte-for-byte. To regenerate them after an intentional format change:

```bash
SQUARK_UPDATE_GOLDEN=1 cargo test -p squark-cli --test pack_integration
```
