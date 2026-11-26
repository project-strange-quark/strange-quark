#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Run the real Excalidraw 1,000-element pan benchmark windowed on the real GPU,
# inside a private nested Hyprland, so desktop input and other windows cannot
# reach or resize it. The nested compositor shows its output on a headless
# 1280x800 monitor and disables its own window on the desktop: the desktop
# can neither tile that window to another size nor deliver pointer input to it.
#
# Usage: scripts/check-excalidraw-pan.sh [binary] [output-dir]
#   binary      packed Excalidraw (default target/excalidraw-native/excalidraw-native)
#   output-dir  driver output (default target/excalidraw-pan)
# SQUARK_PAN_NESTED_HZ sets the nested output's refresh rate. The default, 180,
# matches a 180 Hz desktop panel: the benchmark caps its cadence at 60 Hz, and
# a 60 Hz output beats against that cap instead.

set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# The benchmark and its compositor run in a user scope with CPUWeight=10000. Re-enter this script in one if needed.
cgroup="$(sed -n 's/^0:://p' /proc/self/cgroup)"
if [[ "$(cat "/sys/fs/cgroup$cgroup/cpu.weight" 2>/dev/null)" != 10000 ]]; then
    exec systemd-run --user --scope --quiet -p CPUWeight=10000 "$0" "$@"
fi

binary="$(realpath -m "${1:-target/excalidraw-native/excalidraw-native}")"
output="$(realpath -m "${2:-target/excalidraw-pan}")"
hz="${SQUARK_PAN_NESTED_HZ:-180}"
mkdir -p target "$output"
log="$output/check.log"
exec > >(tee "$log") 2>&1

: "${WAYLAND_DISPLAY:?Run this check from an active Wayland session}"
outer_display=$WAYLAND_DISPLAY
if [[ $outer_display != /* ]]; then
    : "${XDG_RUNTIME_DIR:?The parent Wayland session needs XDG_RUNTIME_DIR}"
    outer_display="$XDG_RUNTIME_DIR/$outer_display"
fi
if [[ ! -S $outer_display ]] || ! command -v Hyprland hyprctl >/dev/null; then
    echo "An active Wayland socket and the Hyprland installation are required."
    exit 1
fi
if [[ ! -x $binary ]]; then
    echo "No packed Excalidraw at $binary. Build it with tests/excalidraw's packExcalidraw()."
    exit 1
fi

work="$(mktemp -d "$root/target/pan-XXXXXX")"
compositor=""
cleanup() {
    if [[ -n $compositor ]]; then
        kill "$compositor" 2>/dev/null || true
        wait "$compositor" 2>/dev/null || true
    fi
    for hyprlog in "$work"/hypr/*/hyprland.log; do
        if [[ -f $hyprlog ]]; then
            cat "$hyprlog" >> "$output/compositor.log"
        fi
    done
    rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# No gaps, borders or rounding: the only, tiled window gets the whole
# 1280x800 output, the size the driver's wheel point is scripted for.
cat > "$work/hyprland.lua" <<LUA
hl.monitor({ output = "WAYLAND-1", disabled = true })
hl.monitor({ output = "PAN", mode = "1280x800@$hz", position = "0x0", scale = 1 })
hl.config({
  general = { gaps_in = 0, gaps_out = 0, border_size = 0 },
  decoration = { rounding = 0 },
  debug = { enable_stdout_logs = true, vfr = true, fifo_pending_workaround = false },
  animations = { enabled = false },
  xwayland = { enabled = false },
  misc = { disable_hyprland_logo = true, disable_splash_rendering = true, force_default_wallpaper = 0 },
})
LUA

# A short path keeps Wayland and Hyprland's Unix socket names below sun_path.
(
    cd "$work"
    exec timeout -k 5s 240s env -u DISPLAY -u WAYLAND_SOCKET -u HYPRLAND_INSTANCE_SIGNATURE \
        XDG_RUNTIME_DIR="/proc/$BASHPID/cwd" WAYLAND_DISPLAY="$outer_display" \
        Hyprland -c "$work/hyprland.lua"
) > "$output/compositor.log" 2>&1 &
compositor=$!
nested="/proc/$compositor/cwd"

display=""
headless=false
ready=false
last_monitors="no Hyprland monitor response received"
deadline=$((SECONDS + 30))
while (( SECONDS < deadline )); do
    if ! kill -0 "$compositor" 2>/dev/null; then
        last_monitors="nested Hyprland exited before its monitor became ready"
        break
    fi
    display="$(find "$work" -maxdepth 1 -type s -name 'wayland-*' -printf '%f\n' -quit)"
    if [[ -n $display && -d $work/hypr ]]; then
        instance="$(find "$work/hypr" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' -quit)"
        if [[ -n $instance ]]; then
            if [[ $headless != true ]] && timeout 1s env XDG_RUNTIME_DIR="$nested" \
                HYPRLAND_INSTANCE_SIGNATURE="$instance" hyprctl output create headless PAN >/dev/null 2>&1; then
                headless=true
            fi
            monitors="$(timeout 1s env XDG_RUNTIME_DIR="$nested" HYPRLAND_INSTANCE_SIGNATURE="$instance" hyprctl -j monitors 2>/dev/null || true)"
            if [[ -n $monitors ]]; then
                last_monitors="$monitors"
                if node -e 'process.exit(JSON.parse(process.argv[1]).some(m =>
                    m.dpmsStatus && m.name === "PAN" && m.width === 1280 && m.height === 800) ? 0 : 1)' "$monitors" 2>/dev/null; then
                    ready=true
                    break
                fi
            fi
        fi
    fi
    sleep 0.05
done
if [[ $ready != true ]]; then
    echo "Nested Hyprland did not configure an active output within 30 seconds. Last monitor state: $last_monitors"
    exit 1
fi
echo "Nested monitor: $(node -e 'const m = JSON.parse(process.argv[1]).find(m => m.name === "PAN");
    console.log(`${m.name} ${m.width}x${m.height}@${m.refreshRate.toFixed(2)}`)' "$last_monitors")"
echo "Load average: $(cut -d' ' -f1-3 /proc/loadavg)"

timeout -k 5s 150s env -u DISPLAY -u WAYLAND_SOCKET -u HYPRLAND_INSTANCE_SIGNATURE \
    XDG_RUNTIME_DIR="$nested" WAYLAND_DISPLAY="$display" \
    node crates/squark-host/tests/fixtures/excalidraw-pan-driver.mjs "$binary" "$output"
