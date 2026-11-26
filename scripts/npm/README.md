# Cutting an npm release for Strange Quark

This directory contains the release tooling for publishing `strange-quark`, `@strange-quark/vite-plugin`, and the six per-platform native binary packages (`@strange-quark/cli-<os>-<cpu>`).

## Package layout

| Package | Path | Contents |
| --- | --- | --- |
| `strange-quark` | `packages/strange-quark` | `defineConfig` / config loader (`dist/`) + `squark` CLI shim (`dist/cli.js`), with exact-version `optionalDependencies` on all 6 platform packages |
| `@strange-quark/vite-plugin` | `packages/@strange-quark/vite-plugin` | Vite plugin and `squark dev` launcher (`dist/`) |
| `@strange-quark/cli-darwin-arm64` | `npm/cli-darwin-arm64` | `bin/squark`, `bin/squark-runtime` (`aarch64-apple-darwin`) |
| `@strange-quark/cli-darwin-x64` | `npm/cli-darwin-x64` | `bin/squark`, `bin/squark-runtime` (`x86_64-apple-darwin`) |
| `@strange-quark/cli-linux-x64` | `npm/cli-linux-x64` | `bin/squark`, `bin/squark-runtime` (`x86_64-unknown-linux-gnu`, `libc: ["glibc"]`) |
| `@strange-quark/cli-linux-arm64` | `npm/cli-linux-arm64` | `bin/squark`, `bin/squark-runtime` (`aarch64-unknown-linux-gnu`, `libc: ["glibc"]`) |
| `@strange-quark/cli-win32-x64` | `npm/cli-win32-x64` | `bin/squark.exe`, `bin/squark-runtime.exe` (`x86_64-pc-windows-msvc`) |
| `@strange-quark/cli-win32-arm64` | `npm/cli-win32-arm64` | `bin/squark.exe`, `bin/squark-runtime.exe` (`aarch64-pc-windows-msvc`) |

Platform package templates live under `npm/` (outside the root `workspaces` globs) so `npm ci` at the monorepo root does not try to resolve `os`/`cpu`-filtered binary packages. Likewise, `strange-quark`'s `optionalDependencies` on the 6 platform packages are injected into the staged `package.json` by `scripts/npm/stage.mjs` (into `target/npm/stage/`) rather than committed in `packages/strange-quark/package.json`, so `npm ci` in a clean checkout succeeds before a new version is published on npm.

## Minimum Node.js version for users (`>=22.18.0`)

Published packages declare `"engines": { "node": ">=22.18.0" }` (whereas developing the monorepo itself uses Node 26):

- `squark build` and `strange-quark`'s `resolveConfig()` import `squark.config.ts` directly with Node's native TypeScript type stripping and no extra flags; unflagged type stripping landed in Node 22.18.0 (LTS) and 23.6.0+.
- Every peer-supported Vite major (Vite 6, and Vite 7 and 8, which require `^20.19.0 || >=22.12.0`) works on Node `>=22.18.0`.
- The compiled TypeScript output targets `ES2024`, fully supported by Node 22+.

## Release workflow

`.github/workflows/release.yml` runs the steps below in CI. Pushing a `v*`
tag that matches the packages' version builds all six targets, smoke-tests
the npm install on each runner that can run its own build, stages and
verifies all eight packages, and publishes them with provenance (platform
packages first). A prerelease version (one with a `-`, such as
`0.2.0-rc.1`) is published under the `next` dist-tag instead of `latest`. A manual run of the workflow does everything except
publish unless its `publish` input is ticked. Publishing needs the
`NPM_TOKEN` repository secret (an npm automation token with access to
`strange-quark` and the `@strange-quark` scope); `scripts/npm/collect-artifacts.mjs`
puts each runner's binaries where `stage.mjs` looks for them.

The manual steps, for a local release or to debug the workflow:

### 1. Build matrix

Build the stripped `dist` binaries for each target triple and collect them under `target/<triple>/dist/`:

```sh
# On each platform runner (or cross-compiled):
cargo build --profile dist --target <triple> -p squark-cli --bin squark -p squark-host --bin squark-runtime
```

Build the TypeScript packages:

```sh
npm run build --workspace packages/strange-quark --workspace packages/@strange-quark/vite-plugin
```

### 2. Sync version and stage packages

Set the release version across all 8 package templates and internal workspace pins (such as `tests/dev/package.json`), then stage the publish-ready packages and built binaries into `target/npm/stage/`:

```sh
node scripts/npm/set-version.mjs 0.1.0
node scripts/npm/stage.mjs --require-all
```

For local host-only testing (when only `target/dist/` exists for the current machine), omit `--require-all`:

```sh
cargo build --profile dist -p squark-cli --bin squark -p squark-host --bin squark-runtime
node scripts/npm/stage.mjs
```

`stage.mjs` writes publish-ready copies under `target/npm/stage/` (injecting the exact-version `optionalDependencies` into `target/npm/stage/strange-quark/package.json`) and writes a JSON pack manifest (`target/npm/pack-manifest.json`) recording each package's version, expected file list, and binary sizes/SHA-256 digests.

### 3. Verify tarballs and run the local install smoke test

Verify every staged manifest and `npm pack --dry-run --json` file list (pass `--require-all` in release CI):

```sh
node scripts/npm/verify.mjs --require-all   # or without --require-all for host-only
```

Run the end-to-end tarball install smoke test on the host platform:

```sh
node scripts/npm/smoke-test.mjs
```

This packs the staged host platform package, `strange-quark`, and `@strange-quark/vite-plugin`, installs the tarballs into an isolated temp project, and runs `npx --no-install squark --help` and `npx --no-install squark build --skip-app-build` with `SQUARK_BIN` / `SQUARK_RUNTIME_BIN` unset and a clean `PATH`.

### 4. Publish (platform packages first)

Ensure the `@strange-quark` npm organization/scope exists and you are authenticated (`npm whoami`). Always publish the 6 staged platform packages **before** publishing `strange-quark`, so users who install `strange-quark` immediately resolve its `optionalDependencies`:

```sh
# 1. Publish all 6 platform packages first:
for dir in target/npm/stage/cli-*; do
  (cd "$dir" && npm publish --provenance --access public)
done

# 2. Publish @strange-quark/vite-plugin and strange-quark:
(cd target/npm/stage/vite-plugin && npm publish --provenance --access public)
(cd target/npm/stage/strange-quark && npm publish --provenance)
```
