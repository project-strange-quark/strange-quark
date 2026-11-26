# @strange-quark/cli-linux-x64

Prebuilt native `squark` CLI and `squark-runtime` binaries for **Linux x64 (glibc)**. This platform package is normally installed automatically by `strange-quark`.

## Requirements

- Node.js `>=22.18.0`.
- Linux x64 with glibc.

## Install

Install the user-facing CLI:

```sh
npm install -D strange-quark
```

Build and run an app:

```sh
npx squark build
./dist/my-app
```

`squark build` runs the app's build command and writes `dist/my-app`; replace `my-app` with the app name.

For Vite live development, install `@strange-quark/vite-plugin` and run `npx squark dev`.

If npm skipped this platform package with `--omit=optional`, restore optional dependencies with `npm install --include=optional -D strange-quark`, or install both packages explicitly:

```sh
npm install -D strange-quark @strange-quark/cli-linux-x64
```

## License

MIT — see [LICENSE](./LICENSE). The binaries include third-party software;
[THIRD_PARTY_NOTICES.txt](./THIRD_PARTY_NOTICES.txt) lists it with its licences.
