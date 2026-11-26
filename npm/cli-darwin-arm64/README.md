# @strange-quark/cli-darwin-arm64

Prebuilt native `squark` CLI and `squark-runtime` binaries for **macOS arm64 (Apple Silicon)**. This platform package is normally installed automatically by `strange-quark`.

## Requirements

- Node.js `>=22.18.0`.
- macOS arm64 (Apple Silicon).

## Install

Install the user-facing CLI:

```sh
npm install -D strange-quark
```

Build and run an app:

```sh
npx squark build
open dist/my-app.app
```

`squark build` runs the app's build command and writes an ad-hoc-signed `dist/my-app.app`; replace `my-app` with the app name. You can also run `dist/my-app.app/Contents/MacOS/my-app` directly.

For Vite live development, install `@strange-quark/vite-plugin` and run `npx squark dev`.

If npm skipped this platform package with `--omit=optional`, restore optional dependencies with `npm install --include=optional -D strange-quark`, or install both packages explicitly:

```sh
npm install -D strange-quark @strange-quark/cli-darwin-arm64
```

## License

MIT — see [LICENSE](./LICENSE).
