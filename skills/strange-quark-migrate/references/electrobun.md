# Electrobun (React view) to Strange Quark

An Electrobun app has a Bun main process (`src/bun/`) and one or more web
views, often React (`src/mainview/` or similar). They talk through typed RPC
(`Electroview`, `BrowserView.defineRPC`). Treat it like Electron:

1. **Find the seam.** Read the RPC schema type (`requests` and
   `messages`), every `electroview.rpc.request.*` and `rpc.send.*` call in
   the view, and the Bun handlers behind them.
2. **Build the view alone** with Vite. Electrobun's config points at the
   view's entry, so reuse that entry. Check that `index.html` does not
   depend on Electrobun's `views://` protocol for asset paths.
3. **Replace each RPC call** with a web equivalent from the Electron table
   in `electron.md` (file dialogs, the clipboard, storage, `fetch`), or
   **stub** it. Bun-side file-system access, `Bun.spawn`, SQLite through
   `bun:sqlite`, tray, menus and `Updater` have no equivalent. Stubs go in
   `src/strange-quark-stubs/electrobun-rpc.ts`, with the RPC schema's types,
   and each starts with `// SQUARK-RENDER STUB:`.
4. **Several `BrowserView`s or windows:** Strange Quark's first release has one
   window. If the app's core needs more, that counts toward partially
   viable or blocked.
5. **Measure the original too,** when you can (`bunx electrobun build`,
   then run it). On Linux, Electrobun uses the system WebKitGTK unless it
   bundles CEF, so record which one. Compare startup, memory and package
   size.
