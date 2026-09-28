// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chunked proof-of-work search (ADR-0054 S5), shared by the solve worker
 * (`pow-worker.ts`) and the calling-thread fallback in `pow.ts`. Kept in its
 * own module so the worker bundle does not pull in `pow.ts`, which itself
 * references the worker script.
 */
import type { PowProofPayload } from "./wire";

/** Read through a function so TypeScript does not narrow `aborted`. */
function isAborted(signal: AbortSignal | undefined): boolean {
  return signal !== undefined && signal.aborted;
}

/** Nonces per `solveChunk` call. One: a single attempt already takes about
 *  260 ms in this wasm on a Mac (see the module doc). */
export const POW_CHUNK_ATTEMPTS = 1;
/** The whole search's budget, the same as `lakko_pow::DEFAULT_MAX_ATTEMPTS`
 *  behind `PowSolver.solve`. */
export const POW_MAX_ATTEMPTS = 1 << 16;

/** One proof as the wasm core returns it. */
export interface WasmPowProof {
  readonly seedB64: string;
  readonly nonceB64: string;
  readonly solutionB64: string;
  free(): void;
}

/** The chunked solver (`PowSolver.solveChunk`); injectable for tests. */
export type PowChunkSolver = (
  seedB64: string,
  effortBits: number,
  maxAttempts: number,
) => WasmPowProof | undefined;

/**
 * Run the search as a loop of chunks. `signal` is checked before every
 * chunk; `yieldBetween` (the calling-thread fallback passes a macrotask
 * yield) runs between chunks so the page can process a `Cancel`. Throws the
 * standard abort error when cancelled, and an `Error` when the whole budget
 * found nothing.
 */
export async function solveInChunks(
  solveChunk: PowChunkSolver,
  seedB64: string,
  effortBits: number,
  options: {
    readonly signal?: AbortSignal;
    readonly yieldBetween?: () => Promise<void>;
    readonly chunkAttempts?: number;
    readonly maxAttempts?: number;
  } = {},
): Promise<PowProofPayload> {
  const chunk = Math.max(1, options.chunkAttempts ?? POW_CHUNK_ATTEMPTS);
  const budget = options.maxAttempts ?? POW_MAX_ATTEMPTS;
  for (let spent = 0; spent < budget; spent += chunk) {
    if (isAborted(options.signal)) {
      throw new DOMException("proof of work cancelled", "AbortError");
    }
    const proof = solveChunk(seedB64, effortBits, Math.min(chunk, budget - spent));
    if (proof !== undefined) {
      const payload = {
        seed: proof.seedB64,
        nonce: proof.nonceB64,
        solution: proof.solutionB64,
      };
      proof.free();
      return payload;
    }
    if (options.yieldBetween !== undefined) await options.yieldBetween();
  }
  throw new Error("PoW budget exhausted (max attempts reached)");
}
