# Release verification for v0.1.0: Excalidraw's platform

Every check below was run on commit `b4d0969` on 2026-11-26.

- **GPUs:** AMD RX 9060 XT (RADV GFX1200), and lavapipe (llvmpipe, LLVM 23.1.3) from Mesa 26.2.5.
- **Reference browser:** Google Chrome for Testing 159.0.8154.6.
- **CI:** not used; these are local runs.

`summary.json` is `tests/platform-report/src/summary.mjs`'s output for this run. It records
per-GPU results in `canvasWpt.nativeByAdapter`, `r3ByAdapter` and
`excalidrawByAdapter`.

| Check | lavapipe | RX 9060 XT |
| --- | --- | --- |
| 1. Canvas 2D WPT subset, native (Chromium passes 66/66) | 66/66 | 66/66 |
| 2. R3 canvas reftests against Chromium (rough.js, perfect-freehand, text, dashes, transforms, clips, drawImage, dark-mode filter) | 15/15, 0 render errors | 15/15, 0 render errors |
| 3. Excalidraw's Web APIs, each with a test | 50/50 APIs and 15/15 Canvas 2D methods pass | same tests |
| 4. `scripts/check.sh` and `scripts/check-lavapipe.sh` | both passed (R1 149/150) | (`check.sh` runs on the default adapter) |

The Excalidraw native smoke test passed 4/4 on both GPUs.

**Deliberately undefined APIs.** Seven of the Web APIs Excalidraw uses are
features Strange Quark deliberately leaves undefined:
- ShadowRoot and visualViewport;
- `navigator.share` and the service worker;
- the File System Access pickers (file open and save use `<input type=file>`
  and `a[download]` instead);
- Worker and OffscreenCanvas.

Each of them passes only through a test of Excalidraw's own fallback path.
