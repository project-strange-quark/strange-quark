# Working on Strange Quark

These rules apply to everyone who changes this repository, people and
agents alike.

## What the project is

Strange Quark runs unmodified React apps, react-dom included, as native apps
with no WebView. v0.1.0 turns upstream **Excalidraw v0.18.1** into a single
native Linux binary without changing a line of Excalidraw's source.

[docs/architecture.md](docs/architecture.md) is the design. Follow it. Where
it turns out to be wrong or silent, don't decide alone: write down the
problem and the options, and ask the maintainer.

The supported scope is the desktop: Linux (Wayland and X11, Vulkan), macOS
12+ (Metal) and Windows 10+ (Direct3D 12), in Compat mode (react-dom on
squark-dom) on V8. Direct mode, QuickJS-ng, AccessKit, mobile, embedded and the
web target are out of scope until the project owner says otherwise.

**Coordination notes live outside the repository.** Task briefs, the status
log, the internal contract between areas, the per-area design notes, the
decision log and the Excalidraw Web API audit are kept by the maintainer
outside this repository. Ask the maintainer when a task needs them.

## Areas

Each area owns a set of paths. Work in your own area.

| Area | Owns |
| --- | --- |
| dom | `packages/@strange-quark/dom` (the JavaScript DOM, events, CSSOM, observers, timers, History and every Web API facade, including Canvas 2D's recorder, FontFace, storage, crypto, clipboard and files, and the journal encoder); `crates/squark-js` (the V8 embedding, module loader, event loop, `__squark` bindings, structuredClone); `crates/squark-journal`; `crates/squark-net` |
| engine | `crates/squark-mirror`, `crates/squark-style`, `crates/squark-layout`, `crates/squark-text`, `crates/squark-assets`, and code forked from Blitz |
| gpu | `crates/squark-paint`, `crates/squark-compositor`, `crates/squark-gpu` (with `shaders/*.wgsl`), `crates/squark-canvas`, `crates/squark-host` (with the `squark-runtime` binary that wires the areas together) |
| tools | `crates/squark-cli`, `packages/strange-quark`, `packages/@strange-quark/vite-plugin`, `npm/` (the per-platform binary packages), `tests/`, `apps/`, `vendor/`, `.github/`, `scripts/`, `deny.toml` |
| maintainer | `README.md`, `CONTRIBUTING.md`, `LICENSE`, `NOTICE`, `AGENTS.md`, `docs/`, `skills/`, the root `Cargo.toml` and `package.json`, `rust-toolchain.toml` |

**Shared types.** `crates/squark-types` holds one module per seam between
areas. A module's owner may change it when the area on the other side of
the seam agrees, and the commit body names that area.

| Module | Owner | Must agree |
| --- | --- | --- |
| `journal` | dom | engine |
| `fragment` | engine | gpu |
| `engine` | engine | dom |
| `render` | gpu | engine, dom |
| `host` | gpu | dom |
| `pack` | tools | engine |
| `id`, `geometry`, `scroll`, and the rest of the crate | maintainer | — |

**Wiring.** Outside your area, make only wiring changes: adding your crate
to `[workspace.dependencies]`, one call site, or one line in a list. Put
each in its own small commit and mention it. Anything larger is a request
to the area's owner, through the maintainer.

**Blockers.** When another area's code blocks you, don't work around it
inside that area. Build against a stub in your own area, and say exactly
what you need.

## Languages and code

- **Rust** for crates: edition 2024, the toolchain pinned in
  `rust-toolchain.toml`, and `[lints] workspace = true` in every crate.
- **`unsafe`** is denied across the workspace. Only `squark-js` (V8) opts in
  at the crate root. `squark-mirror` opts in only in the module that
  implements Stylo's DOM traits, and `squark-host` only in the Windows
  named-pipe transport of its test-control channel (Win32 calls std does
  not wrap). Every `unsafe` block or `unsafe fn` impl carries a
  `// SAFETY:` comment. Prefer a maintained safe crate, `bytemuck` and safe
  wgpu APIs; a new opt-in needs the maintainer's agreement.
- **TypeScript** (strict) for packages, shipped as ESM. squark-dom must run
  unbundled both in Node, for its own tests, and in squark-js.
- **No new C or C++** beyond what a crate's build already pulls in, such as
  V8's prebuilt library.

## Licences

The project is MIT. Every new source file starts with
`SPDX-License-Identifier: MIT`.

Dependencies may come in only under MIT, Apache-2.0, BSD-2/3-Clause, ISC,
Zlib, Unicode-3.0, CC0 or 0BSD, or MPL-2.0 as a dependency (Stylo and its
crates). A patched MPL-2.0 file stays under MPL-2.0 and in its original
place, and the patch is recorded, as `vendor/stylo/PATCHES.md` and `NOTICE`
do for Stylo. Nothing under the GPL, LGPL or AGPL goes into a crate, a
package or a build output; a GPL tool that is only run is fine.

Code copied or forked into the repository keeps its original copyright
header and is recorded in `NOTICE` with its licence and exact commit.
`deny.toml` (cargo-deny) and the npm licence check in `scripts/` enforce
these rules.

## Compatibility

- **Apps stay unmodified.** Do not fork, patch or alias React, react-dom or
  any app's source, including `vendor/excalidraw`. Making them work is the
  DOM's job.
- **Feature detection tells the truth.** Do not define an API Strange Quark
  does not implement. Where a stub is unavoidable, it must behave like a
  browser that lacks the feature, so the app's fallback path runs.
- **Chromium is the reference.** Behaviour and pixels are compared against
  the pinned Chromium (`scripts/chromium-path.mjs`). A deliberate difference
  is the maintainer's call.

## Building and testing

- `cargo build --workspace` and `cargo test --workspace` build and test all
  Rust. Tests must pass with no window system. Tests that need a GPU or a
  window are `#[ignore]`d or gated on `SQUARK_GPU_TESTS=1` or
  `SQUARK_WINDOW_TESTS=1`.
- `npm install` and `npm test` at the root install and test every package.
- `cargo run -p squark-cli -- <command>` runs the `squark` CLI: `dev`, `build`,
  `pack`, `screenshot` and `inspect`.
- `scripts/check.sh` runs everything a push must pass; it wraps
  `node scripts/check.mjs`, which Windows runs directly.
  `scripts/check-lavapipe.sh` runs the GPU tests on Mesa's software adapter.
- On macOS, `squark build` writes an app bundle, `dist/<name>.app`: the
  runtime in `Contents/MacOS`, the pack in `Contents/Resources/app.sqpak`,
  and an ad-hoc signature over the whole bundle. Linux and Windows get one
  binary with the pack appended.
- `scripts/npm/README.md` says how the npm packages are staged, checked
  and released.
- Build output goes under `target/`, `build/` or `dist/`, never into git.

## Working rules

- **Tests with every change.** Each change comes with the test that shows
  it works: a unit or property test, a WPT case, a reftest against Chromium,
  a react-dom suite case or an app interaction script.
- **Timeouts.** Wrap anything that may not end in a timeout (`timeout`,
  or `node scripts/with-timeout.mjs <seconds> <command>` where there is no
  `timeout`, as on macOS and Windows): windows, Chromium, dev servers, GPU
  tests and watchers. Prefer headless runs, keep
  windows short-lived, and leave no process running when you stop.
- **No root.** Install tools for your user only.
- **Commits.** The message is `area: lowercase imperative summary`, where
  the area is `dom`, `engine`, `gpu`, `tools` or `docs`, followed by a body
  that says why. One topic per commit. **No `Co-Authored-By`,
  `Generated-with`, `Assisted-by` or similar trailers.**
- **Pushing.** Before pushing, `git pull --rebase` and run the checks your
  change touches. Never force-push, rewrite pushed history, or delete
  branches or tags.
- **Reporting.** End each piece of work with a short summary: the commits,
  the evidence (test output or screenshot paths), what comes next, and
  anything blocked or needing a decision.
