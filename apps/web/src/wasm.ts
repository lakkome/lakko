// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Thin wrapper around the wasm-pack output of `lakko-ffi-wasm`.
 *
 * The wasm module is loaded once on first call, cached as a module-
 * level promise, and returned to every subsequent caller. wasm-pack
 * emits an `init()` function that fetches+instantiates the .wasm
 * blob; once it resolves the named exports (Identity, Group,
 * EditChain, ContactRequest, …) are usable as regular JS classes.
 *
 * Phase 1.16a smoke goal: prove the JS↔WASM bridge works and the
 * Rust core can produce an Identity from inside the browser. Phase
 * 1.16b adds the rest of the surface (signup / login / chat).
 */
import { createSignal } from "solid-js";

import init, * as wasm from "../wasm/lakko_ffi_wasm";

let cached: Promise<typeof wasm> | null = null;

/** Bumped when the module finished loading, so a synchronous reader
 *  ({@link wasmSync}) re-runs once the exports are usable. */
const [loadedVersion, setLoadedVersion] = createSignal(0);

/**
 * The exports namespace for a synchronous reader (a render), or `null`
 * while the module is not loaded yet. Reactive: a caller inside a tracking
 * scope runs again when the load completes. A module initialised another way
 * (a test calling `init` itself) is usable too: the caller's call simply
 * succeeds.
 */
export function wasmSync(): typeof wasm {
  loadedVersion();
  return wasm;
}

/**
 * Initialise the WASM module on first call; return the cached
 * exports namespace on subsequent calls.
 *
 * Idempotent — the underlying `init()` from wasm-pack is itself
 * idempotent and returns the resolved instance bytes.
 */
export function loadWasm(): Promise<typeof wasm> {
  if (cached === null) {
    cached = init().then(() => {
      setLoadedVersion((v) => v + 1);
      return wasm;
    });
  }
  return cached;
}

/**
 * Type-only re-export of the wasm-bindgen-generated namespace so
 * call sites can `import type { Identity } from "../wasm"` without
 * triggering the loader.
 */
export type Wasm = typeof wasm;
