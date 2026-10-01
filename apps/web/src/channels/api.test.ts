// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the channels API client wire contract: the ADR-0030
 * staged rotation (`stageRotation` / `commitRotation`), ADR-0031 envelope
 * retention, and the ADR-0059 section 9 publisher proof on every write a
 * publisher makes. The global `fetch` is replaced per-test with a capturing
 * mock, and the wasm signers with recording fakes, so the tests can assert
 * that the bytes signed are the bytes sent, the path and query signed are
 * the service path and the query sent, and that the role this account holds
 * for a channel is learned once and then used alone.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/** One `sign` call a fake signer recorded. */
interface SignCall {
  readonly role: "owner" | "copublisher";
  readonly method: string;
  readonly path: string;
  readonly query: string;
  readonly body: Uint8Array;
}

const fake = vi.hoisted(() => {
  const signed: SignCall[] = [];
  const keyOf = (identity: { tag: string }, channelId: Uint8Array, role: string): Uint8Array => {
    const text = `${role}:${identity.tag}:${Array.from(channelId).join(",")}`;
    const out = new Uint8Array(32);
    for (let i = 0; i < text.length; i += 1) out[i % 32] = (out[i % 32]! + text.charCodeAt(i)) & 0xff;
    return out;
  };
  const signer = (role: "owner" | "copublisher", identity: { tag: string }, channelId: Uint8Array) => ({
    verifyingKey: (): Uint8Array => keyOf(identity, channelId, role),
    sign: (method: string, path: string, query: string, body: Uint8Array): string => {
      signed.push({ role, method, path, query, body: body.slice() });
      return `${role}-proof-${signed.length}`;
    },
    free: (): void => {},
  });
  const wasm = {
    channelOwnerProofHeaderName: (): string => "x-lakko-channel-owner-proof",
    channelCoPublisherProofHeaderName: (): string => "x-lakko-channel-copublisher-proof",
    channelOwnerSigner: (identity: { tag: string }, channelId: Uint8Array) =>
      signer("owner", identity, channelId),
    channelCoPublisherSigner: (identity: { tag: string }, channelId: Uint8Array) =>
      signer("copublisher", identity, channelId),
  };
  return { signed, wasm };
});

vi.mock("../wasm", () => ({ loadWasm: async () => fake.wasm }));

import {
  apiError,
  cachedPublisherRole,
  channelErrorText,
  commitRotation,
  createChannel,
  deliverEnvelope,
  fetchEnvelopes,
  forgetPublisherRoles,
  listSubscribers,
  publishPost,
  rememberPublisherRole,
  rotateChannel,
  stageRotation,
} from "./api";

type Identity = Parameters<typeof publishPost>[2];

const CH = "00112233445566778899aabbccddeeff";
const DEFAULT_BASE = "/api/channels";
const OWNER_HEADER = "x-lakko-channel-owner-proof";
const COPUB_HEADER = "x-lakko-channel-copublisher-proof";
const ALICE = { tag: "alice" } as unknown as Identity;
const BOB = { tag: "bob" } as unknown as Identity;

interface CapturedCall {
  readonly url: string;
  readonly method: string;
  readonly headers: Readonly<Record<string, string>>;
  /** The exact bytes `fetch` was given, or `undefined` for no body. */
  readonly bytes: Uint8Array | undefined;
  /** `bytes` parsed as JSON when they are JSON, else `undefined`. */
  readonly body: unknown;
}

const realFetch = globalThis.fetch;

beforeEach(() => {
  fake.signed.length = 0;
  forgetPublisherRoles();
});

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.restoreAllMocks();
});

function bodyBytes(body: BodyInit | null | undefined): Uint8Array | undefined {
  if (body == null) return undefined;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (typeof body === "string") return new TextEncoder().encode(body);
  throw new Error(`unexpected body type ${Object.prototype.toString.call(body)}`);
}

/** Install a capturing fetch mock; `responder` decides the
 *  `Response` for each call. Returns the captured-call log. */
function installFetch(
  responder: (call: CapturedCall) => Response,
): CapturedCall[] {
  const calls: CapturedCall[] = [];
  const fetchMock = vi.fn((input: string | URL, init?: RequestInit) => {
    const bytes = bodyBytes(init?.body);
    let body: unknown;
    if (bytes !== undefined) {
      try {
        body = JSON.parse(new TextDecoder().decode(bytes));
      } catch {
        body = undefined;
      }
    }
    const call: CapturedCall = {
      url: String(input),
      method: init?.method ?? "GET",
      headers: { ...((init?.headers as Record<string, string> | undefined) ?? {}) },
      bytes,
      body,
    };
    calls.push(call);
    return Promise.resolve(responder(call));
  });
  (globalThis as unknown as { fetch: typeof fetch }).fetch =
    fetchMock as unknown as typeof fetch;
  return calls;
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const REFUSED = (): Response =>
  json({ code: "owner-proof-required", detail: "this route needs a valid channel owner proof" }, 401);

describe("stageRotation", () => {
  it("POSTs { target_epoch, envelopes } to /rotate/stage and maps the response", async () => {
    const calls = installFetch(
      () =>
        new Response(JSON.stringify({ target_epoch: 7, staged: 2 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    );

    const envelopes = [
      { subscriber_pubkey_hex: "aa", envelope_b64: "ZW52MQ==" },
      { subscriber_pubkey_hex: "bb", envelope_b64: "ZW52Mg==" },
    ];
    const res = await stageRotation(CH, 7, envelopes, ALICE);

    expect(res).toEqual({ targetEpoch: 7, staged: 2 });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(`${DEFAULT_BASE}/v1/channels/${CH}/rotate/stage`);
    expect(calls[0]!.method).toBe("POST");
    expect(calls[0]!.body).toEqual({ target_epoch: 7, envelopes });
  });

  it("throws a ChannelApiError on 413 chunk-too-large", async () => {
    installFetch(
      () =>
        new Response(
          JSON.stringify({ code: "chunk-too-large", detail: "too many" }),
          { status: 413, headers: { "content-type": "application/json" } },
        ),
    );

    const err = await stageRotation(CH, 7, [], ALICE).catch((e: unknown) => e);
    expect(apiError.is(err)).toBe(true);
    if (apiError.is(err)) {
      expect(err.status).toBe(413);
      expect(err.code).toBe("chunk-too-large");
    }
  });
});

describe("commitRotation", () => {
  it("POSTs { target_epoch } to /rotate/commit and returns the new epoch", async () => {
    const calls = installFetch(
      () =>
        new Response(JSON.stringify({ epoch: 7 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    );

    const res = await commitRotation(CH, 7, ALICE);

    expect(res).toEqual({ epoch: 7 });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/rotate/commit`,
    );
    expect(calls[0]!.method).toBe("POST");
    expect(calls[0]!.body).toEqual({ target_epoch: 7 });
  });

  it("throws a ChannelApiError on 409 stale-target-epoch", async () => {
    installFetch(
      () =>
        new Response(
          JSON.stringify({ code: "stale-target-epoch", detail: "not +1" }),
          { status: 409, headers: { "content-type": "application/json" } },
        ),
    );

    const err = await commitRotation(CH, 99, ALICE).catch((e: unknown) => e);
    expect(apiError.is(err)).toBe(true);
    if (apiError.is(err)) {
      expect(err.status).toBe(409);
      expect(err.code).toBe("stale-target-epoch");
    }
  });
});

// ADR-0031 multi-epoch retention wire contract.
describe("fetchEnvelopes", () => {
  const PK = "aabbccdd";

  it("GETs the plural endpoint and base64url-decodes each envelope", async () => {
    // "env1" => base64url-no-pad "ZW52MQ"; "env2" => "ZW52Mg".
    const calls = installFetch(
      () =>
        new Response(
          JSON.stringify({
            envelopes: [
              { epoch: 3, envelope_b64: "ZW52MQ" },
              { epoch: 4, envelope_b64: "ZW52Mg" },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
    );

    const res = await fetchEnvelopes(CH, PK);

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/subscribers/${PK}/envelopes`,
    );
    expect(calls[0]!.method).toBe("GET");
    expect(res).toHaveLength(2);
    expect(res[0]!.epoch).toBe(3);
    expect(res[1]!.epoch).toBe(4);
    const dec = new TextDecoder();
    expect(dec.decode(res[0]!.envelope)).toBe("env1");
    expect(dec.decode(res[1]!.envelope)).toBe("env2");
  });

  it("returns [] for an empty retained set", async () => {
    installFetch(
      () =>
        new Response(JSON.stringify({ envelopes: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    );
    const res = await fetchEnvelopes(CH, PK);
    expect(res).toEqual([]);
  });

  it("throws a ChannelApiError on 404 unknown-subscriber", async () => {
    installFetch(
      () =>
        new Response(
          JSON.stringify({ code: "unknown-subscriber", detail: "nope" }),
          { status: 404, headers: { "content-type": "application/json" } },
        ),
    );
    const err = await fetchEnvelopes(CH, PK).catch((e: unknown) => e);
    expect(apiError.is(err)).toBe(true);
    if (apiError.is(err)) {
      expect(err.status).toBe(404);
      expect(err.code).toBe("unknown-subscriber");
    }
  });
});

describe("deliverEnvelope", () => {
  const PK = "aabbccdd";

  it("appends ?epoch= when an epoch is supplied (ADR-0031) and signs that query", async () => {
    const calls = installFetch(() => new Response(null, { status: 200 }));
    await deliverEnvelope(CH, PK, new Uint8Array([1, 2, 3]), ALICE, undefined, 5);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.method).toBe("POST");
    expect(calls[0]!.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/subscribers/${PK}/envelope?epoch=5`,
    );
    expect(fake.signed).toHaveLength(1);
    expect(fake.signed[0]!.path).toBe(`/v1/channels/${CH}/subscribers/${PK}/envelope`);
    expect(fake.signed[0]!.query).toBe("epoch=5");
    expect(Array.from(fake.signed[0]!.body)).toEqual([1, 2, 3]);
    expect(Array.from(calls[0]!.bytes!)).toEqual([1, 2, 3]);
  });

  it("omits the query when no epoch is supplied, and signs an empty one", async () => {
    const calls = installFetch(() => new Response(null, { status: 200 }));
    await deliverEnvelope(CH, PK, new Uint8Array([1, 2, 3]), ALICE);
    expect(calls[0]!.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/subscribers/${PK}/envelope`,
    );
    expect(fake.signed[0]!.query).toBe("");
  });

  it("signs exactly the envelope bytes of a sub-view, not its whole buffer", async () => {
    const calls = installFetch(() => new Response(null, { status: 200 }));
    const backing = new Uint8Array([9, 9, 4, 5, 6, 9]);
    await deliverEnvelope(CH, PK, backing.subarray(2, 5), ALICE);
    expect(Array.from(fake.signed[0]!.body)).toEqual([4, 5, 6]);
    expect(Array.from(calls[0]!.bytes!)).toEqual([4, 5, 6]);
  });
});

// ADR-0059 section 9: every publisher write carries the proof.
describe("publisher proof on writes", () => {
  it("signs the service path and the exact JSON bytes it sends on rotate/stage", async () => {
    const calls = installFetch(() => json({ target_epoch: 7, staged: 1 }));
    const envelopes = [{ subscriber_pubkey_hex: "aa", envelope_b64: "ZW52MQ==" }];
    await stageRotation(CH, 7, envelopes, ALICE);

    expect(fake.signed).toHaveLength(1);
    const s = fake.signed[0]!;
    expect(s.role).toBe("owner");
    expect(s.method).toBe("POST");
    expect(s.path).toBe(`/v1/channels/${CH}/rotate/stage`);
    expect(s.path.startsWith("/api")).toBe(false);
    expect(s.query).toBe("");
    expect(Array.from(calls[0]!.bytes!)).toEqual(Array.from(s.body));
    expect(calls[0]!.headers[OWNER_HEADER]).toBe("owner-proof-1");
    expect(calls[0]!.headers["content-type"]).toBe("application/json");
  });

  it("signs rotate, rotate/commit and posts the same way", async () => {
    const calls = installFetch((call) =>
      call.url.endsWith("/posts")
        ? json({ cursor: 1, server_ts_ms: 2, payload_b64: "AQ" })
        : json({ epoch: 3 }),
    );
    await rotateChannel(CH, [], ALICE);
    await commitRotation(CH, 3, ALICE);
    await publishPost(CH, new Uint8Array([7, 8]), ALICE);

    expect(fake.signed.map((x) => x.path)).toEqual([
      `/v1/channels/${CH}/rotate`,
      `/v1/channels/${CH}/rotate/commit`,
      `/v1/channels/${CH}/posts`,
    ]);
    for (let i = 0; i < calls.length; i += 1) {
      expect(Array.from(calls[i]!.bytes!)).toEqual(Array.from(fake.signed[i]!.body));
    }
    expect(calls[0]!.body).toEqual({ envelopes: [] });
    expect(calls[1]!.body).toEqual({ target_epoch: 3 });
    expect(Array.from(calls[2]!.bytes!)).toEqual([7, 8]);
    expect(calls[2]!.headers["content-type"]).toBe("application/octet-stream");
  });
});

describe("publisher role", () => {
  /** The server: accepts the owner proof only when `owner` says so, the
   *  co-publisher proof only when `copub` does. */
  function server(owner: boolean, copub: boolean): CapturedCall[] {
    return installFetch((call) => {
      const ok =
        (call.headers[OWNER_HEADER] !== undefined && owner) ||
        (call.headers[COPUB_HEADER] !== undefined && copub);
      if (!ok) return REFUSED();
      if (call.url.endsWith("/subscribers")) return json({ subscribers: ["aa"] });
      if (call.url.endsWith("/posts")) return json({ cursor: 1, server_ts_ms: 2, payload_b64: "AQ" });
      return json({ target_epoch: 4, staged: 1 });
    });
  }

  it("a co-publisher retries once with its own key, then signs only with it", async () => {
    const calls = server(false, true);
    await publishPost(CH, new Uint8Array([1]), ALICE);
    expect(calls.map((c) => Object.keys(c.headers).find((h) => h.endsWith("-proof")))).toEqual([
      OWNER_HEADER,
      COPUB_HEADER,
    ]);
    expect(await cachedPublisherRole(CH, ALICE)).toBe("copublisher");

    calls.length = 0;
    await stageRotation(CH, 4, [{ subscriber_pubkey_hex: "aa", envelope_b64: "eA" }], ALICE);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.headers[COPUB_HEADER]).toBeDefined();
    expect(calls[0]!.headers[OWNER_HEADER]).toBeUndefined();
  });

  it("the role learned by listing subscribers keeps a stage chunk to one upload", async () => {
    const calls = server(false, true);
    await listSubscribers(CH, ALICE);
    expect(calls).toHaveLength(2);
    calls.length = 0;
    await stageRotation(CH, 4, [{ subscriber_pubkey_hex: "aa", envelope_b64: "eA" }], ALICE);
    expect(calls).toHaveLength(1);
  });

  it("an owner is learned from the first accepted request and never retried", async () => {
    const calls = server(true, false);
    await listSubscribers(CH, ALICE);
    await publishPost(CH, new Uint8Array([1]), ALICE);
    expect(calls).toHaveLength(2);
    expect(calls.every((c) => c.headers[OWNER_HEADER] !== undefined)).toBe(true);
    expect(await cachedPublisherRole(CH, ALICE)).toBe("owner");
  });

  it("creating a channel records the owner role", async () => {
    const calls = installFetch(() => json({ channel_id: CH }));
    await createChannel(CH, ALICE);
    expect(await cachedPublisherRole(CH, ALICE)).toBe("owner");
    calls.length = 0;
    await publishPost(CH, new Uint8Array([1]), ALICE).catch(() => undefined);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.headers[OWNER_HEADER]).toBeDefined();
  });

  it("a remembered role is signed alone; a 401 on it surfaces and forgets it", async () => {
    await rememberPublisherRole(CH, ALICE, "copublisher");
    const calls = server(false, false);
    const err = await publishPost(CH, new Uint8Array([1]), ALICE).catch((e: unknown) => e);
    expect(calls).toHaveLength(1);
    expect(apiError.is(err)).toBe(true);
    if (apiError.is(err)) {
      expect(err.status).toBe(401);
      expect(channelErrorText(err)).not.toMatch(/proof|owner-proof/i);
    }
    expect(await cachedPublisherRole(CH, ALICE)).toBeUndefined();
  });

  it("a refusal under both keys leaves the role unlearned", async () => {
    const calls = server(false, false);
    await expect(publishPost(CH, new Uint8Array([1]), ALICE)).rejects.toMatchObject({ status: 401 });
    expect(calls).toHaveLength(2);
    expect(await cachedPublisherRole(CH, ALICE)).toBeUndefined();
  });

  it("is kept per account: another account on the same channel learns its own", async () => {
    await rememberPublisherRole(CH, ALICE, "copublisher");
    expect(await cachedPublisherRole(CH, BOB)).toBeUndefined();
  });
});
