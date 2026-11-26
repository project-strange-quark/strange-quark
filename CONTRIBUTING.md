# Contributing to Strange Quark

Thanks for helping. This page covers how to get a change merged and what we
think about AI tools.

## Where the rules are

[AGENTS.md](AGENTS.md) holds the working rules for this repository. They
apply to everyone, people and agents alike: which area owns which paths, the
Rust and TypeScript standards, the licence policy, the compatibility rules,
and how to build, test and commit. The design is in
[docs/architecture.md](docs/architecture.md).

The supported scope is the desktop (Linux, macOS and Windows) in Compat mode
on V8. If you want to work on something outside that scope, open an issue
first and ask.

## Before you open a pull request

- `scripts/check.sh` passes (`node scripts/check.mjs` on Windows).
- The change comes with a test that shows it works.
- Commit messages read `area: lowercase imperative summary`, with a body
  that says why. Each commit covers one topic.
- Every new source file starts with `SPDX-License-Identifier: MIT`.
- New dependencies use one of the licences AGENTS.md allows.

## AI-assisted contributions

We don't care whether a change was written by hand, with your editor's
autocomplete, or by an AI agent. We review the change, not the tool that
produced it, and every contribution is held to the same standards.

What we do care about:

- **You own what you submit.** Understand the change, be ready to answer
  review questions about it, and fix what review finds.
- **It must meet the bar.** It follows AGENTS.md, passes the checks and
  comes with its tests, like any other change.
- **You must have the right to contribute it** under the MIT licence. Code
  copied from elsewhere, by you or by a tool, follows the licence rules in
  AGENTS.md.

You don't need to tell us whether you used AI, and we won't ask. Don't add
`Co-Authored-By`, `Generated-with`, `Assisted-by` or similar trailers to your
commits.
