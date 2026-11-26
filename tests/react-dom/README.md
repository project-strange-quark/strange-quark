<!-- SPDX-License-Identifier: MIT -->
# React DOM 19.0.0 in squark-dom

The source lane uses React [v19.0.0](https://github.com/facebook/react/tree/v19.0.0),
commit `7aa5dda3b3e4c2baa905a59b922ae7ec14734b24`. The Jest configuration
inherits React's stable source transforms, mocks, setup and fake timers.
The squark-dom environment loads the compiled ESM into each Jest VM before
React's setup runs. React and its tests run from an unmodified checkout.

## Reproduce

Run from the Strange Quark repository root with Node 26.

```sh
timeout 180s git clone --depth 1 --branch v19.0.0 https://github.com/facebook/react.git target/react-19.0.0
timeout 360s npx --yes yarn@1.22.22 --cwd target/react-19.0.0 install --frozen-lockfile --ignore-scripts --ignore-engines --non-interactive
timeout 120s npm run build --workspace @strange-quark/dom
timeout 180s node tests/react-dom/run.mjs
```

React's Yarn 1 lockfile pins the upstream test tools. The Yarn command
runs from the Strange Quark root so current npm can start Yarn without
parsing the older React checkout's `devEngines` field. The runner accepts
`--source PATH` or `SQUARK_REACT_SOURCE` if the prepared checkout is elsewhere.

The default command runs the passing case names recorded in
[cases.json](cases.json), across all ten suites, and is wired into root
`npm test` via `@strange-quark/react-dom-tests` (`prepare.mjs` + `run.mjs`). To
rerun the full source inventory, including the exclusions, use the commands
below.
The full squark-dom run exits with Jest's failure status while known
exclusions are present.

```sh
timeout 180s node tests/react-dom/run.mjs --baseline --report target/react-baseline.json
timeout 180s node tests/react-dom/run.mjs --all --report target/react-squark-dom.json
```

`--baseline` uses React's upstream jsdom environment. `--all` uses the
squark-dom environment. The environment supplies MessageChannel tasks and
animation frames using the test realm's timers. The explicit V8
checkpoint and fake-clock scheduling tests live in
[crates/squark-js/tests/timing.rs](../../crates/squark-js/tests/timing.rs).

## Cases

The jsdom baseline passed all 561 cases. squark-dom passed 520 cases in the
same run, and 41 cases are excluded from the default Node lane.

[cases.json](cases.json) gives exact Jest full names grouped by upstream
file and a missing feature or harness dependency for every excluded case.
The parser and selector cases depend on the document-engine bridge.
The upstream tests for batching between different roots use a helper
that patches jsdom's private implementation objects to simulate native
dispatch. They need a squark-dom or runtime lane for that helper's
interaction semantics.
