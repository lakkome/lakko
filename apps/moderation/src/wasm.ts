// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web client's wasm core, loaded once. The panel has no copy: it is
 * `apps/web/wasm/lakko_ffi_wasm`, built by `npm run build:wasm` in apps/web.
 * Reports and appeals are opened, and statements built, here in the browser;
 * the panel's backend holds no key that decrypts or signs.
 */

import init, * as wasm from "../../web/wasm/lakko_ffi_wasm";

let cached: Promise<typeof wasm> | null = null;

export function loadWasm(): Promise<typeof wasm> {
  if (cached === null) cached = init().then(() => wasm);
  return cached;
}

export type Wasm = typeof wasm;
