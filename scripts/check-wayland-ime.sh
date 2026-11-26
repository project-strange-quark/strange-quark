#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Run the native text-input-v3 test on an isolated nested Wayland seat.

set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
mkdir -p target
exec > >(tee target/check-wayland-ime.log) 2>&1

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

timeout 180s npm install --no-audit --no-fund --silent
timeout 180s npm run build --workspace=@strange-quark/dom
timeout 720s cargo build --locked --target-dir target -p squark-cli -p squark-host --bins
timeout 720s cargo test --locked --target-dir target -p squark-host --test wayland_ime --no-run

work="$(mktemp -d "$root/target/wli-XXXXXX")"
compositor=""
cleanup() {
    if [[ -n $compositor ]]; then
        kill "$compositor" 2>/dev/null || true
        wait "$compositor" 2>/dev/null || true
    fi
    for log in "$work"/hypr/*/hyprland.log; do
        if [[ -f $log ]]; then
            cat "$log" >> target/check-wayland-ime-compositor.log
        fi
    done
    rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

app="$work/todomvc"
timeout 150s target/debug/squark build apps/todomvc --runtime target/debug/squark-runtime --out "$app"
cat > "$work/hyprland.lua" <<'LUA'
hl.monitor({ output = "WAYLAND-1", mode = "1280x800@60", position = "0x0", scale = 1 })
hl.config({
  debug = { enable_stdout_logs = true, vfr = true, fifo_pending_workaround = false },
  animations = { enabled = false },
  xwayland = { enabled = false },
  misc = { disable_hyprland_logo = true, disable_splash_rendering = true, force_default_wallpaper = 0 },
})
LUA

# A short path keeps Wayland and Hyprland's Unix socket names below sun_path.
# Both nested clients and server see this private runtime directory.
(
    cd "$work"
    exec timeout -k 5s 90s env -u DISPLAY -u WAYLAND_SOCKET -u HYPRLAND_INSTANCE_SIGNATURE \
        XDG_RUNTIME_DIR="/proc/$BASHPID/cwd" WAYLAND_DISPLAY="$outer_display" \
        Hyprland -c "$work/hyprland.lua"
) > target/check-wayland-ime-compositor.log 2>&1 &
compositor=$!
nested="/proc/$compositor/cwd"

display=""
ready=false
last_monitors="no Hyprland monitor response received"
poll_timeout=30
deadline=$((SECONDS + poll_timeout))
while (( SECONDS < deadline )); do
    if ! kill -0 "$compositor" 2>/dev/null; then
        last_monitors="nested Hyprland exited before its monitor became ready"
        break
    fi
    display="$(find "$work" -maxdepth 1 -type s -name 'wayland-*' -printf '%f\n' -quit)"
    if [[ -n $display && -d $work/hypr ]]; then
        instance="$(find "$work/hypr" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' -quit)"
        if [[ -n $instance ]]; then
            monitors="$(timeout 1s env XDG_RUNTIME_DIR="$nested" HYPRLAND_INSTANCE_SIGNATURE="$instance" hyprctl -j monitors 2>/dev/null || true)"
            if [[ -n $monitors ]]; then
                last_monitors="$monitors"
                if node -e 'process.exit(JSON.parse(process.argv[1]).some(m =>
                    m.dpmsStatus && m.width > 0 && m.height > 0 && m.name !== "FALLBACK") ? 0 : 1)' "$monitors" 2>/dev/null; then
                    ready=true
                    break
                fi
            else
                last_monitors="hyprctl -j monitors returned no response"
            fi
        else
            last_monitors="nested Hyprland IPC instance not discovered"
        fi
    else
        last_monitors="nested Wayland socket or Hyprland IPC directory not discovered"
    fi
    sleep 0.05
done
if [[ $ready != true ]]; then
    echo "Nested Hyprland did not configure an active output within $poll_timeout seconds. Last monitor state: $last_monitors"
    echo "See target/check-wayland-ime-compositor.log for compositor details."
    exit 1
fi

echo "Running TodoMVC's IME test on the nested Wayland socket."
timeout -k 5s 55s env -u DISPLAY -u WAYLAND_SOCKET -u HYPRLAND_INSTANCE_SIGNATURE \
    XDG_RUNTIME_DIR="$nested" WAYLAND_DISPLAY="$display" WAYLAND_DEBUG=client \
    SQUARK_WINDOW_TESTS=1 SQUARK_IME_REACT_APP="$app" \
    cargo test --locked --target-dir target -p squark-host --test wayland_ime \
    wayland_ime_commits_to_a_react_controlled_input -- --exact --nocapture
