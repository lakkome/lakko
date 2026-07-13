// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the ADR-0030 staged-rotation wire contract on
 * the channels API client (`stageRotation` / `commitRotation`).
 * The global `fetch` is replaced per-test with a capturing mock
 * so we can assert the URL, method, and JSON body shape against
 * the server-side contract (`services/channels::api`), plus the
 * `ChannelApiError` mapping on a non-2xx response.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  apiError,
  commitRotation,
  deliverEnvelope,
  fetchEnvelopes,
  stageRotation,
} from "./api";

const CH = "00112233445566778899aabbccddeeff";
const DEFAULT_BASE = "/api/channels";

interface CapturedCall {
  readonly url: string;
  readonly method: string;
  readonly body: unknown;
}

const realFetch = globalThis.fetch;

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.restoreAllMocks();
});

/** Install a capturing fetch mock; `responder` decides the
 *  `Response` for each call. Returns the captured-call log. */
function installFetch(
  responder: (call: CapturedCall) => Response,
): CapturedCall[] {
  const calls: CapturedCall[] = [];
  const fetchMock = vi.fn((input: string | URL, init?: RequestInit) => {
    const call: CapturedCall = {
      url: String(input),
      method: init?.method ?? "GET",
      body: init?.body != null ? JSON.parse(String(init.body)) : undefined,
    };
    calls.push(call);
    return Promise.resolve(responder(call));
  });
  (globalThis as unknown as { fetch: typeof fetch }).fetch =
    fetchMock as unknown as typeof fetch;
  return calls;
}

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
    const res = await stageRotation(CH, 7, envelopes);

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

    const err = await stageRotation(CH, 7, []).catch((e: unknown) => e);
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

    const res = await commitRotation(CH, 7);

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

    const err = await commitRotation(CH, 99).catch((e: unknown) => e);
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

  /** deliverEnvelope posts a Blob body (octet-stream), which the
   *  JSON-parsing `installFetch` harness cannot capture, so this
   *  block uses a lean URL/method-only mock. */
  function installUrlCapture(): { url: string; method: string } {
    const captured = { url: "", method: "" };
    const fetchMock = vi.fn((input: string | URL, init?: RequestInit) => {
      captured.url = String(input);
      captured.method = init?.method ?? "GET";
      return Promise.resolve(new Response(null, { status: 200 }));
    });
    (globalThis as unknown as { fetch: typeof fetch }).fetch =
      fetchMock as unknown as typeof fetch;
    return captured;
  }

  it("appends ?epoch= when an epoch is supplied (ADR-0031)", async () => {
    const captured = installUrlCapture();
    await deliverEnvelope(CH, PK, new Uint8Array([1, 2, 3]), undefined, 5);
    expect(captured.method).toBe("POST");
    expect(captured.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/subscribers/${PK}/envelope?epoch=5`,
    );
  });

  it("omits the query when no epoch is supplied", async () => {
    const captured = installUrlCapture();
    await deliverEnvelope(CH, PK, new Uint8Array([1, 2, 3]));
    expect(captured.url).toBe(
      `${DEFAULT_BASE}/v1/channels/${CH}/subscribers/${PK}/envelope`,
    );
  });
});
