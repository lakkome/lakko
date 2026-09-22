// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it, vi } from "vitest";

import { TurnCredentialClient, parseTurnCredential } from "./turn-credential-client";

/** The body `services/auth/src/turn.rs` actually serialises. */
function wireBody(expiry: number): unknown {
  return {
    urls: ["turn:turn.lakko.me:3478?transport=udp", "turns:turn.lakko.me:5349?transport=tcp"],
    username: `${expiry}:01a0c4f54e277486971e8976d0fdc0fa`,
    password: "kB1n8Yb3Zx0Q7t5aWm2rLpQ9sVc=",
    ttl_secs: 600,
    expiry_unix_secs: expiry,
  };
}

function okResponse(body: unknown): Response {
  return { ok: true, status: 200, json: async () => body } as unknown as Response;
}

function errResponse(status: number): Response {
  return { ok: false, status, json: async () => ({}) } as unknown as Response;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("parseTurnCredential", () => {
  it("maps the auth service's body onto an RTCIceServer", () => {
    const parsed = parseTurnCredential(wireBody(1790086863));
    expect(parsed).not.toBeNull();
    expect(parsed?.server.urls).toEqual([
      "turn:turn.lakko.me:3478?transport=udp",
      "turns:turn.lakko.me:5349?transport=tcp",
    ]);
    expect(parsed?.server.username).toBe("1790086863:01a0c4f54e277486971e8976d0fdc0fa");
    expect(parsed?.server.credential).toBe("kB1n8Yb3Zx0Q7t5aWm2rLpQ9sVc=");
    expect(parsed?.expiryUnixSecs).toBe(1790086863);
  });

  it.each([
    ["not an object", 42],
    ["null", null],
    ["no urls", { username: "u", password: "p" }],
    ["empty urls", { urls: [], username: "u", password: "p" }],
    ["urls of the wrong type", { urls: [7], username: "u", password: "p" }],
    ["no username", { urls: ["turn:x"], password: "p" }],
    ["empty username", { urls: ["turn:x"], username: "", password: "p" }],
    ["no password", { urls: ["turn:x"], username: "u" }],
  ])("refuses %s rather than building a half-formed server", (_name, body) => {
    expect(parseTurnCredential(body)).toBeNull();
  });
});

describe("TurnCredentialClient", () => {
  it("returns null without a session token, and does not call the network", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const client = new TurnCredentialClient();
    expect(await client.fetchOrNull()).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("mints once and reuses the credential until it nears expiry", async () => {
    let nowMs = 1_000_000_000_000;
    const expiry = Math.floor(nowMs / 1000) + 600;
    const fetchMock = vi.fn(async () => okResponse(wireBody(expiry)));
    vi.stubGlobal("fetch", fetchMock);

    const client = new TurnCredentialClient("/api/auth", () => nowMs);
    client.setSessionToken("token-a");

    const first = await client.fetchOrNull();
    expect(first?.username).toBe(`${expiry}:01a0c4f54e277486971e8976d0fdc0fa`);
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Well inside the credential's life: served from cache.
    nowMs += 300_000;
    await client.fetchOrNull();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Inside the refresh margin: minted again, because the relay refuses an
    // allocation made under an expired credential.
    nowMs += 260_000;
    await client.fetchOrNull();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("re-mints when the session token changes", async () => {
    const nowMs = 1_000_000_000_000;
    const expiry = Math.floor(nowMs / 1000) + 600;
    const fetchMock = vi.fn(async () => okResponse(wireBody(expiry)));
    vi.stubGlobal("fetch", fetchMock);

    const client = new TurnCredentialClient("/api/auth", () => nowMs);
    client.setSessionToken("token-a");
    await client.fetchOrNull();
    client.setSessionToken("token-b");
    await client.fetchOrNull();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("shares one request between callers in the same tick", async () => {
    const nowMs = 1_000_000_000_000;
    const expiry = Math.floor(nowMs / 1000) + 600;
    const fetchMock = vi.fn(async () => okResponse(wireBody(expiry)));
    vi.stubGlobal("fetch", fetchMock);

    const client = new TurnCredentialClient("/api/auth", () => nowMs);
    client.setSessionToken("token-a");
    const [a, b] = await Promise.all([client.fetchOrNull(), client.fetchOrNull()]);
    expect(a).toEqual(b);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("sends the bearer token to the credential route", async () => {
    const nowMs = 1_000_000_000_000;
    const fetchMock = vi.fn(async () => okResponse(wireBody(Math.floor(nowMs / 1000) + 600)));
    vi.stubGlobal("fetch", fetchMock);

    const client = new TurnCredentialClient("/api/auth", () => nowMs);
    client.setSessionToken("token-a");
    await client.fetchOrNull();
    expect(fetchMock).toHaveBeenCalledWith("/api/auth/call/turn-credential", {
      method: "POST",
      headers: { authorization: "Bearer token-a" },
    });
  });

  it.each([503, 500, 401])(
    "falls back to STUN on HTTP %i rather than failing the call",
    async (status) => {
      vi.stubGlobal(
        "fetch",
        vi.fn(async () => errResponse(status)),
      );
      const client = new TurnCredentialClient();
      client.setSessionToken("token-a");
      expect(await client.fetchOrNull()).toBeNull();
    },
  );

  it("falls back to STUN when the network throws", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("offline");
      }),
    );
    const client = new TurnCredentialClient();
    client.setSessionToken("token-a");
    expect(await client.fetchOrNull()).toBeNull();
  });

  it("falls back to STUN on a 200 whose body is not a credential", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => okResponse({ urls: "not-an-array" })),
    );
    const client = new TurnCredentialClient();
    client.setSessionToken("token-a");
    expect(await client.fetchOrNull()).toBeNull();
  });
});
