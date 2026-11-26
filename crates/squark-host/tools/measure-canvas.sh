#!/usr/bin/env bash
# SPDX-License-Identifier: MIT

set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$root"
out=target/canvas-pan-zoom
mkdir -p "$out"

if [[ ${1:-} == --x11-scope ]]; then
    display_fd=$(mktemp "$out/x11-display.XXXXXX")
    Xwayland -displayfd 3 -nolisten tcp -noreset 3>"$display_fd" >"$out/xwayland.log" 2>&1 &
    server=$!
    trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; rm -f "$display_fd"' EXIT
    for _ in {1..50}; do
        [[ ! -s "$display_fd" ]] || break
        sleep 0.1
    done
    test -s "$display_fd"
    DISPLAY=":$(cat "$display_fd")" env -u WAYLAND_DISPLAY -u XAUTHORITY \
        node crates/squark-host/tools/measure-canvas.mjs target/dist/squark-runtime "$out/scene.sqpak" "$out"
    exit
fi

exec > >(tee "$out/measure.log") 2>&1

timeout 180s npm install --no-audit --no-fund --silent
timeout 180s npm run build --workspace=@strange-quark/dom
timeout 720s cargo build --locked -p squark-cli --bin squark
timeout 1200s cargo build --locked --profile dist -p squark-host --bin squark-runtime
timeout 30s target/debug/squark pack crates/squark-host/tools/canvas-scene --out "$out/scene.sqpak"

if [[ ${1:-} == --x11 ]]; then
    exec timeout -k 8s 150s systemd-run --user --scope -p CPUWeight=10000 "$0" --x11-scope
fi
exec timeout -k 8s 150s systemd-run --user --scope -p CPUWeight=10000 \
    node crates/squark-host/tools/measure-canvas.mjs target/dist/squark-runtime "$out/scene.sqpak" "$out"
