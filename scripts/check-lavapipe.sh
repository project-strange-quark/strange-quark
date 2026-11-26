#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Reproduces CI's "Headless GPU tests (Mesa lavapipe)" job locally. Runs the
# same steps, forces the Mesa software adapter, and refuses to report on a hardware
# adapter. Log: target/lavapipe-check.log.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

LOG="${ROOT_DIR}/target/lavapipe-check.log"
mkdir -p "${ROOT_DIR}/target"
exec > >(tee -a "$LOG") 2>&1

echo "==> lavapipe check started $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "    log: ${LOG}"

fail() {
  echo "ERROR: $*" >&2
  echo "lavapipe check FAILED; log: ${LOG}" >&2
  exit 1
}

# 1. Find the ICD exactly as CI does, then point Vulkan (and wgpu) at it.
ICD="$(find /usr/share/vulkan/icd.d /etc/vulkan/icd.d -maxdepth 1 -name '*lvp*.json' 2>/dev/null | head -n 1 || true)"
if [ -z "$ICD" ] || [ ! -r "$ICD" ]; then
  fail "lavapipe Vulkan ICD not found in /usr/share/vulkan/icd.d or /etc/vulkan/icd.d (install mesa-vulkan-drivers / vulkan-swrast)"
fi
echo "==> Using lavapipe ICD: ${ICD}"
export VK_ICD_FILENAMES="$ICD"
export SQUARK_GPU_TESTS=1
export SQUARK_GPU_ADAPTER=software

# 2. Build everything the steps need, like the CI job does.
echo "==> Installing and building JS workspaces"
timeout 180 npm install --no-audit --no-fund --silent
timeout 180 npm run build --workspaces --if-present

echo "==> Building workspace binaries and test executables"
timeout 1800 cargo build --workspace --bins
timeout 1800 cargo test --workspace --no-run
echo "==> Building the stripped dist runtime for the Excalidraw native tests"
timeout 2400 cargo build --profile dist -p squark-host --bin squark-runtime

# 3. The adapter wgpu selects. squark-hello prints it from wgpu's own AdapterInfo,
# so this is the device wgpu will use, not just what Vulkan lists.
ADAPTER_LOG="$(mktemp "${ROOT_DIR}/target/lavapipe-adapter-XXXXXX.log")"
timeout 120 target/debug/squark-hello --headless "${ROOT_DIR}/target/lavapipe-adapter.png" 2>"$ADAPTER_LOG" \
  || fail "squark-hello could not create a headless GPU adapter (see ${ADAPTER_LOG})"
ADAPTER="$(sed -n 's/^Vulkan adapter: //p' "$ADAPTER_LOG" | head -n1)"
rm -f "$ADAPTER_LOG" "${ROOT_DIR}/target/lavapipe-adapter.png"
[ -n "$ADAPTER" ] || fail "squark-hello did not report its Vulkan adapter"
echo "==> Adapter (wgpu): ${ADAPTER}"
if ! printf '%s' "$ADAPTER" | grep -Eiq 'llvmpipe|lavapipe'; then
  fail "wgpu selected a hardware adapter (${ADAPTER}); lavapipe must be the only ICD"
fi

# 4. R1 inline geometry comparison (squark-layout).
echo "==> R1 inline geometry comparison"
rm -rf target/r1-packs
mkdir -p target/r1-packs/01-inline
for f in tests/reftests/cases/r1/01-inline/*.html; do
  name="$(basename "$f" .html)"
  timeout 30 target/debug/squark pack tests/reftests/cases/r1 --html "$f" \
    --out "target/r1-packs/01-inline/${name}.sqpak" >/dev/null
done
SQUARK_R1_PACKS="$PWD/target/r1-packs" \
  timeout 120 cargo test --workspace -- --ignored compares_r1_layout_to_chromium --nocapture

# 5. Headless GPU tests.
echo "==> Headless GPU tests (cargo test --workspace, SQUARK_GPU_TESTS=1)"
timeout 600 cargo test --workspace

# 6. Counter and Excalidraw build/pack/runtime smoke.
echo "==> Smoke (@strange-quark/smoke)"
timeout 600 npm test --workspace=@strange-quark/smoke

# 7. Native dev save-to-presented measurement.
echo "==> Native dev measurement (@strange-quark/dev-tests test:native)"
timeout 600 npm run test:native --workspace=@strange-quark/dev-tests

# 8. Excalidraw native smoke test (report-only).
echo "==> Excalidraw native smoke test (report-only)"
if ! timeout 600 npm run test:native --workspace=@strange-quark/excalidraw-native; then
  echo "    (report-only: the native smoke checks are failing; see tests/results/platform/excalidraw-native.json)"
fi

# 9. Excalidraw: canonical scene baselines (Chromium, must pass) and the e2e flow.
echo "==> Excalidraw canonical scene baselines (Chromium)"
timeout 900 npm run scenes:check --workspace=@strange-quark/excalidraw-native
echo "==> Excalidraw end-to-end flow (Chromium and native gating, DOM compared at every checkpoint)"
timeout 1200 npm run e2e --workspace=@strange-quark/excalidraw-native

# 10. Platform report (R3 report-only).
echo "==> Platform report (@strange-quark/platform-report)"
SQUARK_LAVAPIPE_RESULT=passed timeout 900 npm test --workspace=@strange-quark/platform-report

# 11. R1 reftest compare on lavapipe.
echo "==> R1 reftest compare"
timeout 900 node tests/reftests/src/cli.mjs \
  --suite r1 \
  --mode compare \
  --gpu-adapter software \
  --squark-bin target/debug/squark \
  --runtime target/debug/squark-runtime \
  --min-pass-rate 0.90

echo "==> lavapipe check passed; log: ${LOG}"
