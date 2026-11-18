// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.6 — casting an anonymous ballot off the UI thread: the
 * linkable one-out-of-many proof costs a measurable fraction of a second.
 * The worker gets the poll's keys, the ring, the voter's 32-byte Ed25519
 * seed and the choices, and returns the sealed row; it keeps nothing.
 */
import { loadWasm } from "../wasm";
import type { BallotCastRequest, BallotCastReply } from "./ballot-cast";
import { castBallotWith } from "./ballot-cast";

self.onmessage = (e: MessageEvent<BallotCastRequest>): void => {
  const req = e.data;
  void (async (): Promise<void> => {
    let reply: BallotCastReply;
    try {
      const wasm = await loadWasm();
      reply = { id: req.id, ...castBallotWith(wasm, req) };
    } catch (err) {
      reply = { id: req.id, error: err instanceof Error ? err.message : "cast failed" };
    } finally {
      req.voterSeed.fill(0);
    }
    (self as unknown as Worker).postMessage(reply);
  })();
};
