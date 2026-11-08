// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Builds `crates/lakko-segment-wasm` for `wasm32-unknown-unknown` in release
 * with wasm SIMD, and copies it to `apps/web/wasm/lakko_segment.wasm`, which
 * the camera background worker instantiates with no imports (ADR-0089).
 *
 * Two flags the default wasm build does not carry, so it builds in a target
 * directory of its own (otherwise every `wasm-pack` run of lakko-ffi-wasm and
 * this one would rebuild each other's dependencies):
 * - `+simd128`: the model is about twice as fast with it (ADR-0089 spike); a
 *   browser without wasm SIMD gets no background rather than a slow one;
 * - `getrandom_backend="custom"`: `tract` reaches `getrandom`, the module
 *   supplies a backend that refuses (the model draws no random numbers).
 *
 * Honours `CARGO_TARGET_DIR` (as the parent of its own directory). Fails
 * loudly when the build fails.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const targetDir = join(
  process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(repo, "target"),
  "segment-wasm",
);

execFileSync(
  "cargo",
  ["build", "-p", "lakko-segment-wasm", "--target", "wasm32-unknown-unknown", "--release"],
  {
    cwd: repo,
    stdio: "inherit",
    env: {
      ...process.env,
      CARGO_TARGET_DIR: targetDir,
      CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:
        '-C target-feature=+simd128 --cfg getrandom_backend="custom"',
    },
  },
);

const built = join(targetDir, "wasm32-unknown-unknown", "release", "lakko_segment_wasm.wasm");
const out = join(repo, "apps", "web", "wasm", "lakko_segment.wasm");
mkdirSync(dirname(out), { recursive: true });
copyFileSync(built, out);
console.log(`lakko_segment.wasm -> ${out} (${statSync(out).size} bytes)`);
