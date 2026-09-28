// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Proof-of-work solver glue — wraps the WASM `PowSolver` into a small,
 * cancellable, promise-returning helper.
 *
 * Since ADR-0054 S4 exactly one request pays a proof of work: `/signup/start`
 * (the ordinary signup and the guest signup of a call link). Discovery, the
 * DS and push-gw take anonymous tokens instead, and every other auth route is
 * limited server-side.
 *
 * ## Off the main thread, and cancellable
 *
 * `PowSolver.solve` is one synchronous call that does not return until it has
 * found a proof, and at the signup effort it runs for a noticeable time. Each
 * solve gets its own module Web Worker (`pow-worker.ts`), created for the
 * solve and terminated when it ends. Terminating is also how a solve is
 * cancelled: the wasm call cannot be interrupted from inside, but a worker can
 * be killed from outside. Leaving the signup screen, or tapping `Cancel`,
 * aborts the signal and the worker dies with the solve (docs/ux/anti-abuse.md
 * section 1).
 *
 * ## Chunks (ADR-0054 S5)
 *
 * The search runs as a loop of `PowSolver.solveChunk` calls
 * ({@link solveInChunks}): at most {@link POW_CHUNK_ATTEMPTS} nonces per call,
 * `undefined` when none of them produced a proof, the cancel signal checked
 * between calls. In the worker the loop is only the search (cancelling
 * terminates the worker, which is immediate and needs no cooperation). On the
 * calling thread it is what makes the fallback cancellable at all: the loop
 * yields to the event loop between chunks, so a `Cancel` tap and leaving the
 * screen are seen within one chunk and the CPU work stops there.
 *
 * The chunk is one attempt. Measured on the maintainer's Mac (node, this wasm
 * build): one Equix attempt takes about 260 ms, so no chunk can meet the
 * brief's "about 100 ms" and the smallest one is the closest; a phone is
 * slower still. Nothing is lost by the small chunk: each call samples fresh
 * nonces, so the loop is the same search as one `solve`.
 *
 * ## Fallback
 *
 * Environments without `Worker` (the vitest `node` environment) solve on the
 * calling thread. So does a browser whose worker fails to START (the script
 * or the wasm cannot load in a worker): that is a missing capability, and a
 * frozen page is better than no signup at all. A solve that fails INSIDE a
 * working worker is a real error (a bad seed, an exhausted solver budget) and
 * is re-thrown, not retried here.
 */
import { loadWasm } from "../wasm";
import type { PowProofPayload } from "./wire";
import type { PowWorkerRequest, PowWorkerResponse } from "./pow-worker";
import { solveInChunks, type PowChunkSolver } from "./pow-chunks";

export {
  POW_CHUNK_ATTEMPTS,
  POW_MAX_ATTEMPTS,
  solveInChunks,
  type PowChunkSolver,
  type WasmPowProof,
} from "./pow-chunks";

/** A macrotask yield: lets the page run input and timers between chunks. */
function macrotask(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** Tests only: replace the calling thread's chunk solver (`null` = wasm). */
let chunkSolverOverride: PowChunkSolver | null = null;
export function setPowChunkSolverForTests(solver: PowChunkSolver | null): void {
  chunkSolverOverride = solver;
}

/** Thrown when a worker could not be started. */
class PowWorkerUnavailable extends Error {
  override name = "PowWorkerUnavailable";
}

/** Read through a function so TypeScript does not narrow `aborted` across
 *  the blocking call in between two checks. */
function isAborted(signal: AbortSignal | undefined): boolean {
  return signal !== undefined && signal.aborted;
}

/** The standard abort error a cancelled solve rejects with. */
function abortError(): DOMException {
  return new DOMException("proof of work cancelled", "AbortError");
}

/** The subset of `Worker` this module drives (a test can supply a fake). */
export interface PowWorkerLike {
  onmessage: ((ev: MessageEvent<PowWorkerResponse>) => void) | null;
  onerror: ((ev: ErrorEvent) => void) | null;
  onmessageerror: ((ev: MessageEvent) => void) | null;
  postMessage(message: PowWorkerRequest): void;
  terminate(): void;
}

let workerFactory: (() => PowWorkerLike) | null =
  typeof Worker === "undefined"
    ? null
    : () =>
        new Worker(new URL("./pow-worker.ts", import.meta.url), {
          type: "module",
          name: "lakko-pow",
        }) as unknown as PowWorkerLike;
let workersBroken = false;
let nextRequestId = 1;

/** Tests only: replace how a solve worker is made (`null` = no workers). */
export function setPowWorkerFactoryForTests(factory: (() => PowWorkerLike) | null): void {
  workerFactory = factory;
  workersBroken = false;
}

function solveInWorker(
  make: () => PowWorkerLike,
  seedB64: string,
  effortBits: number,
  signal: AbortSignal | undefined,
): Promise<PowProofPayload> {
  return new Promise((resolve, reject) => {
    let worker: PowWorkerLike;
    try {
      worker = make();
    } catch {
      reject(new PowWorkerUnavailable("PoW worker could not be created"));
      return;
    }
    let settled = false;
    const finish = (): void => {
      settled = true;
      signal?.removeEventListener("abort", onAbort);
      worker.terminate();
    };
    const onAbort = (): void => {
      if (settled) return;
      finish();
      reject(abortError());
    };
    signal?.addEventListener("abort", onAbort);
    worker.onmessage = (ev) => {
      if (settled) return;
      finish();
      const response = ev.data;
      if (response.kind === "proof") resolve(response.proof);
      else if (response.kind === "unavailable") reject(new PowWorkerUnavailable(response.message));
      else reject(new Error(response.message));
    };
    const failed = (): void => {
      if (settled) return;
      finish();
      reject(new PowWorkerUnavailable("PoW worker failed to start"));
    };
    worker.onerror = (ev) => {
      ev.preventDefault();
      failed();
    };
    worker.onmessageerror = failed;
    worker.postMessage({ id: nextRequestId++, seedB64, effortBits });
  });
}

async function solveOnCallingThread(
  seedB64: string,
  effortBits: number,
  signal: AbortSignal | undefined,
): Promise<PowProofPayload> {
  let solveChunk = chunkSolverOverride;
  if (solveChunk === null) {
    const wasm = await loadWasm();
    solveChunk = (seed, effort, max) => wasm.PowSolver.solveChunk(seed, effort, max);
  }
  // Only reached where no worker can run it: chunked, yielding between
  // chunks, so a cancel stops the work within one chunk.
  return solveInChunks(solveChunk, seedB64, effortBits, {
    ...(signal !== undefined ? { signal } : {}),
    yieldBetween: macrotask,
  });
}

/**
 * Solve the PoW for the given challenge and return a proof object ready to
 * drop into the `pow` field of `/signup/start`.
 *
 * @param seedB64     base64-encoded 16-byte server seed
 * @param effortBits  leading zero bits required on
 *                    SHA-256(seed || nonce || solution); the server
 *                    controls this knob (`LAKKO_SIGNUP_POW_EFFORT_BITS`).
 * @param signal      aborting it terminates the solve's worker and rejects
 *                    with a `DOMException` named `AbortError`.
 */
export async function solvePow(
  seedB64: string,
  effortBits: number,
  signal?: AbortSignal,
): Promise<PowProofPayload> {
  if (isAborted(signal)) throw abortError();
  const make = workerFactory;
  if (make !== null && !workersBroken) {
    try {
      return await solveInWorker(make, seedB64, effortBits, signal);
    } catch (err) {
      if (!(err instanceof PowWorkerUnavailable)) throw err;
      workersBroken = true;
      // eslint-disable-next-line no-console
      console.warn("PoW worker unavailable; solving on the main thread", err);
    }
  }
  return solveOnCallingThread(seedB64, effortBits, signal);
}

/** `true` when `err` is the abort a cancelled solve (or signup) throws. */
export function isAbortError(err: unknown): boolean {
  return (
    typeof err === "object" &&
    err !== null &&
    (err as { name?: unknown }).name === "AbortError"
  );
}
