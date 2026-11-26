# Release verification for v0.1.0: Excalidraw

Every check below was run on commit `9b24112` on 2026-11-26.

- **GPUs:** AMD RX 9060 XT (RADV GFX1200), and lavapipe (llvmpipe, LLVM 23.1.3) from Mesa 26.2.5.
- **Reference browser:** Google Chrome for Testing 159.0.8154.6.
- **Load:** the 1-minute load average was 6–14 during the timing runs.
- **CI:** not used; these are local runs.

| # | Check | Result | Evidence |
| --- | --- | --- | --- |
| 1 | No patches: `vendor/excalidraw` is unmodified v0.18.1 (`a2ec2889`), built with env and config only, collaboration off | pass | `packaging.json` |
| 2 | One binary: `dist/excalidraw`, a single stripped x86_64 ELF, 95.8 MiB, 12 system libraries | pass; a copy alone in an empty directory mounts the app | `packaging.json` |
| 3 | Startup under 1.5 s, median of 5, windowed, RX 9060 XT | **87.5 ms** (86.8 to 169 ms) | `startup.json` |
| 4 | The end-to-end script on the native binary, with DOM state matching Chromium at every step | headless on lavapipe: 20/20 steps, 20/20 checkpoints. Windowed on RADV: 20/20 steps, 20/20 checkpoints. Chromium: 20/20 | `e2e-headless-lavapipe.json`, `e2e-windowed.json` |
| 5 | The 10 canonical scenes against Chromium, at most 0.5% of pixels differing by more than 8 levels | RX 9060 XT: 10/10, worst 0.458%. lavapipe: 10/10, worst 0.456% | `scenes-auto.json`, `scenes-software.json` |
| 6 | A 1,000-element pan at 60 fps (95% of frames on time) | 300/300 in each of 3 runs, all valid: 300 wheels counted in the host, the page and the canvas, and the full scripted scroll | `pan.json`, `pan-runs.txt` |
| 7 | `scripts/check.sh` and `scripts/check-lavapipe.sh` | both passed (R1 149/150) | `check.txt`, `check-lavapipe.txt` |

`summary.json` is `tests/excalidraw/src/summary.mjs`'s report for this run:
verdict `passing`, blockers `[]`.

**How the timing was measured.**
- Startup and the windowed e2e ran in a regular desktop session.
- The pan ran in a private nested Hyprland with a fixed 1280×800 headless
  output, which `scripts/check-excalidraw-pan.sh` sets up.
- Every timing ran under `systemd-run --user --scope -p CPUWeight=10000`.
- A pan run counts only when it is valid: the host, the page and the canvas
  each count exactly the 300 scripted wheel events, the viewport keeps one
  size, and the scene ends at the scripted scroll. The rules are in
  `crates/squark-host/tests/fixtures/excalidraw-pan-driver.mjs`.
