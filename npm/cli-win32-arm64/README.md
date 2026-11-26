# @strange-quark/cli-win32-arm64

Prebuilt native `squark` CLI and `squark-runtime` binaries for **Windows arm64**. This platform package is normally installed automatically by `strange-quark`.

## Requirements

- Node.js `>=22.18.0`.
- Windows arm64.

## Install

Install the user-facing CLI:

```sh
npm install -D strange-quark
```

Build and run an app:

```powershell
npx squark build
.\dist\my-app.exe
```

`squark build` writes `dist/my-app.exe`; replace `my-app` with the app name.

For Vite live development, install `@strange-quark/vite-plugin` and run `npx squark dev`.

If npm skipped this platform package with `--omit=optional`, restore optional dependencies with `npm install --include=optional -D strange-quark`, or install both packages explicitly:

```sh
npm install -D strange-quark @strange-quark/cli-win32-arm64
```

## License

MIT — see [LICENSE](./LICENSE). The binaries include third-party software;
[THIRD_PARTY_NOTICES.txt](./THIRD_PARTY_NOTICES.txt) lists it with its licences.
