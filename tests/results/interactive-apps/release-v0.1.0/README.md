# Release verification for v0.1.0: interactive apps

Every check below was run on 2026-11-25 at commit `756a0d2`, from the history
before this repository was published (it starts from a snapshot without that
history), on this machine:

- **GPUs:** an AMD RX 9060 XT (RADV GFX1200), and lavapipe (llvmpipe, LLVM 23.1.3) from Mesa 26.2.5.
- **Reference browser:** Google Chrome for Testing 159.0.8154.6.

CI was not used; these are local runs.

| Check | Result | Evidence |
| --- | --- | --- |
| 1. `apps/shadcn` passes its interaction script with screenshot diffs | **Pass** on both GPUs. DOM checkpoints 9/9. Screenshots 3/3 within the app screenshot tolerance (at most 0.5% of pixels differing by more than 8 levels): initial 0.371% / 0.367%, dialog 0.369% / 0.369%, after submit 0.479% / 0.474% (lavapipe / RX 9060 XT). | `apps-lavapipe.json`, `apps-rx9060xt.json` |
| 2. A 10,000-row virtual list scrolls at 60 fps (95% of frames meet the commit deadline) | **Pass:** 99.33%, 99.67% and 97.33% of frames on time in three windowed runs on the RX 9060 XT, with commit p95 of 4.0, 4.0 and 6.4 ms. | `fling-4.json` to `fling-6.json` |
| 3. `squark dev` shows an edited component in under 1 s | **Pass:** save to presented frame took 66.5, 43.0 and 45.0 ms, and Fast Refresh kept state. | `dev-loop.txt` |
| 4. IME composition reaches a React input on Wayland | **Pass:** `wayland_ime_commits_to_a_react_controlled_input` passed in a private nested Hyprland, using text-input-v3 with TodoMVC. | `ime.txt` |
| 5. Checks | **Pass:** `scripts/check.sh` passed. `scripts/check-lavapipe.sh` passed, with R1 at 149/150. | `check.txt`, `check-lavapipe.txt` |

## Notes

- **Measuring under load.** Concurrent builds kept the load average between
  15 and 32 on 32 cores. The fling runs in the table ran
  under `systemd-run --user --scope -p CPUWeight=10000`, so the app got the
  CPU share a foreground app has. No other process was paused. Without that
  weight, the four runs scored 98.33%, 84.33%, 99.33% and 94.67%
  (`fling-unweighted.txt`). Frame pacing under heavy CPU contention is still
  open work.
- **The fling script's own pass flag is stricter than the target.**
  `measure-fling.mjs` also requires at least 299 submitted frames, so it
  printed `"pass": false` for runs 4 and 6 even though both met the 95% bar.
  The table judges against the 95% target.
- **Virtual-list screenshots.** The virtual list's DOM matches Chromium, but
  its screenshots don't yet meet the app screenshot tolerance: 0.629% at the
  top and 0.779% after scrolling. Check 2 measures frame timing, not
  screenshots, so this doesn't fail it.
