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
  ANON_TOKEN_CONTEXTS,
  AnonTokensDisabledError,
  AnonTokenUnavailableError,
  POOL_TARGET,
  REFILL_INTERVAL_MAX_MS,
  REFILL_INTERVAL_MIN_MS,
  anonTokenQuotaExhausted,
  anonTokenRefillOnDemandInThisBuild,
  anonymousFetchInit,
  isAnonTokenNetworkFailure,
} from "./anon-token-pool";
import { FAKE_ISSUER_KEY, fakeTokenPool } from "./anon-token-test-doubles";

/** Let background refills settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function issuances(issuer: ReturnType<typeof fakeTokenPool>["issuer"], context?: string) {
  return issuer.filter(
    (r) =>
      r.url === "/api/auth/auth/tokens" &&
      (context === undefined || (r.body as { context?: string }).context === context),
  );
}

describe("a take on an empty bucket: the release rule and a test build", () => {
  async function emptied(refillOnDemand?: boolean) {
    const made = fakeTokenPool(refillOnDemand === undefined ? {} : { refillOnDemand });
    await made.pool.take("ds-knock");
    await settle();
    while (made.pool.size("ds-knock") > 0) await made.pool.take("ds-knock");
    return made;
  }

  it("release: a take never starts an issuance", async () => {
    const { pool, issuer } = await emptied();
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuances(issuer, "ds-knock")).toHaveLength(1);
  });

  it("test build: the take starts the issuance and waits for it", async () => {
    const { pool, issuer } = await emptied(true);
    const token = await pool.take("ds-knock");
    expect(token.context).toBe("ds-knock");
    expect(issuances(issuer, "ds-knock")).toHaveLength(2);
  });

  it("test build: takes at once share one issuance", async () => {
    const { pool, issuer } = await emptied(true);
    await Promise.all([pool.take("ds-knock"), pool.take("ds-knock")]);
    expect(issuances(issuer, "ds-knock")).toHaveLength(2);
  });

  it("the build rule reads the on-demand flag, and this test build is not on-demand", () => {
    expect(anonTokenRefillOnDemandInThisBuild()).toBe(false);
  });
});

describe("refill on a schedule, take without drawing (ADR-0078 section 8)", () => {
  it("the session's first refill draws a day of use WITH the session; a take waits for it", async () => {
    const { pool, issuer } = fakeTokenPool();
    const token = await pool.take("discovery-lookup");
    expect(token.headerValue).toBe("tok-1");
    const [first] = issuances(issuer, "discovery-lookup");
    expect(first?.method).toBe("POST");
    expect(first?.authorization).toBe("Bearer v1.QUNDT1VOVA.0.tag");
    const body = first?.body as { context: string; epoch: number; blinded_elements: string[] };
    expect(body.blinded_elements).toHaveLength(POOL_TARGET["discovery-lookup"]);
    // The epoch is the local day number (1_700_000_000 s / 86_400 s).
    expect(body.epoch).toBe(19675);
    expect(pool.size("discovery-lookup")).toBe(POOL_TARGET["discovery-lookup"] - 1);
  });

  it("the token pool refills on a timer, not on spend", async () => {
    const { pool, issuer, timers } = fakeTokenPool();
    await pool.take("ds-knock");
    await settle();
    expect(issuances(issuer, "ds-knock")).toHaveLength(1);
    // Every context is re-armed at a random time inside the interval.
    expect(timers.armed).toHaveLength(ANON_TOKEN_CONTEXTS.length);
    for (const ms of timers.armed) {
      expect(ms).toBeGreaterThanOrEqual(REFILL_INTERVAL_MIN_MS);
      expect(ms).toBeLessThanOrEqual(REFILL_INTERVAL_MAX_MS);
    }
    // Spending the bucket empty draws nothing ...
    while (pool.size("ds-knock") > 0) await pool.take("ds-knock");
    await settle();
    expect(issuances(issuer, "ds-knock")).toHaveLength(1);
    // ... and an empty bucket fails rather than drawing at the moment of use.
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuances(issuer, "ds-knock")).toHaveLength(1);
    // The next scheduled refill tops it up to its target in one issuance.
    timers.fireScheduled();
    await settle();
    expect(issuances(issuer, "ds-knock")).toHaveLength(2);
    expect(pool.size("ds-knock")).toBe(POOL_TARGET["ds-knock"]);
  });

  it("a scheduled refill of a full bucket draws nothing", async () => {
    const { pool, issuer, timers } = fakeTokenPool();
    await pool.ready();
    await settle();
    const before = issuances(issuer).length;
    timers.fireScheduled();
    await settle();
    expect(issuances(issuer)).toHaveLength(before);
  });

  it("contexts are separate buckets", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("push-register");
    await settle();
    expect(pool.size("push-register")).toBe(POOL_TARGET["push-register"] - 1);
    expect(pool.size("invite-consume")).toBe(POOL_TARGET["invite-consume"]);
  });

  it("without a session there is nothing to draw with: fails without a request", async () => {
    const { pool, issuer } = fakeTokenPool();
    pool.unbind();
    await expect(pool.take("discovery-lookup")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuer).toHaveLength(0);
  });
});

describe("the issuance is verified against the key route's key", () => {
  it("fetches the key anonymously and finalizes under IT, not the response's public_key", async () => {
    const { pool, issuer, wasm } = fakeTokenPool({ responseKey: "a-key-from-the-response" });
    await pool.take("invite-consume");
    expect(wasm.finalizeKeys.length).toBeGreaterThan(0);
    expect(wasm.finalizeKeys.every((k) => k === FAKE_ISSUER_KEY)).toBe(true);
    const keyFetch = issuer.find((r) => r.url.includes("/auth/tokens/key/invite-consume/"));
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
    expect(wasm.finalizeKeys.every((k) => k === "the-published-key")).toBe(true);
    expect(pool.size("discovery-lookup")).toBe(0);
  });

  it("caches the key for its epoch", async () => {
    const { pool, issuer, timers } = fakeTokenPool();
    await pool.take("discovery-register");
    while (pool.size("discovery-register") > 0) await pool.take("discovery-register");
    timers.fireScheduled();
    await settle();
    expect(
      issuer.filter((r) => r.url.includes("/auth/tokens/key/discovery-register/")),
    ).toHaveLength(1);
    expect(issuances(issuer, "discovery-register")).toHaveLength(2);
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
          context: "discovery-lookup",
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
    expect(issuances(issuer, "discovery-lookup")).toHaveLength(1);
  });

  it("503 anon-tokens-disabled is the generic network failure", async () => {
    const { pool } = fakeTokenPool({
      issueScript: [
        { status: 503, json: { error: "anon-tokens-disabled" }, context: "push-register" },
      ],
    });
    const err = await pool.take("push-register").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AnonTokensDisabledError);
    expect(isAnonTokenNetworkFailure(err)).toBe(true);
  });

  it("400 epoch-not-issuable rebuilds the batch for the server's epoch, once", async () => {
    const { pool, issuer } = fakeTokenPool({
      issueScript: [
        {
          status: 400,
          json: { error: "epoch-not-issuable", current_epoch: 19676 },
          context: "ds-knock",
        },
      ],
    });
    const token = await pool.take("ds-knock");
    expect(token.epoch).toBe(19676n);
    const bodies = issuances(issuer, "ds-knock").map((r) => (r.body as { epoch: number }).epoch);
    expect(bodies).toEqual([19675, 19676]);
  });

  it("a second epoch disagreement gives up rather than looping", async () => {
    const { pool, issuer } = fakeTokenPool({
      issueScript: [
        {
          status: 400,
          json: { error: "epoch-not-issuable", current_epoch: 19676 },
          context: "ds-knock",
        },
        {
          status: 400,
          json: { error: "epoch-not-issuable", current_epoch: 19677 },
          context: "ds-knock",
        },
      ],
    });
    await expect(pool.take("ds-knock")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
    expect(issuances(issuer, "ds-knock")).toHaveLength(2);
  });

  it("a 401 from the issuer is not a sign-out: it is only 'no token'", async () => {
    const { pool } = fakeTokenPool({
      issueScript: [{ status: 401, json: {}, context: "ds-knock" }],
    });
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

describe("letting go of the account on logout and account switch", () => {
  it("signing out keeps the held tokens for the push unregister; unbindIfNoSession then drops them", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("push-register");
    pool.setSessionToken(null);
    expect(pool.size("push-register")).toBe(POOL_TARGET["push-register"] - 1);
    const held = await pool.take("push-register");
    expect(held.headerValue).toBe("tok-2");
    pool.unbindIfNoSession();
    expect(pool.size("push-register")).toBe(0);
    await expect(pool.take("push-register")).rejects.toBeInstanceOf(AnonTokenUnavailableError);
  });

  it("unbindIfNoSession leaves a session installed in the meantime alone", async () => {
    const { pool } = fakeTokenPool();
    pool.setSessionToken(null);
    pool.setSessionToken("v1.QUNDT1VOVA.0.tag");
    await pool.take("discovery-lookup");
    pool.unbindIfNoSession();
    expect(pool.size("discovery-lookup")).toBe(POOL_TARGET["discovery-lookup"] - 1);
  });

  it("another account's session drops the held tokens from memory", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("discovery-lookup");
    pool.setSessionToken("v1.QU5PVEhFUg.0.tag");
    expect(pool.size("discovery-lookup")).toBe(0);
  });

  it("the same account's renewed session keeps the pool", async () => {
    const { pool } = fakeTokenPool();
    await pool.take("discovery-lookup");
    pool.setSessionToken("v1.QUNDT1VOVA.999.other");
    expect(pool.size("discovery-lookup")).toBe(POOL_TARGET["discovery-lookup"] - 1);
  });

  it("a refill that lands after an unbind is thrown away", async () => {
    const { pool } = fakeTokenPool();
    const pending = pool.take("invite-consume").catch((e: unknown) => e);
    pool.unbind();
    const outcome = await pending;
    expect(outcome).toBeInstanceOf(AnonTokenUnavailableError);
    expect(pool.size("invite-consume")).toBe(0);
  });

  it("a take started under one account never hands a token over under another", async () => {
    const { pool } = fakeTokenPool();
    // The first take waits for a refill drawn for account A ...
    const pending = pool.take("invite-consume").catch((e: unknown) => e);
    // ... and account B signs in before it lands.
    pool.setSessionToken("v1.QU5PVEhFUg.0.tag");
    const outcome = await pending;
    expect(outcome).toBeInstanceOf(AnonTokenUnavailableError);
    // B starts from its own (empty) bucket, not from A's batch.
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
