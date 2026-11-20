// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 3.6 — one anonymous ballot cast, the same code on the
 * worker and (where no worker can be started) on the page.
 */
import { loadWasm } from "../wasm";

type Wasm = Awaited<ReturnType<typeof loadWasm>>;

export interface BallotCastRequest {
  readonly id: number;
  readonly groupId: Uint8Array;
  readonly pollId: Uint8Array;
  readonly secret: Uint8Array;
  /** `PollRing.encode()`. */
  readonly ring: Uint8Array;
  /** The 32-byte Ed25519 seed of the key this account's leaf is bound to. */
  readonly voterSeed: Uint8Array;
  readonly choices: Uint8Array;
  /** The recorded set version on a revote; `undefined` on the first vote. */
  readonly setVersion: number | undefined;
}

export type BallotCastReply =
  | { readonly id: number; readonly row: Uint8Array; readonly setVersion: number }
  | { readonly id: number; readonly error: string };

export function castBallotWith(
  wasm: Wasm,
  req: BallotCastRequest,
): { row: Uint8Array; setVersion: number } {
  const keys = new wasm.PollBallotKeys(req.groupId, req.pollId, req.secret);
  const ring = wasm.PollRing.decode(req.ring);
  const voter = wasm.PollVoterKey.fromEd25519Seed(req.voterSeed);
  try {
    const cast = wasm.castPollBallot(keys, ring, voter, req.choices, req.setVersion ?? null);
    try {
      return { row: cast.row, setVersion: cast.setVersion };
    } finally {
      cast.free();
    }
  } finally {
    voter.free();
    ring.free();
    keys.free();
  }
}

let worker: Worker | null = null;
let nextId = 1;
const waiting = new Map<number, (r: BallotCastReply) => void>();

function ballotWorker(): Worker | null {
  if (worker !== null) return worker;
  if (typeof Worker === "undefined") return null;
  try {
    worker = new Worker(new URL("./ballot-worker.ts", import.meta.url), { type: "module", name: "lakko-ballot" });
    worker.onmessage = (e: MessageEvent<BallotCastReply>): void => {
      const done = waiting.get(e.data.id);
      waiting.delete(e.data.id);
      done?.(e.data);
    };
    worker.onerror = (): void => {
      for (const [id, done] of waiting) done({ id, error: "worker failed" });
      waiting.clear();
      worker?.terminate();
      worker = null;
    };
    return worker;
  } catch {
    return null;
  }
}

/** Cast off the UI thread; on the page only where no worker starts. */
export async function castBallot(req: Omit<BallotCastRequest, "id">): Promise<{ row: Uint8Array; setVersion: number }> {
  const w = ballotWorker();
  if (w === null) {
    const wasm = await loadWasm();
    try {
      return castBallotWith(wasm, { ...req, id: 0 });
    } finally {
      req.voterSeed.fill(0);
    }
  }
  const id = nextId++;
  const reply = await new Promise<BallotCastReply>((resolve) => {
    waiting.set(id, resolve);
    w.postMessage({ ...req, id });
    // The worker holds its own copy; this one is not needed any more.
    req.voterSeed.fill(0);
  });
  if ("error" in reply) throw new Error(reply.error);
  return { row: reply.row, setVersion: reply.setVersion };
}
