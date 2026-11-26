# @strange-quark/vite-plugin

Vite dev-server plugin and launcher used by `squark dev` to run React apps with HMR and React Fast Refresh inside the native `strange-quark` runtime.

## Requirements

- Node.js `>=22.18.0`
- Vite `^6.0.0 || ^7.0.0`
- `strange-quark` and a supported platform binary package

## Install

```sh
npm install -D strange-quark @strange-quark/vite-plugin
```

## Develop and build

`squark dev` starts Vite with this plugin and launches the app in the native runtime:

```sh
npx squark dev
```

For React Fast Refresh, include the standard React plugin in the app's `vite.config.ts`:

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { strangeQuark } from "@strange-quark/vite-plugin";

export default defineConfig({
  plugins: [react(), strangeQuark()],
});
```

To make a standalone app, run `npx squark build`; the app's configured build command runs automatically. The output is under `dist/`:

| Platform | Output | Run |
| --- | --- | --- |
| Linux | `dist/my-app` | `./dist/my-app` |
| macOS | Ad-hoc-signed `dist/my-app.app` | `open dist/my-app.app` |
| Windows | `dist/my-app.exe` | `.\dist\my-app.exe` in PowerShell |

Replace `my-app` with the app name. macOS can also run `dist/my-app.app/Contents/MacOS/my-app` directly.

If npm skipped the native binary package with `--omit=optional`, restore it with `npm install --include=optional -D strange-quark` (and install this plugin as shown above). The `strange-quark` missing-binary error names the platform package if you need to install it explicitly.

## License

MIT — see [LICENSE](./LICENSE).
