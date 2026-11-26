# Measuring fidelity and performance

`scripts/measure.mjs` runs the same core flows in headless Chromium and in
the Strange Quark binary. It compares what both engines show, and times what
both engines do. This file covers how to write the flows, how to run the
script, and how to read what it reports.

## Setup (once per machine)

The script needs `playwright-core` (to drive a Chromium that is already
installed; it downloads no browser) and `pngjs`. Install them into a
throwaway directory, never into the user's project:

```sh
npm install --prefix ~/.cache/strange-quark-migrate playwright-core@1 pngjs@7
```

It finds Chromium through `$SQUARK_CHROMIUM_PATH`, then `/usr/bin/chromium`,
then `chromium` or `google-chrome` on `PATH`.

## Writing flows

`flows.json` holds the core flows from `INVENTORY.md`, as steps. Write
them before you look at results, and keep them identical in both engines.
They exist to compare, not to pass.

```json
{
  "viewport": { "width": 1280, "height": 800 },
  "root": "#root",
  "flows": [
    {
      "name": "add-and-complete-a-todo",
      "core": true,
      "steps": [
        { "checkpoint": "empty" },
        { "click": "input.new-todo" },
        { "type": "Buy milk" },
        { "press": "Enter" },
        { "checkpoint": "after-add" },
        { "click": "li:first-child input.toggle" },
        { "checkpoint": "after-complete" }
      ]
    }
  ]
}
```

These steps are available:

| Step | What it does |
| --- | --- |
| `{ "click": "<selector>" }` | Clicks the centre of the first matching element |
| `{ "dblclick": "<selector>" }` | Double-clicks it |
| `{ "type": "text" }` | Types into the focused element, key by key |
| `{ "press": "Enter" }` | Presses one key. Modifiers go `Control+a`, `Shift+Tab` |
| `{ "wheel": { "selector": "...", "dy": 400 } }` | Scrolls over an element |
| `{ "wait": 250 }` | Waits that many milliseconds. Use it sparingly: every step already waits for the next idle frame |
| `{ "checkpoint": "name" }` | Records the DOM state under `root` and a screenshot |

A flow that touches a stub must be marked `"stub": true`. It is still run
and reported, but never counted as passing.

## Running

```sh
node <skill-dir>/scripts/measure.mjs \
  --dist dist \
  --squark-binary dist/my-app \
  --flows strange-quark-migration/flows.json \
  --out strange-quark-migration/results
```

`--dist` is the app's static build output. The script serves it on
localhost for Chromium, with a fallback to `index.html` for client routes.
Use `--skip-squark` to measure only the browser baseline, for example when
Strange Quark cannot build the app yet. Each run gets a fresh browser profile
and a fresh Strange Quark data directory (`XDG_DATA_HOME`), so state saved by
one run cannot leak into the next.

## What it reports

`results/measure.json` and a printed summary contain:

- **DOM state per checkpoint.** A normalized serialization of the tree
  under `root`: tags, ids, classes, text, form values and checked states.
  It must match exactly. A mismatch is a real behaviour difference, not
  noise.
- **Screenshot per checkpoint.** It passes when no more than 0.5% of pixels
  differ by more than 8 levels in any colour channel. That is Strange Quark's
  own app tolerance. Its renderer uses a closed-form
  shadow and analytic antialiasing, so edges and shadows differ by a few
  levels by design. A failure is a visible difference: look at the diff
  image in `results/diff/` before describing it.
- **Time to first frame**, from launch: from starting the engine's process
  to the first frame after the root has children.
  - *Chromium*: launching the browser, navigating, and the first two frames
    after the app mounts.
  - *Strange Quark*: spawning the binary, until the first idle frame with the
    root mounted.

  Both include process start and loading the app's scripts, which is what
  launching a desktop app costs. Neither includes building it. Report it
  that way, since it is not navigation timing.
- **Memory.** Resident set size after the flows, summed over each engine's
  process tree. For Chromium that is the whole browser: the browser, GPU,
  renderer and utility processes. That is a fair stand-in for what an
  Electron app's Chromium costs. For Strange Quark it is one process.
- **Input to frame** (the main responsiveness number). It is measured
  inside the page, with the same code in both engines: from an input
  event's `timeStamp` to the first animation frame after the DOM changed in
  response. Harness overhead does not count. Report p50 and p95, with the
  sample count.
- **Harness step time.** The wall-clock time from sending a step's input
  until the engine is idle again. It includes each driver's own idle wait,
  which differs between engines, so it is a sanity check, not a
  comparison. Never present it as the engines' speed.
- **Binary size** of the file given as `--squark-binary`. If that is a wrapper
  script, report the real binary's size from `stat` instead. Measure a release build;
  a debug build is several times larger and says nothing about shipping
  size.

## Honesty in numbers

- Report medians of at least 3 runs for timings and memory, and say how
  many you ran.
- Say where you measured: GPU or software adapter, headless or windowed,
  the machine.
- A number you did not measure is written "not measured", never estimated.
- If Strange Quark crashes or a step times out, that is the result. Report it,
  with the error text.
