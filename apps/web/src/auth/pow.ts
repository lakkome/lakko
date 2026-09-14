// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * PoW solver glue — wraps the WASM `PowSolver` into a small
 * promise-returning helper.
 *
 * ## Off the main thread
 *
 * `PowSolver.solve` is one synchronous call that does not return until it
 * has found a proof. It used to run on the main thread, on the assumption
 * that the default effort would take well under a second. It does not: a
 * single signup at the production default effort was measured at 224 s with
 * the page frozen the whole time — no repaint, no input, no way to tell the
 * app from a hung tab. Every call now goes to a small pool of module Web
 * Workers (`pow-worker.ts`), so the UI stays live and
 * `PowActivityIndicator` can say that work is in progress.
 *
 * The pool size is small and bounded: several solves can be in flight at
 * once (an identity-bundle upload and a KeyPackage upload are independent),
 * and each worker instantiates its own copy of the wasm core, which costs
 * memory, so there is no point in more workers than spare cores.
 *
 * ## Fallback
 *
 * Environments without `Worker` (the vitest `node` environment) solve on the
 * calling thread, as before. So does a browser whose worker fails to START
 * (the script or the wasm cannot load in a worker): that is a missing
 * capability, and freezing the page is better than failing every PoW-gated
 * request. A solve that fails INSIDE a working worker is a real error (a bad
 * seed, an exhausted solver budget) and is re-thrown, not retried here.
 *
 * The server effort defaults are untouched.
 */
import { loadWasm } from "../wasm";
import type { PowProofPayload } from "./wire";
import type { PowWorkerRequest, PowWorkerResponse } from "./pow-worker";

// ---------------------------------------------------------------------------
// Activity (for the UI)
// ---------------------------------------------------------------------------

type ActivityListener = (inFlight: number) => void;
let inFlight = 0;
const listeners = new Set<ActivityListener>();

function setInFlight(next: number): void {
  inFlight = next;
  for (const listener of listeners) listener(inFlight);
}

/** Subscribe to the number of PoW solves in flight. Called immediately with
 *  the current value. Returns the unsubscribe function. */
export function subscribePowActivity(listener: ActivityListener): () => void {
  listeners.add(listener);
  listener(inFlight);
  return () => {
    listeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------
// Worker pool
// ---------------------------------------------------------------------------

/** Thrown when a worker could not be started or died outside a solve. */
class PowWorkerUnavailable extends Error {
  override name = "PowWorkerUnavailable";
}

interface Slot {
  readonly worker: Worker;
  busy: boolean;
  settle: ((response: PowWorkerResponse | PowWorkerUnavailable) => void) | null;
}

const slots: Slot[] = [];
const queue: Array<() => void> = [];
let workersBroken = false;
let nextRequestId = 1;

function poolSize(): number {
  const cores =
    typeof navigator !== "undefined" && typeof navigator.hardwareConcurrency === "number"
      ? navigator.hardwareConcurrency
      : 2;
  return Math.max(1, Math.min(3, cores - 1));
}

function createSlot(): Slot {
  const worker = new Worker(new URL("./pow-worker.ts", import.meta.url), {
    type: "module",
    name: "lakko-pow",
  });
  const slot: Slot = { worker, busy: false, settle: null };
  worker.onmessage = (ev: MessageEvent<PowWorkerResponse>) => {
    const settle = slot.settle;
    slot.settle = null;
    settle?.(ev.data);
  };
  const fail = (): void => {
    // A worker that errors outside a solve's own try/catch (a script or wasm
    // load failure) is unusable; retire the whole pool and fall back.
    workersBroken = true;
    const settle = slot.settle;
    slot.settle = null;
    settle?.(new PowWorkerUnavailable("PoW worker failed to start"));
    worker.terminate();
    const index = slots.indexOf(slot);
    if (index >= 0) slots.splice(index, 1);
  };
  worker.onerror = (ev) => {
    ev.preventDefault();
    fail();
  };
  worker.onmessageerror = fail;
  return slot;
}

function acquireSlot(): Promise<Slot> {
  return new Promise((resolve, reject) => {
    const attempt = (): void => {
      if (workersBroken) {
        reject(new PowWorkerUnavailable("PoW workers unavailable"));
        return;
      }
      let slot = slots.find((s) => !s.busy);
      if (slot === undefined && slots.length < poolSize()) {
        try {
          slot = createSlot();
          slots.push(slot);
        } catch {
          workersBroken = true;
          reject(new PowWorkerUnavailable("PoW worker could not be created"));
          return;
        }
      }
      if (slot === undefined) {
        queue.push(attempt);
        return;
      }
      slot.busy = true;
      resolve(slot);
    };
    attempt();
  });
}

function releaseSlot(slot: Slot): void {
  slot.busy = false;
  const next = queue.shift();
  next?.();
}

async function solveInWorker(
  seedB64: string,
  effortBits: number,
): Promise<PowProofPayload> {
  const slot = await acquireSlot();
  try {
    const request: PowWorkerRequest = {
      id: nextRequestId++,
      seedB64,
      effortBits,
    };
    const response = await new Promise<PowWorkerResponse | PowWorkerUnavailable>(
      (resolve) => {
        slot.settle = resolve;
        slot.worker.postMessage(request);
      },
    );
    if (response instanceof PowWorkerUnavailable) throw response;
    if (response.kind === "unavailable") {
      workersBroken = true;
      throw new PowWorkerUnavailable(response.message);
    }
    if (response.kind === "error") throw new Error(response.message);
    return response.proof;
  } finally {
    releaseSlot(slot);
    if (workersBroken) {
      // Anything still queued would wait forever for a pool that is gone.
      while (queue.length > 0) queue.shift()?.();
    }
  }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

async function solveOnCallingThread(
  seedB64: string,
  effortBits: number,
): Promise<PowProofPayload> {
  const wasm = await loadWasm();
  // Blocking call — only reached where no worker can run it.
  const proof = wasm.PowSolver.solve(seedB64, effortBits);
  return {
    seed: proof.seedB64,
    nonce: proof.nonceB64,
    solution: proof.solutionB64,
  };
}

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
  setInFlight(inFlight + 1);
  try {
    if (typeof Worker !== "undefined" && !workersBroken) {
      try {
        return await solveInWorker(seedB64, effortBits);
      } catch (err) {
        if (!(err instanceof PowWorkerUnavailable)) throw err;
        // eslint-disable-next-line no-console
        console.warn("PoW worker unavailable; solving on the main thread", err);
      }
    }
    return await solveOnCallingThread(seedB64, effortBits);
  } finally {
    setInFlight(inFlight - 1);
  }
}
