// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `AuthClient.recallFinish` and `recallStatus` against a scripted `fetch`
 * (ADR-0074 "T5 as built", items 24 and 25): a lost answer is asked again
 * with the BYTE-IDENTICAL request, a `statement-stale` answer is re-signed at
 * the server's time once, and no other answer is retried.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AuthClient, AuthHttpError, AuthNetworkError, type RecallPendingFinish } from "./client";
import type { RecallFinishRequest } from "./wire";

const ATTESTATION_B64URL = "A".repeat(194); // 145 bytes, URL-safe base64 without padding

function okBody(): string {
  return JSON.stringify({
    key_reset_attest_b64: ATTESTATION_B64URL,
    session_token: "v3.token",
    session_expires_at_ms: Date.now() + 900_000,
    refresh_token: "r2.refresh",
    refresh_expires_at_ms: Date.now() + 2_592_000_000,
    key_generation: 4,
    fresh_grant: "k1.grant",
    fresh_grant_expires_at_ms: Date.now() + 600_000,
  });
}

function pending(): RecallPendingFinish & { resigned: number[] } {
  const base = {
    session_id: "attempt-1",
    account_id_b64: "acct",
    anchor_seq: 1,
    login_finish: "ke3",
    password_upload: "pw",
    recovery_upload: "rec",
    new_signature_pubkey_b64: "newkey",
  };
  const resigned: number[] = [];
  return {
    request: { ...base, statement_b64: "signed-now" } as RecallFinishRequest,
    resign: (atMs) => {
      resigned.push(atMs);
      return { ...base, statement_b64: `signed-at-${atMs}` } as RecallFinishRequest;
    },
    exportKey: new Uint8Array(64),
    dispose: () => undefined,
    resigned,
  };
}

type Answer = "network" | { status: number; body: string };
let sent: string[];
let script: Answer[];
const realFetch = globalThis.fetch;

beforeEach(() => {
  sent = [];
  script = [];
  vi.useFakeTimers();
  globalThis.fetch = (async (_url: RequestInfo | URL, init?: RequestInit) => {
    sent.push(String(init?.body ?? ""));
    const next = script.shift() ?? { status: 200, body: okBody() };
    if (next === "network") throw new TypeError("network down");
    return new Response(next.body, {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;
});
afterEach(() => {
  vi.useRealTimers();
  globalThis.fetch = realFetch;
});

/** Run `work` while the resend pauses elapse. */
async function settle<T>(work: Promise<T>): Promise<T> {
  let done = false;
  const guarded = work.finally(() => {
    done = true;
  });
  guarded.catch(() => undefined);
  while (!done) await vi.advanceTimersByTimeAsync(10_000);
  return guarded;
}

describe("recall/finish asked again (T5 item 25)", () => {
  it("a lost answer resends the byte-identical request and lands", async () => {
    script = ["network", { status: 502, body: "" }];
    const p = pending();
    const result = await settle(new AuthClient("http://auth.test").recallFinish(p));
    expect(sent).toHaveLength(3);
    expect(new Set(sent).size).toBe(1);
    expect(JSON.parse(sent[0] as string).statement_b64).toBe("signed-now");
    expect(result.attestation).toHaveLength(145);
    expect(result.session.keyGeneration).toBe(4n);
    expect(p.resigned).toEqual([]);
  });

  it("gives up after the bound with the request intact, so the caller can ask again later", async () => {
    script = ["network", "network", "network", "network", "network"];
    const p = pending();
    await expect(settle(new AuthClient("http://auth.test").recallFinish(p))).rejects.toBeInstanceOf(
      AuthNetworkError,
    );
    expect(sent).toHaveLength(4);
    expect(new Set(sent).size).toBe(1);
    // The same pending value, later: the identical bytes again.
    script = [];
    await settle(new AuthClient("http://auth.test").recallFinish(p));
    expect(sent[4]).toBe(sent[0]);
  });

  it("an answer the server chose is not resent", async () => {
    script = [{ status: 401, body: '{"error":"recall-refused"}' }];
    await expect(settle(new AuthClient("http://auth.test").recallFinish(pending()))).rejects.toMatchObject({
      status: 401,
    });
    expect(sent).toHaveLength(1);
  });
});

describe("statement-stale (T5 item 24)", () => {
  it("finish: re-signed once at the server's time, and the re-signed request is the one kept", async () => {
    script = [{ status: 401, body: '{"error":"statement-stale","server_time_ms":1700000000000}' }];
    const p = pending();
    await settle(new AuthClient("http://auth.test").recallFinish(p));
    expect(p.resigned).toEqual([1700000000000]);
    expect(sent.map((b) => JSON.parse(b).statement_b64)).toEqual(["signed-now", "signed-at-1700000000000"]);
    expect(p.request.statement_b64).toBe("signed-at-1700000000000");
  });

  it("status: re-signed once; a second stale answer surfaces as it is and closes nothing", async () => {
    const stale = { status: 401, body: '{"error":"statement-stale","server_time_ms":5000}' };
    script = [stale, { status: 200, body: '{"anchor_seq":1}' }];
    const at: Array<number | null> = [];
    const sign = (signAtMs: number | null): Uint8Array => {
      at.push(signAtMs);
      return new Uint8Array([1, 2, 3]);
    };
    expect(await new AuthClient("http://auth.test").recallStatus(sign)).toEqual({ anchor_seq: 1 });
    expect(at).toEqual([null, 5000]);
    script = [stale, stale];
    const err = await new AuthClient("http://auth.test").recallStatus(sign).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AuthHttpError);
    expect((err as AuthHttpError).errorCode).toBe("statement-stale");
    expect(sent).toHaveLength(4);
  });
});
