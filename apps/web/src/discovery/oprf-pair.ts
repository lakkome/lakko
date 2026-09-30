// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 5 - the two values one OPRF round yields, kept apart by
 * name so no caller can send the wrong one to discovery.
 *
 * - `lookupKey` is the ONLY value discovery ever sees: username register and
 *   lookup, channel handle register and lookup, name terms
 *   (`PUT /discovery/visibility`) and `/discovery/name/lookup`. It is also
 *   what a username certificate binds (`verifyUsernameBinding`'s expected
 *   hash).
 * - `output` never leaves the device. Public records and introduction
 *   records are sealed under keys derived from it, so a server holding it
 *   could open them.
 */
import type { OprfBlinder, OprfResult } from "../../wasm/lakko_ffi_wasm";

/** One OPRF round, split. */
export interface OprfPair {
  /** Sent to discovery; bound by a username certificate. 64 bytes. */
  readonly lookupKey: Uint8Array;
  /** Stays on the device; seals and opens records. 64 bytes. Secret. */
  readonly output: Uint8Array;
}

/** Copy both values out of the wasm result and free it. */
export function splitOprfResult(result: OprfResult): OprfPair {
  try {
    return { lookupKey: result.lookupKey, output: result.output };
  } finally {
    result.free();
  }
}

/** `blinder.finalize(evaluatedB64)`, split. `finalize` consumes the blinder. */
export function finalizeOprf(blinder: OprfBlinder, evaluatedB64: string): OprfPair {
  return splitOprfResult(blinder.finalize(evaluatedB64));
}
