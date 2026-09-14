// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Proof-of-work worker — runs the blocking `PowSolver.solve` off the main
 * thread. Companion to `./pow.ts`, which owns the pool, the queue and the
 * main-thread fallback; this script only answers one request at a time.
 *
 * Loads the same wasm core as the page (`vite.config.ts` repeats
 * `vite-plugin-wasm` under `worker.plugins`, exactly as for the SFrame
 * transform worker). The proof is public data — a seed the server issued, a
 * nonce and a solution — so nothing secret crosses the message boundary.
 */
import { loadWasm } from "../wasm";
import type { PowProofPayload } from "./wire";

export interface PowWorkerRequest {
  readonly id: number;
  readonly seedB64: string;
  readonly effortBits: number;
}

export type PowWorkerResponse =
  | { readonly id: number; readonly kind: "proof"; readonly proof: PowProofPayload }
  /** The solve itself failed (a bad seed, an exhausted solver budget). */
  | { readonly id: number; readonly kind: "error"; readonly message: string }
  /** The wasm core could not be loaded in this worker — a missing
   *  capability, which the pool answers with the main-thread fallback. */
  | { readonly id: number; readonly kind: "unavailable"; readonly message: string };

// Guarded so the type-only import from `pow.ts` stays harmless if this module
// is ever evaluated outside a worker.
const scope = globalThis as unknown as {
  onmessage: ((ev: MessageEvent<PowWorkerRequest>) => void) | null;
  postMessage: (message: PowWorkerResponse) => void;
  WorkerGlobalScope?: unknown;
};

if (typeof scope.WorkerGlobalScope !== "undefined") {
  scope.onmessage = (ev: MessageEvent<PowWorkerRequest>) => {
    const { id, seedB64, effortBits } = ev.data;
    void (async (): Promise<void> => {
      let wasm: Awaited<ReturnType<typeof loadWasm>>;
      try {
        wasm = await loadWasm();
      } catch (err) {
        scope.postMessage({ id, kind: "unavailable", message: messageOf(err) });
        return;
      }
      try {
        const proof = wasm.PowSolver.solve(seedB64, effortBits);
        const payload: PowProofPayload = {
          seed: proof.seedB64,
          nonce: proof.nonceB64,
          solution: proof.solutionB64,
        };
        proof.free();
        scope.postMessage({ id, kind: "proof", proof: payload });
      } catch (err) {
        scope.postMessage({ id, kind: "error", message: messageOf(err) });
      }
    })();
  };
}

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
