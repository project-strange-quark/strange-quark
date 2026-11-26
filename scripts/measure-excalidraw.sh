#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Excalidraw startup and pan measurements. Re-executes itself in a user scope
# with maximum CPU weight because the machine may be shared with other builds, and the
# windowed startup and pan numbers are only meaningful when this process is not
# starved. Each measurement records the load average next to its result.
#
#   SQUARK_WINDOW_TESTS=1 is set by default (windowed startup, windowed pan).
#   --startup runs only the 5-run windowed startup measurement.
#   --pan runs the validated nested-Hyprland 1,000-element Excalidraw pan
#   benchmark 3 times (gating: all 3 runs must be valid and >= 95%; pass
#   --report-only-pan to record without failing).
#   Packaging checks live in tests/excalidraw/src/packaging.mjs (packaging:check).
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [ "${SQUARK_MEASURE_SCOPE:-}" != "1" ] && command -v systemd-run >/dev/null 2>&1; then
  exec systemd-run --user --scope -q -p CPUWeight=10000 \
    env SQUARK_MEASURE_SCOPE=1 SQUARK_WINDOW_TESTS="${SQUARK_WINDOW_TESTS:-1}" "${ROOT_DIR}/scripts/measure-excalidraw.sh" "$@"
fi

export SQUARK_WINDOW_TESTS="${SQUARK_WINDOW_TESTS:-1}"
exec node tests/excalidraw/src/measure.mjs "$@"
