#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Runs every check required before pushing to main (AGENTS.md).
#
# The steps live in check.mjs so Linux, macOS and Windows run the same list;
# Windows has no bash. This wrapper only locates Node and hands over.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

exec node scripts/check.mjs "$@"
