// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  RECALL_FINISH_SENDS,
  isStatementStale,
  recallFinishResend,
  sendRecallFinish,
  sendWithStaleRetry,
  staleServerTimeMs,
} from "./statement-stale";
import { recallAttemptFailure, recallStatusVerdict } from "./recall";

class Http extends Error {
  constructor(readonly status: number, readonly body: string) {
    super("http");
  }
}
const staleOf = (err: unknown): number | null =>
  err instanceof Http ? staleServerTimeMs(err.status, err.body) : null;

describe("statement-stale (ADR-0066 T5 items 17 and 24)", () => {
  it("reads the server's time only from a 401 statement-stale with a usable time", () => {
    expect(staleServerTimeMs(401, '{"error":"statement-stale","server_time_ms":1700000000123}')).toBe(1700000000123);
    expect(staleServerTimeMs(401, '{"error":"recall-refused","server_time_ms":5}')).toBeNull();
    expect(staleServerTimeMs(403, '{"error":"statement-stale","server_time_ms":5}')).toBeNull();
    expect(staleServerTimeMs(401, '{"error":"statement-stale"}')).toBeNull();
    expect(staleServerTimeMs(401, '{"error":"statement-stale","server_time_ms":"5"}')).toBeNull();
    expect(staleServerTimeMs(401, "not json")).toBeNull();
    expect(isStatementStale(401, "statement-stale")).toBe(true);
    expect(isStatementStale(401, "key-generation-superseded")).toBe(false);
  });

  it("re-signs at the server's time and sends exactly once more", async () => {
    const signedAt: Array<number | null> = [];
    const out = await sendWithStaleRetry(async (at) => {
      signedAt.push(at);
      if (at === null) throw new Http(401, '{"error":"statement-stale","server_time_ms":42000}');
      return "ok";
    }, staleOf);
    expect(out).toBe("ok");
    expect(signedAt).toEqual([null, 42000]);
  });

  it("a second stale answer is thrown, never looped", async () => {
    let sends = 0;
    await expect(
      sendWithStaleRetry(async () => {
        sends += 1;
        throw new Http(401, '{"error":"statement-stale","server_time_ms":42000}');
      }, staleOf),
    ).rejects.toBeInstanceOf(Http);
    expect(sends).toBe(2);
  });

  it("every other refusal is not retried", async () => {
    let sends = 0;
    await expect(
      sendWithStaleRetry(async () => {
        sends += 1;
        throw new Http(401, '{"error":"recall-refused"}');
      }, staleOf),
    ).rejects.toBeInstanceOf(Http);
    expect(sends).toBe(1);
  });

  it("a stale answer never closes the recall record and is not a wrong password", () => {
    expect(
      recallStatusVerdict({ status: 401, errorCode: "statement-stale", body: null }, 0),
    ).toEqual({ kind: "unknown", offline: false });
    expect(
      recallAttemptFailure({
        wrongPasswordLocally: false,
        status: 401,
        errorCode: "statement-stale",
        retryAfterSecs: null,
      }),
    ).toEqual({ kind: "error" });
  });
});

describe("recall/finish asked again (ADR-0066 T5 item 25)", () => {
  it("resends only when no answer arrived or the server failed", () => {
    expect(recallFinishResend({ status: null, sends: 1 })).toBe(true);
    expect(recallFinishResend({ status: 502, sends: 1 })).toBe(true);
    expect(recallFinishResend({ status: 401, sends: 1 })).toBe(false);
    expect(recallFinishResend({ status: 400, sends: 1 })).toBe(false);
    expect(recallFinishResend({ status: 429, sends: 1 })).toBe(false);
    expect(recallFinishResend({ status: null, sends: RECALL_FINISH_SENDS })).toBe(false);
  });

  it("a lost answer is asked again with the same request and lands", async () => {
    const bodies: string[] = [];
    const request = '{"session_id":"s","statement_b64":"abc"}';
    let n = 0;
    const out = await sendRecallFinish(
      async () => {
        bodies.push(request);
        n += 1;
        if (n < 3) throw new Http(0, "");
        return "attestation";
      },
      (err) => (err instanceof Http ? (err.status === 0 ? null : err.status) : undefined),
      async () => undefined,
    );
    expect(out).toBe("attestation");
    expect(bodies).toEqual([request, request, request]);
  });

  it("gives up after the bound, and never resends a chosen refusal or a local failure", async () => {
    let n = 0;
    await expect(
      sendRecallFinish(
        async () => {
          n += 1;
          throw new Http(0, "");
        },
        () => null,
        async () => undefined,
      ),
    ).rejects.toBeInstanceOf(Http);
    expect(n).toBe(RECALL_FINISH_SENDS);
    n = 0;
    await expect(
      sendRecallFinish(
        async () => {
          n += 1;
          throw new Http(401, '{"error":"recall-refused"}');
        },
        (err) => (err as Http).status,
        async () => undefined,
      ),
    ).rejects.toBeInstanceOf(Http);
    expect(n).toBe(1);
    n = 0;
    await expect(
      sendRecallFinish(
        async () => {
          n += 1;
          throw new TypeError("bug");
        },
        () => undefined,
        async () => undefined,
      ),
    ).rejects.toBeInstanceOf(TypeError);
    expect(n).toBe(1);
  });
});
