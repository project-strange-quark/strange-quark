<!-- SPDX-License-Identifier: MIT -->
# Third-party notices

`THIRD_PARTY_NOTICES.txt` at the repository root lists the third-party
software linked into the shipped `squark` and `squark-runtime` binaries, with
the licence each is used under and its full text. Those licences require the
notices to travel with the binaries, so:

- every `@strange-quark/cli-<os>-<cpu>` npm package ships the file beside
  `bin/` (`scripts/npm/stage.mjs`);
- `squark-runtime` embeds it and prints it with `--third-party-notices`. Every
  app `squark build` makes is that runtime with a pack added, so the app
  carries the notices too: `./dist/my-app --third-party-notices`. A Windows
  app is a GUI program, so redirect the output there:
  `.\dist\my-app.exe --third-party-notices > notices.txt`.

## Regenerating

Run this after any change to `Cargo.lock`, and commit the result:

```sh
cargo install cargo-about --locked --features cli --version 0.9.3
node scripts/notices/generate.mjs
```

CI and the release workflow run `node scripts/notices/generate.mjs --check`,
which fails when the committed file is out of date.

The file has two parts:

1. **Libraries in V8's prebuilt static library.** The `v8` crate links
   `librusty_v8.a`, which bundles more than V8: ICU, Abseil, Highway,
   simdutf, fast_float, Dragonbox, FP16, rapidhash, HalfSipHash, a UTF-8
   decoder, and LLVM's libc++, libc++abi, libunwind and llvm-libc. The
   published crate carries none of their licence files, so
   `v8/components.json` lists them, with versions from the crate's
   `v8/DEPS`, and `v8/` holds each licence text, copied unchanged from
   upstream.
2. **Rust crates**, from `cargo-about` with `about.toml`: every crate linked
   into a binary on any of the six npm targets. Build-only and dev-only
   dependencies are left out.

## When the `v8` crate changes

`generate.mjs` refuses to run when `Cargo.lock`'s `v8` version differs from
`v8Crate` in `components.json`. Update the components by hand:

1. Read the new V8 version from the crate's `v8/include/v8-version.h` and
   the bundled revisions from its `v8/DEPS`.
2. Check which libraries the static library defines: `nm -C` on
   `target/*/gn_out/obj/librusty_v8.a`, looking at the namespaces
   (`icu_78::`, `absl::`, `hwy::`, `simdutf::`, `std::__Cr::` and so on).
   Count defined symbols only, not references.
3. Fetch each licence file at the new revision, replace the texts in `v8/`,
   update `components.json`, including `v8Crate`, and regenerate.
