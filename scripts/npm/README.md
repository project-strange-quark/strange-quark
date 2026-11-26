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
tag that matches the packages' version builds all six targets, checks that
the Linux binaries need glibc 2.35 at most, smoke-tests the npm install on
each runner that can run its own build, checks that `THIRD_PARTY_NOTICES.txt`
is current, stages and verifies all eight packages, and publishes them with
provenance through `scripts/npm/publish.mjs`. That script publishes the
platform packages first, skips any version npm already has, so a release that
failed part-way can run again, and sends a prerelease version (one with a
`-`, such as `0.2.0-rc.1`) to the `next` dist-tag instead of `latest`. A
manual run of the workflow does everything except publish, ending with
`npm publish --dry-run`, unless its `publish` input is ticked.
`scripts/npm/collect-artifacts.mjs` puts each runner's binaries where
`stage.mjs` looks for them.

The repository must be public when the workflow publishes: npm records
provenance only for public source repositories.

### npm credentials

npm can set up trusted publishing (OIDC, no token) only for a package that
already exists, so the first release uses a token and later ones do not:

1. Create the `strange-quark` npm organization, which owns the
   `@strange-quark` scope.
2. Create a granular access token with read and write access to all
   packages and the `@strange-quark` scope, allowed to publish without
   two-factor prompts, and store it as the `NPM_TOKEN` repository secret.
   npm caps a write token's lifetime, so give it a short one.
3. Push the first `v*` tag.
4. On npmjs.com, add a trusted publisher to each of the eight packages: this
   repository and the `release.yml` workflow.
5. Delete the `NPM_TOKEN` secret and revoke the token. Later releases
   authenticate through the workflow's OIDC token (`id-token: write`).

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

Make sure you are authenticated (`npm whoami`), then publish the staged
packages. The script publishes the six platform packages before
`@strange-quark/vite-plugin` and `strange-quark`, so `strange-quark`'s
`optionalDependencies` resolve as soon as it appears, and it skips any
version npm already has:

```sh
node scripts/npm/publish.mjs --dry-run --no-provenance   # check first
node scripts/npm/publish.mjs --no-provenance             # provenance needs CI
```
