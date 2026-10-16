// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The anonymous call-admission mint (ADR-0078 section 2): one `call-join`
 * token pays for it, the request names the room (never the call id) and
 * carries no credentials, and the TURN credential minted beside the token
 * is read out of the answer.
 */
import { describe, expect, it, vi } from "vitest";

import {
  CallJwtArgError,
  CallJwtClient,
  CallJwtHttpError,
  parseTurnCredential,
  turnProviderForCall,
  type CallJwtWasm,
} from "./call-jwt-client";

const ROOM = "ab".repeat(32);

const wasm = {
  sfuRoomForCall: (callId: Uint8Array): string => {
    if (callId.length !== 16) throw new Error("callIdLength");
    return ROOM;
  },
  parseCallJwtUnverified: () => ({
    iss: "lakko-auth",
    aud: "lakko-sfu",
    room: ROOM,
    peerId: "00".repeat(16),
    role: "participant",
    iat: 1n,
    exp: 2n,
    kid: "k",
  }),
  verifyCallJwtWithJwks: () => {
    throw new Error("unused");
  },
} as unknown as CallJwtWasm;

interface Sent {
  url: string;
  init: RequestInit;
  context: string;
}

function harness(response: Response) {
  const sent: Sent[] = [];
  const tokens = {
    spend: async (
      context: string,
      send: (headerName: string, headerValue: string) => Promise<Response>,
    ): Promise<Response> => {
      const r = await send("x-lakko-anon-token", "token-value");
      sent[sent.length - 1]!.context = context;
      return r;
    },
  };
  const fetchImpl = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
    sent.push({ url: String(url), init: init ?? {}, context: "" });
    return response;
  });
  const client = new CallJwtClient(
    wasm,
    "/api/auth",
    0,
    tokens as never,
    fetchImpl as unknown as typeof fetch,
  );
  return { client, sent };
}

const TURN = {
  urls: ["turn:turn.lakko.test:3478?transport=udp"],
  username: "1790086863:01a0c4f54e277486971e8976d0fdc0fa",
  password: "kB1n8Yb3Zx0Q7t5aWm2rLpQ9sVc=",
  ttl_secs: 600,
  expiry_unix_secs: 1790086863,
};

function ok(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200 });
}

describe("CallJwtClient.mintForCall", () => {
  it("spends a call-join token, names the room, and sends no credentials", async () => {
    const { client, sent } = harness(
      ok({ jwt: "a.b.c", expires_at_unix_secs: 2, turn: TURN }),
    );
    const minted = await client.mintForCall(new Uint8Array(16).fill(1));
    expect(sent).toHaveLength(1);
    const req = sent[0]!;
    expect(req.context).toBe("call-join");
    expect(req.url).toBe("/api/auth/call/jwt");
    expect(JSON.parse(String(req.init.body))).toEqual({ room: ROOM, role: "participant" });
    const headers = req.init.headers as Record<string, string>;
    expect(headers["x-lakko-anon-token"]).toBe("token-value");
    expect(Object.keys(headers).map((h) => h.toLowerCase())).not.toContain("authorization");
    expect(req.init.credentials).toBe("omit");
    expect(minted.jwt).toBe("a.b.c");
    expect(minted.claims.room).toBe(ROOM);
    expect(minted.turn).toEqual({
      urls: TURN.urls,
      username: TURN.username,
      credential: TURN.password,
    });
  });

  it("a deployment without a relay answers turn: null", async () => {
    const { client } = harness(ok({ jwt: "a.b.c", expires_at_unix_secs: 2, turn: null }));
    expect((await client.mintForCall(new Uint8Array(16))).turn).toBeNull();
  });

  it("refuses a call id that is not 16 bytes before any request", async () => {
    const { client, sent } = harness(ok({}));
    await expect(client.mintForCall(new Uint8Array(15))).rejects.toBeInstanceOf(CallJwtArgError);
    expect(sent).toHaveLength(0);
  });

  it("surfaces the room cap as an HTTP 429", async () => {
    const { client } = harness(
      new Response(JSON.stringify({ error: "room-admission-limit" }), { status: 429 }),
    );
    const err = await client.mintForCall(new Uint8Array(16)).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(CallJwtHttpError);
    expect((err as CallJwtHttpError).status).toBe(429);
  });
});

describe("parseTurnCredential", () => {
  it.each([
    ["not an object", 42],
    ["null", null],
    ["no urls", { username: "u", password: "p" }],
    ["empty urls", { urls: [], username: "u", password: "p" }],
    ["urls of the wrong type", { urls: [7], username: "u", password: "p" }],
    ["no username", { urls: ["turn:x"], password: "p" }],
    ["no password", { urls: ["turn:x"], username: "u" }],
  ])("refuses %s rather than building a half-formed server", (_name, body) => {
    expect(parseTurnCredential(body)).toBeNull();
  });
});

describe("turnProviderForCall", () => {
  it("mints once per call and resolves null on a failure", async () => {
    const mint = vi.fn(async () => ({ turn: { urls: ["turn:x"], username: "u", credential: "p" } }));
    const provider = turnProviderForCall({ mintForCall: mint } as never, new Uint8Array(16));
    expect(await provider()).toEqual({ urls: ["turn:x"], username: "u", credential: "p" });
    expect(await provider()).not.toBeNull();
    expect(mint).toHaveBeenCalledTimes(1);

    const failing = turnProviderForCall(
      { mintForCall: async () => Promise.reject(new Error("offline")) } as never,
      new Uint8Array(16),
    );
    expect(await failing()).toBeNull();
  });
});
