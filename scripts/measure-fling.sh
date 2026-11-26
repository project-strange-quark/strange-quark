#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Measure the commit-deadline rate in a native virtual-list window.

set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
mkdir -p target
exec > >(tee target/measure-fling.log) 2>&1

if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
    echo "The fling needs an active Wayland or X11 output at 60 Hz or faster."
    exit 1
fi

timeout 180s npm install --no-audit --no-fund --silent
timeout 180s npm run build --workspace=@strange-quark/dom
timeout 720s cargo build --locked --target-dir target -p squark-cli --bin squark
timeout 1200s cargo build --locked --target-dir target --release -p squark-host --bin squark-runtime
timeout 150s target/debug/squark build apps/virtual-list \
    --runtime target/release/squark-runtime --out target/virtual-list

timeline=target/measure-fling-timeline.json
rm -f -- "$timeline"
timeout -k 5s 55s node crates/squark-host/tools/measure-fling.mjs target/virtual-list "$timeline"
