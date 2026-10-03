// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The session token provider (ADR-0065 section 3.3) driven with a fake
 * refresh transport and a fake `fetch`: refresh ahead of expiry, one refresh
 * in flight, one retry after a 401, the superseded signal, a stale capture
 * substituted, an unmanaged bearer untouched.
 */
import { describe, expect, it, vi } from "vitest";

import { REFRESH_AHEAD_MS, type SessionPair } from "./session-pair";
import {
  SessionTokenProvider,
  type RefreshTransportResult,
  type SessionEvent,
} from "./session-token-provider";

const NOW = 1_800_000_000_000;
const ACCT = "QUJDRA==";

function pair(n: number, overrides: Partial<SessionPair> = {}): SessionPair {
  return {
    sessionToken: `v2.acct.1.${n}.tag`,
    sessionExpiresAtMs: NOW + n * 900_000,
    refreshToken: `r1.acct.1.${n}.tag`,
    refreshExpiresAtMs: NOW + 1_000_000_000,
    keyGeneration: 1n,
    ...overrides,
  };
}

interface Harness {
  readonly provider: SessionTokenProvider;
  readonly events: SessionEvent[];
  readonly refreshes: string[];
  readonly persisted: SessionPair[];
  readonly sent: { url: string; authorization: string | null }[];
  now: number;
  refreshAnswer: (refreshToken: string) => RefreshTransportResult | Promise<RefreshTransportResult>;
  responses: Response[];
}

function harness(): Harness {
  const h: Harness = {
    provider: new SessionTokenProvider(),
    events: [],
    refreshes: [],
    persisted: [],
    sent: [],
    now: NOW,
    refreshAnswer: () => ({ kind: "pair", pair: pair(2) }),
    responses: [],
  };
  h.provider.configure({
    refresh: async (rt) => {
      h.refreshes.push(rt);
      return h.refreshAnswer(rt);
    },
    persist: async (_acct, p) => {
      h.persisted.push(p);
    },
    now: () => h.now,
    fetchImpl: async (input, init) => {
      const headers = new Headers(init?.headers);
      h.sent.push({ url: String(input), authorization: headers.get("authorization") });
      const next = h.responses.shift();
      return next ?? new Response("{}", { status: 200 });
    },
  });
  h.provider.subscribe((e) => h.events.push(e));
  return h;
}

const ok = (): Response => new Response("{}", { status: 200 });
const unauthorized = (body = ""): Response => new Response(body, { status: 401 });

describe("current()", () => {
  it("hands out the held token while more than the window remains, without a refresh", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    expect(await h.provider.current()).toBe(pair(1).sessionToken);
    expect(h.refreshes).toEqual([]);
  });

  it("refreshes first inside the window and hands out the new token", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.now = NOW + 900_000 - REFRESH_AHEAD_MS;
    expect(await h.provider.current()).toBe(pair(2).sessionToken);
    expect(h.refreshes).toEqual([pair(1).refreshToken]);
    expect(h.persisted).toEqual([pair(2)]);
    expect(h.events).toEqual([{ kind: "pair", accountIdB64: ACCT, pair: pair(2) }]);
    // The old token is still recognised and resolves to the live one.
    expect(h.provider.manages(pair(1).sessionToken)).toBe(true);
    expect(h.provider.peek()).toBe(pair(2).sessionToken);
  });

  it("is null without a pair, and once the refresh credential expired", async () => {
    const h = harness();
    expect(await h.provider.current()).toBeNull();
    h.provider.install(ACCT, pair(1, { refreshExpiresAtMs: NOW }));
    expect(await h.provider.current()).toBeNull();
    expect(h.refreshes).toEqual([]);
  });

  it("keeps a still-valid token on a network failure, and nothing on an expired one", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => {
      throw new Error("offline");
    };
    h.now = NOW + 900_000 - 1;
    expect(await h.provider.current()).toBe(pair(1).sessionToken);
    h.now = NOW + 900_000;
    expect(await h.provider.current()).toBeNull();
    expect(h.provider.pair()).toEqual(pair(1));
    expect(h.events).toEqual([]);
  });
});

describe("refreshOnce()", () => {
  it("shares one refresh among concurrent callers", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    let release: (r: RefreshTransportResult) => void = () => undefined;
    h.refreshAnswer = () =>
      new Promise<RefreshTransportResult>((resolve) => {
        release = resolve;
      });
    h.now = NOW + 900_000;
    const a = h.provider.refreshOnce();
    const b = h.provider.refreshOnce();
    const c = h.provider.current();
    release({ kind: "pair", pair: pair(2) });
    expect(await a).toBe("refreshed");
    expect(await b).toBe("refreshed");
    expect(await c).toBe(pair(2).sessionToken);
    expect(h.refreshes).toHaveLength(1);
  });

  it("drops the pair and raises superseded on the superseded code", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 401, errorCode: "key-generation-superseded" });
    expect(await h.provider.refreshOnce()).toBe("superseded");
    expect(h.provider.pair()).toBeNull();
    expect(h.events).toEqual([{ kind: "superseded", accountIdB64: ACCT }]);
  });

  it("drops the pair and raises refresh-invalid on any other 401", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 401, errorCode: "refresh-invalid" });
    expect(await h.provider.refreshOnce()).toBe("invalid");
    expect(h.provider.pair()).toBeNull();
    expect(h.events).toEqual([{ kind: "refresh-invalid", accountIdB64: ACCT }]);
  });

  it("keeps the pair on a non-401 refusal (a limit, an outage)", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 429, errorCode: "too-many-attempts" });
    expect(await h.provider.refreshOnce()).toBe("network");
    expect(h.provider.pair()).toEqual(pair(1));
    expect(h.events).toEqual([]);
  });

  it("ignores a late answer after the session was cleared", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    let release: (r: RefreshTransportResult) => void = () => undefined;
    h.refreshAnswer = () =>
      new Promise<RefreshTransportResult>((resolve) => {
        release = resolve;
      });
    const outcome = h.provider.refreshOnce();
    h.provider.clear();
    release({ kind: "pair", pair: pair(2) });
    expect(await outcome).toBe("none");
    expect(h.provider.pair()).toBeNull();
    expect(h.events).toEqual([]);
  });
});

describe("fetch()", () => {
  it("passes a request with no bearer, or a bearer it does not manage, through untouched", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(unauthorized(), unauthorized());
    await h.provider.fetch("/x", { method: "GET" });
    await h.provider.fetch("/y", { headers: { authorization: "Bearer someone-else" } });
    expect(h.sent.map((s) => s.authorization)).toEqual([null, "Bearer someone-else"]);
    expect(h.refreshes).toEqual([]);
  });

  it("refreshes ahead of expiry and sends the live token; a stale capture is substituted", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.now = NOW + 900_000 - 10;
    h.responses.push(ok());
    await h.provider.fetch("/a", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(h.sent[0]?.authorization).toBe(`Bearer ${pair(2).sessionToken}`);
    // A later request still carrying the token of the first pair.
    h.responses.push(ok());
    await h.provider.fetch("/b", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(h.sent[1]?.authorization).toBe(`Bearer ${pair(2).sessionToken}`);
    expect(h.refreshes).toHaveLength(1);
  });

  it("on a plain 401 refreshes once and sends the request once more with the new token", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(unauthorized(), ok());
    const response = await h.provider.fetch("/a", {
      method: "POST",
      headers: { authorization: `Bearer ${pair(1).sessionToken}`, "content-type": "application/json" },
      body: "{}",
    });
    expect(response.status).toBe(200);
    expect(h.sent.map((s) => s.authorization)).toEqual([
      `Bearer ${pair(1).sessionToken}`,
      `Bearer ${pair(2).sessionToken}`,
    ]);
    expect(h.refreshes).toEqual([pair(1).refreshToken]);
  });

  it("reports a second 401 as it is, with no further refresh", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(unauthorized(), unauthorized());
    const response = await h.provider.fetch("/a", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(response.status).toBe(401);
    expect(h.refreshes).toHaveLength(1);
    expect(h.events.map((e) => e.kind)).toEqual(["pair"]);
  });

  it("signs out on 401 key-generation-superseded from a bearer route, without a retry", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(unauthorized('{"error":"key-generation-superseded"}'));
    const response = await h.provider.fetch("/a", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(response.status).toBe(401);
    expect(h.sent).toHaveLength(1);
    expect(h.refreshes).toEqual([]);
    expect(h.events).toEqual([{ kind: "superseded", accountIdB64: ACCT }]);
    expect(h.provider.pair()).toBeNull();
    // The body is still readable by the caller.
    expect(await response.text()).toContain("key-generation-superseded");
  });

  it("signs out when the retry after a refresh answers superseded", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(unauthorized(), unauthorized('{"error":"key-generation-superseded"}'));
    await h.provider.fetch("/a", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(h.events.map((e) => e.kind)).toEqual(["pair", "superseded"]);
  });

  it("returns the first 401 when the refresh is refused, and the listener saw why", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 401, errorCode: "refresh-invalid" });
    h.responses.push(unauthorized());
    const response = await h.provider.fetch("/a", { headers: { authorization: `Bearer ${pair(1).sessionToken}` } });
    expect(response.status).toBe(401);
    expect(h.sent).toHaveLength(1);
    expect(h.events).toEqual([{ kind: "refresh-invalid", accountIdB64: ACCT }]);
  });

  it("raises superseded once per account even when several requests see it", async () => {
    const h = harness();
    h.provider.install(ACCT, pair(1));
    h.responses.push(
      unauthorized('{"error":"key-generation-superseded"}'),
      unauthorized('{"error":"key-generation-superseded"}'),
    );
    const headers = { authorization: `Bearer ${pair(1).sessionToken}` };
    await Promise.all([h.provider.fetch("/a", { headers }), h.provider.fetch("/b", { headers })]);
    expect(h.events.filter((e) => e.kind === "superseded")).toHaveLength(1);
  });

  it("a listener that throws does not break the others", async () => {
    const h = harness();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    h.provider.subscribe(() => {
      throw new Error("listener");
    });
    h.provider.install(ACCT, pair(1));
    h.refreshAnswer = () => ({ kind: "refused", status: 401, errorCode: "key-generation-superseded" });
    await h.provider.refreshOnce();
    expect(h.events).toEqual([{ kind: "superseded", accountIdB64: ACCT }]);
    warn.mockRestore();
  });
});
