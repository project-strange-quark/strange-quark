# Report template

Write `strange-quark-migration/REPORT.md` with exactly these sections, in this
order. Keep it factual and short: someone will decide on the strength of
it, and they need to be able to check every claim.

```markdown
# Strange Quark migration report: <app name>

**Verdict: Viable | Partially viable | Blocked**

<Two or three sentences: what the app is, what Strange Quark can and cannot do
with it, and the single most important reason for the verdict.>

| | |
| --- | --- |
| Source | <Next.js 15 App Router / Electron 33 + Vite / ...> |
| Branch | `strange-quark-migration` (from <original branch> at <sha>, not pushed) |
| Strange Quark | <version, or "not installed"> |
| Measured on | <machine, OS and CPU architecture, GPU or software adapter, headless or windowed, date> |

## Core flows

| Flow | Chromium | Strange Quark | DOM checkpoints | Screenshots | Notes |
| --- | --- | --- | --- | --- | --- |
| <name> | pass | pass / fail / stubbed / not run | 4/4 | 3/4 (worst 0.9%) | <one line> |

## Performance

| Metric | Chromium | Strange Quark | Original app (if measured) |
| --- | --- | --- | --- |
| Time to first frame (median of N) | | | |
| Memory after the flows (RSS) | | | |
| Input to frame p50 / p95 (in page) | | | |
| Size on disk | n/a | <binary size> | <package size> |

## What changed in the migration

<Counts by category (mechanical, behavioural, stub), then the behavioural
changes and stubs that a user of the app would notice. The full list is in
MIGRATION.md.>

## Gaps

| Gap | Kind | Affects | What would close it |
| --- | --- | --- | --- |
| <e.g. Tauri command `index_folder` stubbed> | stub | core flow "search notes" | <a Strange Quark native module (not in v0.1.0), or moving indexing to a server> |
| <e.g. `@container` queries ignored> | Strange Quark gap (tier 2) | card layout at narrow widths | <Strange Quark tier 2 work, or media queries in the app> |

Kinds: migration gap (fixable in the app), Strange Quark gap (missing feature
or bug; describe it so it can be reported), by design (Strange Quark does not
plan it).

## Not tested

<Everything you could not measure or did not try, and why.>

## How to reproduce

<The exact commands: build, `squark build`, measure.mjs with its arguments.>
```

## Verdict wording

Use exactly one of the three words. If you are torn between two, choose
the lower one, and explain in one sentence what would raise it.
