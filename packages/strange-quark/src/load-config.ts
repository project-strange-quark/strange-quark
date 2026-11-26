#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Emits ResolvedSquarkConfig as JSON on stdout for `squark-cli` (`squark build`).

import { resolveConfig } from "./config.js";

const projectDir = process.argv[2] ?? process.cwd();
const configPath = process.argv[3] || null;
const resolved = await resolveConfig(projectDir, { configPath });
process.stdout.write(JSON.stringify(resolved) + "\n");
