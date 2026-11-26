<!-- SPDX-License-Identifier: MIT -->
# Local Stylo patches

This directory contains the published `stylo` 0.24.0 crate from Servo
Stylo commit `32359dc3f17780ee269980cb7a3a7f8597a1dc0e`. It remains under
MPL-2.0. See LICENSE for the full licence text.

## Parse SVG pointer-events keywords in Servo builds

Upstream candidate. In `values/specified/ui.rs`, remove the Gecko-only
guards on `PointerEvents` variants for the SVG keywords. Chromium accepts
these values on HTML as well as SVG and preserves them in computed style.
For example, a `var()` that resolves to `all` must override an inherited
`none`. The embedding renderer remains responsible for hit-test semantics.
