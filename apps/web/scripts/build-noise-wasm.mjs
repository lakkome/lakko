// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Builds `crates/lakko-noise-wasm` for `wasm32-unknown-unknown` (always in
 * release: RNNoise in a debug build is too slow for the audio thread) and
 * copies it to `apps/web/wasm/lakko_noise.wasm`, where
 * `src/calls/noise-assets.ts` imports it (ADR-0088).
 *
 * Honours `CARGO_TARGET_DIR`. Fails loudly when the build fails: a web build
 * without the module would ship a noise switch that always falls back.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const targetDir = process.env.CARGO_TARGET_DIR
  ? resolve(process.env.CARGO_TARGET_DIR)
  : join(repo, "target");

execFileSync(
  "cargo",
  ["build", "-p", "lakko-noise-wasm", "--target", "wasm32-unknown-unknown", "--release"],
  { cwd: repo, stdio: "inherit" },
);

const built = join(targetDir, "wasm32-unknown-unknown", "release", "lakko_noise_wasm.wasm");
const out = join(repo, "apps", "web", "wasm", "lakko_noise.wasm");
mkdirSync(dirname(out), { recursive: true });
copyFileSync(built, out);
console.log(`lakko_noise.wasm -> ${out}`);
