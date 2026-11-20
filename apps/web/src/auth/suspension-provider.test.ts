// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 step 3: the session token provider in suspended mode, driven with
 * a fake refresh transport and a fake `fetch`. Entering it from a bearer
 * route's `403 account-suspended` (through the refresh), the local refusal of
 * everything a restricted session does not open, the restricted routes going
 * out with the live restricted token, and leaving it on a normal refresh.
 */
import { describe, expect, it } from "vitest";

import { REFRESH_AHEAD_MS, type SessionPair } from "./session-pair";
import {
  routeOf,
  SessionTokenProvider,
  type RefreshTransportResult,
  type SessionEvent,
} from "./session-token-provider";
import { bearerRefusal } from "./session-401";
import type { RestrictedSession } from "./suspension";

const NOW = 1_800_000_000_000;
const ACCT = "QUJDRA==";

function pair(n: number): SessionPair {
  return {
    sessionToken: `v3.acct.1.${n}.tag`,
    sessionExpiresAtMs: NOW + 900_000,
    refreshToken: `r2.acct.1.${n}.tag`,
    refreshExpiresAtMs: NOW + 1_000_000_000,
    keyGeneration: 1n,
  };
}

function restricted(n: number, expiresAtMs = NOW + 900_000): RestrictedSession {
  return {
    restrictedToken: `s1.acct.1.${n}.tag`,
    restrictedExpiresAtMs: expiresAtMs,
    refreshToken: `r2.acct.1.s${n}.tag`,
    refreshExpiresAtMs: NOW + 1_000_000_000,
    keyGeneration: 1n,
  };
}

interface Harness {
  readonly provider: SessionTokenProvider;
  readonly events: SessionEvent[];
  readonly refreshes: string[];
  readonly persistedRestricted: RestrictedSession[];
  readonly sent: { url: string; method: string; authorization: string | null }[];
  now: number;
  refreshAnswer: (refreshToken: string) => RefreshTransportResult;
  responses: Response[];
}

function harness(): Harness {
  const h: Harness = {
    provider: new SessionTokenProvider(),
    events: [],
    refreshes: [],
    persistedRestricted: [],
    sent: [],
    now: NOW,
    refreshAnswer: () => ({ kind: "suspended", restricted: restricted(1) }),
    responses: [],
  };
  h.provider.configure({
    refresh: async (rt) => {
      h.refreshes.push(rt);
      return h.refreshAnswer(rt);
    },
    persist: async () => undefined,
    persistRestricted: async (_acct, r) => {
      h.persistedRestricted.push(r);
    },
    now: () => h.now,
    fetchImpl: async (input, init) => {
      const headers = new Headers(init?.headers);
      h.sent.push({
        url: String(input),
        method: init?.method ?? "GET",
        authorization: headers.get("authorization"),
      });
      return h.responses.shift() ?? new Response("{}", { status: 200 });
    },
  });
  h.provider.subscribe((e) => h.events.push(e));
  return h;
}

const suspended403 = (): Response =>
  new Response(JSON.stringify({ error: "account-suspended" }), { status: 403 });

describe("bearerRefusal", () => {
  it("reads a 403 account-suspended as suspended, any other 403 as pass", () => {
    expect(
      bearerRefusal({ status: 403, errorCode: "account-suspended", retried: false, refreshAvailable: true }),
    ).toBe("suspended");
    expect(
      bearerRefusal({ status: 403, errorCode: "fresh-auth-required", retried: false, refreshAvailable: true }),
    ).toBe("pass");
  });
});

describe("entering suspended mode from a bearer route", () => {
  it("refreshes once on 403 account-suspended and holds the restricted session", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(suspended403());
    const res = await h.provider.fetch("/api/auth/auth/tokens", {
      method: "POST",
      headers: { authorization: `Bearer ${pair(1).sessionToken}` },
    });
    expect(res.status).toBe(403);
    expect(h.refreshes).toEqual([pair(1).refreshToken]);
    expect(h.provider.pair()).toBeNull();
    expect(h.provider.restricted()).toEqual(restricted(1));
    expect(h.provider.accountIdB64()).toBe(ACCT);
    expect(h.persistedRestricted).toEqual([restricted(1)]);
    expect(h.events).toEqual([{ kind: "suspended", accountIdB64: ACCT, restricted: restricted(1) }]);
  });

  it("raises suspended on every refresh that answers it, and keeps the newest", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    h.refreshAnswer = () => ({ kind: "suspended", restricted: restricted(2) });
    expect(await h.provider.refreshOnce()).toBe("suspended");
    expect(h.refreshes).toEqual([restricted(1).refreshToken]);
    expect(h.provider.restricted()).toEqual(restricted(2));
    expect(h.events.map((e) => e.kind)).toEqual(["suspended"]);
  });
});

describe("while suspended", () => {
  it("answers a request with the old access token locally and sends nothing", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    await h.provider.refreshOnce();
    const res = await h.provider.fetch("/api/discovery/discovery/keypackage", {
      method: "POST",
      headers: { authorization: `Bearer ${pair(1).sessionToken}` },
    });
    expect(res.status).toBe(403);
    expect(await res.json()).toEqual({ error: "account-suspended" });
    expect(h.sent).toEqual([]);
  });

  it("answers a non-restricted auth route locally, even with the restricted token", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    const res = await h.provider.fetch("/api/auth/auth/session/bind-device", {
      method: "POST",
      headers: { authorization: `Bearer ${restricted(1).restrictedToken}` },
    });
    expect(res.status).toBe(403);
    expect(h.sent).toEqual([]);
  });

  it("sends the restricted routes with the live restricted token", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    await h.provider.refreshOnce();
    for (const [url, method] of [
      ["/api/auth/account/suspension", "GET"],
      ["/api/auth/account/suspension/appeal", "POST"],
      ["/api/auth/account", "DELETE"],
      ["/api/auth/auth/session/logout", "POST"],
      ["/api/auth/auth/action/start", "POST"],
      ["/api/auth/auth/action/finish", "POST"],
      ["/api/auth/auth/tokens", "POST"],
      ["/api/discovery/discovery/account", "DELETE"],
      ["/api/storage/backup", "DELETE"],
    ] as const) {
      const res = await h.provider.fetch(url, {
        method,
        headers: { authorization: `Bearer ${pair(1).sessionToken}` },
      });
      expect(res.status).toBe(200);
    }
    expect(h.sent.map((s) => s.authorization)).toEqual(
      Array(9).fill(`Bearer ${restricted(1).restrictedToken}`),
    );
  });

  it("refreshes a restricted token that is about to expire before sending", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1, NOW + REFRESH_AHEAD_MS - 1));
    h.refreshAnswer = () => ({ kind: "suspended", restricted: restricted(2) });
    await h.provider.fetch("/api/auth/account/suspension", {
      method: "GET",
      headers: { authorization: `Bearer ${restricted(1).restrictedToken}` },
    });
    expect(h.refreshes).toHaveLength(1);
    expect(h.sent[0]?.authorization).toBe(`Bearer ${restricted(2).restrictedToken}`);
  });

  it("lets through only those methods on those paths", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    for (const [url, method] of [
      ["/api/discovery/discovery/keypackage", "DELETE"],
      ["/api/discovery/discovery/account", "GET"],
      ["/api/storage/backup", "PUT"],
      ["/api/storage/backup", "GET"],
      ["/api/auth/auth/tokens/key/blob-read/1", "GET"],
      ["/api/channels/v1/channels/x/posts", "POST"],
    ] as const) {
      const res = await h.provider.fetch(url, {
        method,
        headers: { authorization: `Bearer ${restricted(1).restrictedToken}` },
      });
      expect(res.status).toBe(403);
    }
    expect(h.sent).toEqual([]);
  });
});

describe("leaving suspended mode", () => {
  it("installs the pair a refresh answers and raises pair", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    h.refreshAnswer = () => ({ kind: "pair", pair: pair(3) });
    expect(await h.provider.refreshOnce()).toBe("refreshed");
    expect(h.provider.restricted()).toBeNull();
    expect(h.provider.pair()).toEqual(pair(3));
    expect(h.events).toEqual([{ kind: "pair", accountIdB64: ACCT, pair: pair(3) }]);
  });

  it("then resolves a captured restricted token to the live access token", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    h.refreshAnswer = () => ({ kind: "pair", pair: pair(3) });
    await h.provider.refreshOnce();
    await h.provider.fetch("/api/discovery/discovery/keypackage", {
      method: "POST",
      headers: { authorization: `Bearer ${restricted(1).restrictedToken}` },
    });
    expect(h.sent[0]?.authorization).toBe(`Bearer ${pair(3).sessionToken}`);
  });

  it("is signed out, not suspended, when the refresh says refresh-invalid", async () => {
    const h = harness();
    h.provider.installRestricted(ACCT, restricted(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 401, errorCode: "refresh-invalid" });
    expect(await h.provider.refreshOnce()).toBe("invalid");
    expect(h.provider.restricted()).toBeNull();
    expect(h.events.map((e) => e.kind)).toEqual(["refresh-invalid"]);
  });
});

describe("routeOf", () => {
  it("names the method and the path, without the query", () => {
    expect(routeOf("/api/auth/account?x=1", "DELETE")).toBe("DELETE /api/auth/account");
    expect(routeOf("https://x.example/api/auth/account/suspension", undefined)).toBe(
      "GET /api/auth/account/suspension",
    );
  });
});
