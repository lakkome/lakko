// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * PoW solver glue — wraps the WASM `PowSolver` into a small
 * promise-returning helper.
 *
 * Phase 1.16c3b: the solver runs on the **main thread**. At the
 * server-default `effort_bits = 8` this is 100–500 ms, which is
 * acceptable inside a user-initiated signup / login flow that
 * already shows a "Signing in…" spinner. A dedicated Web Worker
 * pool is a discrete optimisation tracked as a follow-up — see
 * `docs/ROADMAP.md` 1.16c3 notes.
 *
 * Even though the call is synchronous, this helper returns a
 * `Promise` so future-us can switch to a Worker without churning
 * every call site.
 */
import { loadWasm } from "../wasm";
import type { PowProofPayload } from "./wire";

/**
 * Solve the PoW for the given challenge and return a proof object
 * ready to drop into the `pow` field of any expensive auth body.
 *
 * @param seedB64       base64-encoded 16-byte server seed
 * @param effortBits    leading zero bits required on
 *                      SHA-256(seed || nonce || solution); the server
 *                      controls this knob via `LAKKO_POW_EFFORT_BITS`.
 */
export async function solvePow(
  seedB64: string,
  effortBits: number,
): Promise<PowProofPayload> {
  const wasm = await loadWasm();
  // Blocking call. See file-level docstring for why this is OK
  // here (and why it is not OK forever).
  const proof = wasm.PowSolver.solve(seedB64, effortBits);
  return {
    seed: proof.seedB64,
    nonce: proof.nonceB64,
    solution: proof.solutionB64,
  };
}
