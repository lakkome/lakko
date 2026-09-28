// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S3 — the anonymous-token pool: refill, take, the retry-once on a
 * refused token, clearing on sign-out and account switch, the issuance
 * verified against the key route's key (never the response's), and the
 * refusals that must never read as a lost session.
 */
import { describe, expect, it } from "vitest";

import {
  AnonTokenQuotaExhaustedError,
  AnonTokensDisabledError,
  AnonTokenUnavailableError,
  LOW_WATER,
  REFILL_BATCH,
  anonTokenQuotaExhausted,
  anonymousFetchInit,
  isAnonTokenNetworkFailure,
} from "./anon-token-pool";
import { FAKE_ISSUER_KEY, fakeTokenPool } from "./anon-token-test-doubles";

/** Let background refills settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function issuances(issuer: ReturnType<typeof fakeTokenPool>["issuer"]) {
  return issuer.filter((r) => r.url === "/api/auth/auth/tokens");
}

describe("refill and take", () => {
  it("an empty pool waits for a refill drawn WITH the session, then hands out a token", async () => {
    const { pool, issuer } = fakeTokenPool();
    const token = await pool.take("discovery-lookup");
    expect(token.headerValue).toBe("tok-1");
    const [first] = issuances(issuer);
    expect(first?.method).toBe("POST");
    expect(first?.authorization).toBe("Bearer v1.QUNDT1VOVA.0.tag");
    const body = first?.body as { context: string; epoch: number; blinded_elements: string[] };
    expect(body.context).toBe("discovery-lookup");
    expect(body.blinded_elements).toHaveLength(REFILL_BATCH["discovery-lookup"]);
    // The epoch is the local day number (1_700_000_000 s / 86_400 s).
    expect(body.epoch).toBe(19675);
    expect(pool.size("discovery-lookup")).toBe(REFILL_BATCH["discovery-lookup"] - 1);
  });

  it("refills in the background once a context drops below the low-water mark", async () => {
    const { pool, issuer } = fakeTokenPool();
    await pool.take("ds-knock");
    expect(issuances(issuer)).toHaveLength(1);
    // Take down to LOW_WATER - 1 held: the next take starts a refill.
    while (pool.size("ds-knock") >= LOW_WATER["ds-knock"]) await pool.take("ds-knock");
    await settle();
    expect(issuances(issuer)).toHaveLength(2);
    expect(pool.size("ds-knock")).toBeGreaterThanOrEqual(REFILL_BATCH["ds-knock"]);
  });

  it("contexts are separate buckets", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("push-register");
    expect(pool.size("push-register")).toBe(REFILL_BATCH["push-register"] - 1);
    expect(pool.size("invite-consume")).toBe(0);
  });

  it("without a session there is nothing to draw with: fails without a request", async () => {
    const { pool, issuer } = fakeTokenPool();
    pool.clear();
    await expect(pool.take("discovery-lookup")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuer).toHaveLength(0);
  });
});

describe("the issuance is verified against the key route's key", () => {
  it("fetches the key anonymously and finalizes under IT, not the response's public_key", async () => {
    const { pool, issuer, wasm } = fakeTokenPool({ responseKey: "a-key-from-the-response" });
    await pool.take("invite-consume");
    expect(wasm.finalizeKeys).toEqual([FAKE_ISSUER_KEY]);
    const keyFetch = issuer.find((r) => r.url.includes("/auth/tokens/key/"));
    expect(keyFetch?.url).toBe("/api/auth/auth/tokens/key/invite-consume/19675");
    expect(keyFetch?.authorization).toBeUndefined();
    expect(keyFetch?.credentials).toBe("omit");
  });

  it("a proof that does not verify under the published key discards the whole batch", async () => {
    // The issuer evaluated under one key; the key route publishes another.
    // The response's own `public_key` would verify - using it would let an
    // issuer tag this account with a key of its own.
    const { pool, wasm } = fakeTokenPool({
      keyRouteKey: "the-published-key",
      evaluatedUnder: "a-tagging-key",
      responseKey: "a-tagging-key",
    });
    const err = await pool.take("discovery-lookup").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AnonTokenUnavailableError);
    expect(String((err as Error).message)).toMatch(/batch discarded/);
    expect(wasm.finalizeKeys).toEqual(["the-published-key"]);
    expect(pool.size("discovery-lookup")).toBe(0);
  });

  it("caches the key for its epoch", async () => {
    const { pool, issuer } = fakeTokenPool();
    await pool.take("discovery-register");
    while (pool.size("discovery-register") >= LOW_WATER["discovery-register"]) await pool.take("discovery-register");
    await settle();
    expect(issuer.filter((r) => r.url.includes("/auth/tokens/key/"))).toHaveLength(1);
    expect(issuances(issuer)).toHaveLength(2);
  });
});

describe("issuer refusals", () => {
  it("429 anon-token-quota-exhausted is the allowance used up, with Retry-After; never a sign-out", async () => {
    const { pool, issuer } = fakeTokenPool({
      issueScript: [
        {
          status: 429,
          json: { error: "anon-token-quota-exhausted", remaining: 0, retry_after_secs: 3600 },
          retryAfter: "3600",
        },
      ],
    });
    const err = await pool.take("discovery-lookup").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AnonTokenQuotaExhaustedError);
    expect((err as AnonTokenQuotaExhaustedError).retryAfterSecs).toBe(3600);
    expect(anonTokenQuotaExhausted({ cause: err })).toBe(err);
    // Known exhausted: the next take does not ask again.
    await expect(pool.take("discovery-lookup")).rejects.toBeInstanceOf(
      AnonTokenQuotaExhaustedError,
    );
    expect(issuances(issuer)).toHaveLength(1);
  });

  it("503 anon-tokens-disabled is the generic network failure", async () => {
    const { pool } = fakeTokenPool({
      issueScript: [{ status: 503, json: { error: "anon-tokens-disabled" } }],
    });
    const err = await pool.take("push-register").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AnonTokensDisabledError);
    expect(isAnonTokenNetworkFailure(err)).toBe(true);
  });

  it("400 epoch-not-issuable rebuilds the batch for the server's epoch, once", async () => {
    const { pool, issuer } = fakeTokenPool({
      issueScript: [{ status: 400, json: { error: "epoch-not-issuable", current_epoch: 19676 } }],
    });
    const token = await pool.take("ds-knock");
    expect(token.epoch).toBe(19676n);
    const bodies = issuances(issuer).map((r) => (r.body as { epoch: number }).epoch);
    expect(bodies).toEqual([19675, 19676]);
  });

  it("a second epoch disagreement gives up rather than looping", async () => {
    const { pool, issuer } = fakeTokenPool({
      issueScript: [
        { status: 400, json: { error: "epoch-not-issuable", current_epoch: 19676 } },
        { status: 400, json: { error: "epoch-not-issuable", current_epoch: 19677 } },
      ],
    });
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuances(issuer)).toHaveLength(2);
  });

  it("a 401 from the issuer is not a sign-out: it is only 'no token'", async () => {
    const { pool } = fakeTokenPool({ issueScript: [{ status: 401, json: {} }] });
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
  });
});

describe("spend: drop a refused token and retry ONCE", () => {
  function route(statuses: Array<{ status: number; error?: string }>) {
    const seen: string[] = [];
    const send = async (name: string, value: string): Promise<Response> => {
      expect(name).toBe("x-lakko-anon-token");
      seen.push(value);
      const next = statuses.shift() ?? { status: 200 };
      return new Response(
        JSON.stringify(next.error === undefined ? { ok: true } : { error: next.error }),
        { status: next.status },
      );
    };
    return { seen, send };
  }

  it.each([
    "anon-token-spent",
    "anon-token-invalid",
    "anon-token-expired",
    "anon-token-wrong-context",
    "anon-token-malformed",
  ])("%s: takes another token and retries once", async (code) => {
    const { pool } = fakeTokenPool();
    const r = route([{ status: 401, error: code }, { status: 200 }]);
    const response = await pool.spend("discovery-lookup", r.send);
    expect(response.status).toBe(200);
    expect(r.seen).toEqual(["tok-1", "tok-2"]);
  });

  it("a second refusal is returned, not retried again", async () => {
    const { pool } = fakeTokenPool();
    const r = route([
      { status: 401, error: "anon-token-spent" },
      { status: 401, error: "anon-token-spent" },
    ]);
    const response = await pool.spend("discovery-lookup", r.send);
    expect(response.status).toBe(401);
    expect(r.seen).toHaveLength(2);
  });

  it("anon-token-required is a client bug: surfaced, not retried", async () => {
    const { pool } = fakeTokenPool();
    const r = route([{ status: 401, error: "anon-token-required" }]);
    const response = await pool.spend("discovery-lookup", r.send);
    expect(response.status).toBe(401);
    expect(r.seen).toHaveLength(1);
  });

  it("503 anon-tokens-disabled from the route throws the disabled error", async () => {
    const { pool } = fakeTokenPool();
    const r = route([{ status: 503, error: "anon-tokens-disabled" }]);
    await expect(pool.spend("push-register", r.send)).rejects.toBeInstanceOf(
      AnonTokensDisabledError,
    );
  });

  it("the caller can still read the body of a returned refusal", async () => {
    const { pool } = fakeTokenPool();
    const r = route([{ status: 401, error: "anon-token-required" }]);
    const response = await pool.spend("ds-knock", r.send);
    await expect(response.json()).resolves.toEqual({ error: "anon-token-required" });
  });
});

describe("clearing on logout and account switch", () => {
  it("signing out keeps the held tokens for the push unregister; clearIfNoSession then drops them", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("push-register");
    pool.setSessionToken(null);
    expect(pool.size("push-register")).toBe(REFILL_BATCH["push-register"] - 1);
    const held = await pool.take("push-register");
    expect(held.headerValue).toBe("tok-2");
    pool.clearIfNoSession();
    expect(pool.size("push-register")).toBe(0);
    await expect(pool.take("push-register")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
  });

  it("clearIfNoSession leaves a session installed in the meantime alone", async () => {
    const { pool } = fakeTokenPool();
    pool.setSessionToken(null);
    pool.setSessionToken("v1.QUNDT1VOVA.0.tag");
    await pool.take("discovery-lookup");
    pool.clearIfNoSession();
    expect(pool.size("discovery-lookup")).toBe(REFILL_BATCH["discovery-lookup"] - 1);
  });

  it("another account's session clears the pool", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("discovery-lookup");
    pool.setSessionToken("v1.QU5PVEhFUg.0.tag");
    expect(pool.size("discovery-lookup")).toBe(0);
  });

  it("the same account's renewed session keeps the pool", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("discovery-lookup");
    pool.setSessionToken("v1.QUNDT1VOVA.999.other");
    expect(pool.size("discovery-lookup")).toBe(REFILL_BATCH["discovery-lookup"] - 1);
  });

  it("a refill that lands after a clear is thrown away", async () => {
    const { pool } = fakeTokenPool();
    const pending = pool.take("invite-consume").catch((e: unknown) => e);
    pool.clear();
    const outcome = await pending;
    expect(outcome).toBeInstanceOf(AnonTokenUnavailableError);
    expect(pool.size("invite-consume")).toBe(0);
  });
});

describe("anonymousFetchInit", () => {
  it("omits credentials and the referrer", () => {
    const init = anonymousFetchInit({ method: "POST", headers: { "x-lakko-anon-token": "t" } });
    expect(init.credentials).toBe("omit");
    expect(init.referrerPolicy).toBe("no-referrer");
    expect(init.headers).toEqual({ "x-lakko-anon-token": "t" });
  });

  it("refuses an Authorization or Cookie header instead of carrying it", () => {
    expect(() =>
      anonymousFetchInit({ method: "POST", headers: { Authorization: "Bearer x" } }),
    ).toThrow();
    expect(() => anonymousFetchInit({ method: "POST", headers: { cookie: "a=b" } })).toThrow();
  });
});
