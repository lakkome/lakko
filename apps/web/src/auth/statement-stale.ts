// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `401 {"error":"statement-stale","server_time_ms":T}` and the lost answer of
 * `recall/finish` (ADR-0066 "T5 as built", items 17, 24 and 25), as pure
 * rules. The network and the wasm signer are the caller's.
 *
 * An account statement is good for 120 s either side of the server's clock.
 * The server answers `statement-stale` ONLY when the statement's signature
 * verified and its time alone is off, and tells its own time; nothing was
 * counted and nothing was consumed. The device signs again at that time and
 * sends once more. It is never a refusal of the device: a recall record is
 * not destroyed for it and a kept device does not read it as a reset.
 */

export const CODE_STATEMENT_STALE = "statement-stale";

/** The server's time out of a `statement-stale` answer, or `null` for every
 *  other answer (and for a stale answer without a usable time). */
export function staleServerTimeMs(status: number, body: string): number | null {
  if (status !== 401) return null;
  try {
    const parsed = JSON.parse(body) as { error?: unknown; server_time_ms?: unknown };
    if (parsed.error !== CODE_STATEMENT_STALE) return null;
    const t = parsed.server_time_ms;
    return typeof t === "number" && Number.isFinite(t) && t > 0 ? Math.trunc(t) : null;
  } catch {
    return null;
  }
}

/** Whether an answer is `statement-stale` at all (with or without a time). */
export function isStatementStale(status: number, errorCode: string | null): boolean {
  return status === 401 && errorCode === CODE_STATEMENT_STALE;
}

/**
 * Send a request that carries an account statement; when it is answered
 * `statement-stale` with the server's time, sign again AT that time and send
 * ONCE more. `send(null)` signs at the device's clock. A second stale answer
 * is thrown as it is (no loop).
 */
export async function sendWithStaleRetry<T>(
  send: (signAtMs: number | null) => Promise<T>,
  staleTimeOf: (err: unknown) => number | null,
): Promise<T> {
  try {
    return await send(null);
  } catch (err) {
    const serverTimeMs = staleTimeOf(err);
    if (serverTimeMs === null) throw err;
    return send(serverTimeMs);
  }
}

/** How many times the byte-identical `recall/finish` is sent in all before
 *  the attempt is reported as unanswered. Chosen, not measured. */
export const RECALL_FINISH_SENDS = 4;

/** The pause before resend number `n` (1-based), milliseconds. */
export function recallFinishBackoffMs(resend: number): number {
  return Math.min(8000, 1000 * 2 ** Math.max(0, resend - 1));
}

/**
 * Whether a failed `recall/finish` is sent again with the SAME bytes: only
 * when no answer arrived, or the server failed (`5xx`). Every answer the
 * server chose (`401`, `400`, `429`) stands. A successful first finish whose
 * answer was lost answers `200` again to the identical request within the
 * attempt's ten minutes.
 */
export function recallFinishResend(input: {
  /** `null` when no answer arrived. */
  readonly status: number | null;
  /** Sends made so far, this one included. */
  readonly sends: number;
}): boolean {
  if (input.sends >= RECALL_FINISH_SENDS) return false;
  return input.status === null || input.status >= 500;
}

/**
 * Send `request` (already serialised: the caller must not rebuild it) until
 * it is answered or the resend rule says stop.
 */
export async function sendRecallFinish<T>(
  sendOnce: () => Promise<T>,
  statusOf: (err: unknown) => number | null | undefined,
  sleep: (ms: number) => Promise<void>,
): Promise<T> {
  let sends = 0;
  for (;;) {
    sends += 1;
    try {
      return await sendOnce();
    } catch (err) {
      const status = statusOf(err);
      // `undefined`: not a transport or HTTP failure at all; never resent.
      if (status === undefined || !recallFinishResend({ status, sends })) throw err;
      await sleep(recallFinishBackoffMs(sends));
    }
  }
}
