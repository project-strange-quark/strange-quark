# Strange Quark

Run unmodified React (`react-dom`) apps as native desktop binaries — no WebView, no bundled browser.

This package provides the `squark` CLI (backed by a platform-specific native binary package) and the `defineConfig` helper for `squark.config.ts`.

## Requirements

- Node.js `>=22.18.0` (needed for native ESM and unflagged TypeScript config loading).
- Prebuilt binaries are available for macOS arm64/x64, Linux glibc arm64/x64, and Windows arm64/x64.

## Install

```sh
npm install -D strange-quark
```

For Vite live development with `squark dev`, also install the plugin:

```sh
npm install -D @strange-quark/vite-plugin
```

## Build and run

Build the app and package it as a native app:

```sh
npx squark build
```

The app's build command runs automatically. By default, output is under the app's `dist/` directory:

| Platform | Output | Run |
| --- | --- | --- |
| Linux | `dist/my-app` | `./dist/my-app` |
| macOS | Ad-hoc-signed `dist/my-app.app` | `open dist/my-app.app` |
| Windows | `dist/my-app.exe` | `.\dist\my-app.exe` in PowerShell |

Replace `my-app` with the app name. On macOS, you can also run `dist/my-app.app/Contents/MacOS/my-app` directly.

### Distributing your app

A built app contains the Strange Quark runtime, which includes third-party software (V8, Stylo and others) whose licences require their notices to travel with it. Ship them alongside the app: `./dist/my-app --third-party-notices > THIRD_PARTY_NOTICES.txt` writes them on Linux, and the same flag works on the macOS executable (`dist/my-app.app/Contents/MacOS/my-app`) and on Windows (`.\dist\my-app.exe` in PowerShell). The installed `@strange-quark/cli-<os>-<cpu>` package also holds the file as `THIRD_PARTY_NOTICES.txt`.

## Live development

Install `@strange-quark/vite-plugin` and run:

```sh
npx squark dev
```

`squark dev` starts Vite with the plugin and runs the app in the native runtime. Add `@vitejs/plugin-react` to the app's Vite config for React Fast Refresh.

## Configuration

Configure optional window, build, and network settings in `squark.config.ts`:

```ts
import { defineConfig } from "strange-quark";

export default defineConfig({
  name: "my-app",
  window: { title: "My App", width: 1280, height: 800 },
  build: {
    command: "npm run build",
    distDir: "dist",
  },
  network: {
    allow: ["api.example.com"],
  },
});
```

## Troubleshooting optional dependencies

If npm skipped the platform binary because installation used `--omit=optional`, install with optional dependencies enabled:

```sh
npm install --include=optional -D strange-quark
```

Alternatively, install the matching `@strange-quark/cli-<os>-<cpu>` package alongside `strange-quark`; the missing-binary error names the expected package for your platform.

## Environment overrides

- `SQUARK_BIN`: path to a custom `squark` executable.
- `SQUARK_RUNTIME_BIN`: path to a custom `squark-runtime` executable.

## License

MIT — see [LICENSE](./LICENSE).
