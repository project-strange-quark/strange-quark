---
name: strange-quark-migrate
description: Migrates a React codebase that Strange Quark cannot run as-is — a React meta-framework app (Next.js, TanStack Start, Remix, React Router framework mode) or a desktop app whose UI is React inside Electron, Tauri or Electrobun — onto a separate git branch as a plain client-rendered React app that Strange Quark can build, then measures it against the same app in Chromium (rendering fidelity and performance) and writes an honest verdict, viable, partially viable or blocked, with the evidence behind it. Use this skill whenever someone wants to run an existing React project on Strange Quark, asks whether their Next.js, Electron, Tauri or Electrobun app could become a Strange Quark native app, wants a Strange Quark compatibility assessment or migration plan, or asks to port, convert or move a React frontend off a meta-framework or desktop shell for Strange Quark — even when they only ask "would this work on Strange Quark?".
---

# Migrating a React codebase to Strange Quark

Strange Quark runs client-rendered React apps (React 18.3 or 19) as native
binaries with no WebView. It does not run what React meta-frameworks add on
top of React: server rendering, React Server Components, server actions and
functions, API routes, middleware. Nor does it run the backend half of a
desktop shell: Electron's main process, Tauri's Rust commands, Electrobun's
Bun side. Those apps need a migration before Strange Quark can build them, and
the honest answer to "will it work?" depends on how much of the app lives in
those parts.

This skill does four things, in order:

1. **Inventory** what the app is and what it depends on.
2. **Migrate** it, on its own branch, to a client-rendered React app.
3. **Measure** it in Chromium and in Strange Quark, with the same scripted flows.
4. **Verdict:** viable, partially viable or blocked, with evidence.

The person reading the report will decide whether to invest in Strange Quark
based on it. A hopeful report that turns out wrong costs them far more than
a blunt one. Write it the way you would want it if your own project were on
the line.

## Ground rules

- **Work on a branch, never on theirs.** Require a clean working tree. If it
  is dirty, stop and ask; never stash someone's work silently. Create
  `strange-quark-migration` from the current HEAD, or `strange-quark-migration-2`
  and so on if that name exists. Commit there in small steps with clear
  messages. Never commit to the original branch, never push, and never
  rewrite history. The branch is a proposal. What happens to it is the
  user's call. If the project is not a git repository, ask before running
  `git init`. If you cannot ask, initialize it and commit the original
  state as the first commit, so the migration can still be compared with
  it.
- **Change only what Strange Quark needs.** Keep components, styles and
  behaviour. Every change goes into `strange-quark-migration/MIGRATION.md`, with
  its reason and one of three categories:
  - *mechanical*: same behaviour, different plumbing, such as `next/link`
    becoming a router `Link`;
  - *behavioural*: something works differently now, such as data fetched on
    the client instead of rendered on the server;
  - *stub*: functionality removed and replaced with a placeholder.
- **Prefer a real web equivalent to a stub.** The first-release target has
  web APIs for file pickers, downloads, the clipboard, `localStorage`,
  IndexedDB and opening a URL in the browser. Check the installed-runtime
  probe before relying on any of them.
- **Stub, and flag, everything with no equivalent.** That means Node or Rust
  backend calls, IPC, native menus, tray icons, global shortcuts and OS
  hooks. A stub lives in `src/strange-quark-stubs/`. It keeps the original
  function's signature and starts with a `// SQUARK-RENDER STUB:` comment saying
  what the real code did. It either returns clearly fake data or throws
  `NotAvailableInStrangeQuark`, whichever lets the rest of the UI keep working.
  Stubs are listed in the report. A flow that touches a stub never counts as
  working.
- **Never claim more than you measured.** Every claim in the report points
  at evidence: a command's output, a measurement file, a screenshot diff.
  Anything you did not test is listed under "not tested", not left out. Do
  not round a failure into a pass, or a crash into "minor issue".

## Step 0: Find Strange Quark

Run `squark --version` (or `npx squark --version`) and record the version.
Probe the exact runtime or built app binary the user plans to ship:

```sh
node <skill-dir>/scripts/probe.mjs \
  --squark-binary <path-to-squark-runtime-or-built-app> \
  --out strange-quark-migration/probe.json
```

If you have the `squark` CLI but not a packed app, pass `--squark <path-to-squark>`
instead. The probe result is the authority for what that installed runtime
supports. It wins over `references/compatibility.md`,
and `docs/architecture.md` when they disagree. Confirm
every gap attributed to Strange Quark with the probe or a measured failure.
The probe checks file-input selection through the DOM. Measure the native
file chooser separately if a core flow depends on it.

If Strange Quark is not installed, the probe cannot start, or it cannot build
apps yet, you can still do the inventory, the migration and the browser
baseline. The verdict is then capped at what you could measure, and the
report says Strange Quark was not measured. Do not attribute an unprobed gap to
Strange Quark.

## Step 1: Inventory (read only, before you change anything)

Run the scanner, then read what it points at:

```sh
node <skill-dir>/scripts/scan.mjs <repo> --out strange-quark-migration/inventory.json
```

It reports the source kind (Next.js, TanStack Start, Remix or React Router
framework mode, Electron, Tauri, Electrobun, or plain React), the React
version, the bundler, and every server-only or shell-only marker it found.
It also lists every web API or CSS feature Strange Quark does not support
yet. The scanner finds patterns. You decide what they mean.

Write `strange-quark-migration/INVENTORY.md`:

- **What the app is**, in two sentences.
- **Source kind and versions.** React older than 18.3 has to be upgraded,
  which is a risk of its own.
- **The rendering and data model:** what renders or runs on a server, or in
  the shell's backend. List each server action, API route, loader, IPC
  channel or command, and what it does.
- **Core flows:** the 3 to 6 things users mainly do with the app, written
  as concrete steps ("open the board, add a card titled X, drag it to
  Done"). Decide these before migrating. They drive the measurements and
  the verdict, and choosing them afterwards invites choosing easy ones.
- **Unsupported features from the scan,** each marked as core (a core flow
  needs it) or peripheral.

## Step 2: Migrate

Read the playbook for the source kind before changing anything. Each one
maps that framework's features to strange-quark-compatible React:

| Source | Playbook |
| --- | --- |
| Next.js (App Router or Pages Router) | `references/nextjs.md` |
| TanStack Start | `references/tanstack-start.md` |
| Remix, React Router framework mode | `references/remix-react-router.md` |
| Electron | `references/electron.md` |
| Tauri | `references/tauri.md` |
| Electrobun | `references/electrobun.md` |

The target is a client-rendered single-page app whose build writes static
files, normally Vite with React and a client router. Work in this order:

1. **Make it a single-page app that runs in a browser.** Strange Quark is
   measured against this browser build. If the app does not work in
   Chromium, Strange Quark cannot be judged on it.
2. **Handle the server or backend code.**
   - *Meta-framework server code* (actions, route handlers, data fetching
     in server components) usually becomes an HTTP API the app calls with
     `fetch`. Move it into a `server/` package in the repo when that is
     straightforward. The report must then say that the native app needs
     that server running somewhere. When the extraction would be large,
     stub it instead, and say so.
   - *Desktop backend code* (IPC, commands, Node or Rust APIs) gets the
     real web equivalent where one exists. Everything else is stubbed and
     flagged.
3. **Add `squark.config.ts`:** the app name, the window size and title, the
   build command and output directory, and `network.allow` for every host
   the app fetches from.
4. **Commit after each meaningful stage**, and keep `MIGRATION.md` current
   as you go, not written from memory at the end.

## Step 3: Browser baseline

Build the migrated app (`npm run build`, or whatever the project uses).
Check that every core flow works in Chromium. Fix the migration until they
do, or record a flow as broken by the migration itself. That is a migration
gap, not a Strange Quark result.

## Step 4: Build with Strange Quark

```sh
squark build            # or: npx squark build; writes dist/<name>
```

Run the binary once, headless and in a window if a display is available.
Keep everything it prints: unsupported-CSS warnings, console errors,
undefined APIs. These are evidence.

## Step 5: Measure

Read `references/measuring.md`, then write the core flows as a flows file
and run them in both engines:

```sh
node <skill-dir>/scripts/measure.mjs \
  --dist <build-output-dir> --squark-binary dist/<name> \
  --flows strange-quark-migration/flows.json --out strange-quark-migration/results
```

The script runs every flow in headless Chromium and in Strange Quark, and
compares DOM state and screenshots at every checkpoint. It uses
Strange Quark's own app tolerance: up to 8 levels per colour channel, on at
most 0.5% of pixels. It also records time to first rendered frame, memory,
in-page input-to-frame latency, and binary size.

For a desktop shell, also measure the original Electron, Tauri or
Electrobun app if it builds and runs here: startup time, memory and package
size. That is the comparison these users care about most. If you cannot
build it, say so; do not estimate.

## Step 6: Verdict and report

Write `strange-quark-migration/REPORT.md` from `references/report-template.md`.
Choose the verdict by these rules, not by how the migration felt:

- **Viable.** All of these hold:
  - the app builds and starts;
  - every core flow passes in Strange Quark, with equal DOM state at every
    checkpoint and screenshots within tolerance;
  - no core flow touches a stub or an unsupported API;
  - nothing crashes;
  - performance is not materially worse than Chromium: time to first frame
    within 1.5× Chromium's (or under 1.5 s), and input-to-frame p95 within
    two frames (33 ms at 60 Hz) and within 1.5× Chromium's.
- **Partially viable.** The app starts and at least one core flow passes,
  but some functionality is stubbed, unsupported or out of tolerance. List
  exactly what keeps it from viable, and who would have to change: the
  app's code, Strange Quark (a missing feature or a bug), or nobody because
  the gap is by design.
- **Blocked.** Any of these:
  - the app cannot be built or started;
  - the app's primary purpose depends on something Strange Quark does not
    support, and no change on the app's side fixes it (a WebGL scene, a
    `contenteditable` editor, an iframe-based core, a required native
    integration);
  - a core flow crashes.

For every gap, say which kind it is. A **migration gap** can be fixed in
the app. A **Strange Quark gap** is a missing feature or a bug, which you
describe so it can be reported upstream. **By design** means Strange Quark
does not plan to support it.

End with a short message to the user:

- the branch name and the verdict;
- the three facts that decided it;
- where the report is;
- that nothing was pushed.

## Files this skill writes

Everything goes on the migration branch:

```
strange-quark-migration/
├─ INVENTORY.md     step 1
├─ inventory.json   the scanner's output
├─ probe.json       installed runtime capability probe
├─ MIGRATION.md     every change, with its reason and category
├─ flows.json       the core flows, as measured
├─ results/         measure.mjs output: measure.json, screenshots, diffs
└─ REPORT.md        the verdict and its evidence
src/strange-quark-stubs/  only if anything was stubbed
squark.config.ts
```
