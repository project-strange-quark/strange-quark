<!-- SPDX-License-Identifier: MIT -->
# Security

## Reporting a vulnerability

Please report security problems privately, through GitHub's private
vulnerability reporting: open the repository's **Security** tab and choose
**Report a vulnerability**. Don't open a public issue for them.

Say which version you used (`squark --version`, or
`<app> --version` for a built app), your platform, and how to reproduce the
problem. We will reply in the report, and fix what we confirm in the newest
release.

## What Strange Quark protects, and what it doesn't

**An app's JavaScript is trusted code.** A Strange Quark app is a native
program built from code its author ships. The runtime exposes only the web
APIs that squark-dom implements, but it is not a sandbox for hostile code
the way a browser tab is. Don't load untrusted remote scripts into an app.

**`network.allow` is a policy, not a sandbox.** The hosts in
`squark.config.ts`'s `network.allow` are the only ones `fetch` and
`WebSocket` may reach, which keeps an app to the servers its author meant
it to use. It does not make running untrusted code safe.

**Opening links.** A link to another origin, or one that opens a new
window, goes to the operating system's default handler, and the runtime
hands over only `http`, `https` and `mailto` URLs.

## Developer and test hooks in every build

Every `squark-runtime`, and so every app `squark build` makes, accepts
these. All of them need a person or program that can already start the
app as the current user, with the arguments and environment it chooses.

- **`--test-control <path>`** opens a local channel that the test
  harnesses use to drive the app. Through it, a client can evaluate
  JavaScript in the app (`dom.eval`), send keyboard, pointer, wheel and IME
  input, read and write the clipboard, queue answers to file dialogs, write
  screenshots to a path it names, and quit the app. The channel is off unless the flag is
  passed. On Linux and macOS it is a Unix socket that only the current user
  can open (mode 0600, created in a private directory). On Windows it is a
  named pipe whose access list admits only the current user, which refuses
  remote clients and which another process cannot create first.
- **A pack argument.** An app given a path as its first argument runs that
  `.sqpak` or packed binary instead of its own pack.
- **`SQUARK_OPEN_EXTERNAL_COMMAND`** replaces the program that opens
  external links.

If you find a way to reach any of these without that access, please report
it as described above.
