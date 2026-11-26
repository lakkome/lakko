// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Builds Lakko's translation engine (ADR-0100) by running
 * tools/translate-wasm/build.sh, which writes
 * apps/web/wasm-translate/lakko_translate.{mjs,wasm,d.mts}. The script skips the build
 * when its stamp says nothing changed; the first run downloads the pinned
 * mozilla/translations tree, emsdk and zstd into the user cache.
 *
 * Fails loudly when the build fails: a web build without the module would
 * ship a translation engine that never loads.
 */

import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");

execFileSync("bash", [join(repo, "tools", "translate-wasm", "build.sh")], {
  cwd: repo,
  stdio: "inherit",
});
