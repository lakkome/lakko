// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The member-card routes of `services/channels` (ADR-0060 section 3): the
 * method, path, signature header and body shape each client call sends, and
 * the decoding of a members page.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  apiError,
  deleteMemberCard,
  listMemberCards,
  MEMBER_CARD_SIG_HEADER,
  putMemberCard,
} from "./api";

const CH = "00112233445566778899aabbccddeeff";
const PK = "ab".repeat(32);
const realFetch = globalThis.fetch;

afterEach(() => {
  (globalThis as unknown as { fetch: typeof fetch }).fetch = realFetch;
  vi.restoreAllMocks();
});

interface Captured {
  readonly url: string;
  readonly init: RequestInit | undefined;
}

function installFetch(response: () => Response): Captured[] {
  const calls: Captured[] = [];
  (globalThis as unknown as { fetch: typeof fetch }).fetch = vi.fn(
    (input: string | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return Promise.resolve(response());
    },
  ) as unknown as typeof fetch;
  return calls;
}

function header(init: RequestInit | undefined, name: string): string | undefined {
  const h = init?.headers as Record<string, string> | undefined;
  return h?.[name];
}

describe("putMemberCard", () => {
  it("PUTs the raw card under the subscriber key with the signature header", async () => {
    const calls = installFetch(() => new Response(null, { status: 204 }));
    const card = new Uint8Array([1, 2, 3]);
    await putMemberCard(CH, PK, card, "c2ln");
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/members/${PK}`);
    expect(calls[0]!.init?.method).toBe("PUT");
    expect(header(calls[0]!.init, MEMBER_CARD_SIG_HEADER)).toBe("c2ln");
    const body = calls[0]!.init?.body as Blob;
    expect(new Uint8Array(await body.arrayBuffer())).toEqual(card);
  });

  it("surfaces unknown-subscriber as a ChannelApiError", async () => {
    installFetch(
      () =>
        new Response(JSON.stringify({ code: "unknown-subscriber", detail: "not registered" }), {
          status: 404,
          headers: { "content-type": "application/json" },
        }),
    );
    const err = await putMemberCard(CH, PK, new Uint8Array([1]), "c2ln").catch((e: unknown) => e);
    expect(apiError.is(err) && err.code).toBe("unknown-subscriber");
  });
});

describe("deleteMemberCard", () => {
  it("sends a signed DELETE with no body", async () => {
    const calls = installFetch(() => new Response(null, { status: 204 }));
    await deleteMemberCard(CH, PK, "c2ln");
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/members/${PK}`);
    expect(calls[0]!.init?.method).toBe("DELETE");
    expect(header(calls[0]!.init, MEMBER_CARD_SIG_HEADER)).toBe("c2ln");
    expect(calls[0]!.init?.body).toBeUndefined();
  });
});

describe("listMemberCards", () => {
  it("pages with after/limit and decodes base64url keys and cards", async () => {
    const calls = installFetch(
      () =>
        new Response(
          JSON.stringify({
            members: [{ subscriber_pk_b64: "_-8", card_b64: "AQI" }],
            next_after: "_-8",
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
    );
    const page = await listMemberCards(CH, "prev", 50);
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/members?after=prev&limit=50`);
    expect(page.members).toHaveLength(1);
    expect(page.members[0]!.subscriberPk).toEqual(new Uint8Array([0xff, 0xef]));
    expect(page.members[0]!.card).toEqual(new Uint8Array([1, 2]));
    expect(page.nextAfter).toBe("_-8");
  });

  it("asks from the start without a query and reads a null next_after", async () => {
    const calls = installFetch(
      () =>
        new Response(JSON.stringify({ members: [], next_after: null }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    );
    const page = await listMemberCards(CH, null);
    expect(calls[0]!.url).toBe(`/api/channels/v1/channels/${CH}/members`);
    expect(page).toEqual({ members: [], nextAfter: null });
  });
});
