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

import { apiError, commitRotation, stageRotation } from "./api";

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
