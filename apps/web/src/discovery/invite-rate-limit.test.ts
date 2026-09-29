// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The open invite routes of the discovery ledger answer 429 since the
 * short-link amendment of ADR-0028 (2026-09-27): a 6-byte id is guessable in
 * principle, so fetch and consume sit behind a per-address token bucket. A
 * 429 says nothing about the link; every surface must say "try again later"
 * and never "this link is invalid". Also pins the 8-character id the upload
 * returns and the refusal of the old 43-character one.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import {
  DiscoveryClient,
  DiscoveryClientArgError,
  DiscoveryHttpError,
  DiscoveryRateLimitedError,
  isDiscoveryRateLimited,
} from "./client";
import { ContactsClient, ContactsError } from "../contacts/client";
import { joinFailureNotice } from "../routes/join-group-failure";
import { INVITE_LINK_RATE_LIMITED } from "../contacts/invite-link";
import { acceptCallInviteFromSlug } from "../calls/acceptor";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { fakeTokenPool } from "../tokens/anon-token-test-doubles";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const ID = new Uint8Array(6).fill(0x2a);
const ACCOUNT = new Uint8Array(16).fill(0x01);
const CAP = new Uint8Array(160).fill(0x41);
const UPLOAD = {
  sealed: CAP,
  issuerPubkey: new Uint8Array(32).fill(0x02),
  expiryMs: 0n,
  maxUses: 5,
};

/** Stub `fetch`: every request answers the given status (with an optional
 *  JSON body). No route asks for a proof of work any more (ADR-0054 S3). */
function stubFetch(status: number, body: unknown = {}, headers: Record<string, string> = {}) {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      calls.push(url);
      if (url.endsWith("/pow/challenge")) throw new Error("no route asks for a proof of work");
      return new Response(status === 204 ? null : JSON.stringify(body), { status, headers });
    }),
  );
  return calls;
}

describe("fetch answers 429: rate limited, never 'not found'", () => {
  const client = new DiscoveryClient("https://d.test");

  it.each([
    ["group", (c: DiscoveryClient) => c.fetchGroupInvite(ID)],
    ["contact", (c: DiscoveryClient) => c.fetchInvite(ID)],
    ["call", (c: DiscoveryClient) => c.fetchCallInvite(ID)],
  ])("%s-invite fetch throws DiscoveryRateLimitedError with Retry-After", async (_, run) => {
    stubFetch(429, { error: "rate limited" }, { "retry-after": "12" });
    const err = await run(client).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DiscoveryRateLimitedError);
    expect((err as DiscoveryRateLimitedError).retryAfterSecs).toBe(12);
    expect(isDiscoveryRateLimited(err)).toBe(true);
  });

  it("404 is still null (the not-found fold is unchanged)", async () => {
    stubFetch(404);
    await expect(client.fetchGroupInvite(ID)).resolves.toBeNull();
  });

  it("fetches with the 8-character id in the path", async () => {
    const calls = stubFetch(404);
    await client.fetchGroupInvite(ID);
    expect(calls[0]).toBe(`https://d.test/discovery/group-invite/${base64UrlEncodeNoPad(ID)}`);
    expect(base64UrlEncodeNoPad(ID)).toHaveLength(8);
  });

  it("refuses the old 32-byte id before any request", async () => {
    const calls = stubFetch(200);
    await expect(client.fetchGroupInvite(new Uint8Array(32))).rejects.toBeInstanceOf(
      DiscoveryClientArgError,
    );
    expect(calls).toHaveLength(0);
  });
});

describe("consume answers 429: rate limited, never 'not found'", () => {
  const client = new DiscoveryClient("https://d.test", { tokens: fakeTokenPool().pool });

  it.each([
    ["group", (c: DiscoveryClient) => c.consumeGroupInvite(ID, CAP, ACCOUNT)],
    ["contact", (c: DiscoveryClient) => c.consumeInvite(ID, CAP, ACCOUNT)],
    ["call", (c: DiscoveryClient) => c.consumeCallInvite(ID, CAP, ACCOUNT)],
  ])("%s-invite consume throws DiscoveryRateLimitedError", async (_, run) => {
    stubFetch(429);
    const err = await run(client).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DiscoveryRateLimitedError);
    expect((err as DiscoveryRateLimitedError).retryAfterSecs).toBeNull();
  });
});

describe("upload returns an 8-character id", () => {
  const client = new DiscoveryClient("https://d.test");

  it("decodes the fresh 8-character id to 6 bytes", async () => {
    stubFetch(200, { blob_id: base64UrlEncodeNoPad(ID) });
    await expect(client.uploadGroupInvite(UPLOAD)).resolves.toEqual(ID);
  });

  it("refuses the old 43-character id from a server", async () => {
    stubFetch(200, { blob_id: base64UrlEncodeNoPad(new Uint8Array(32)) });
    await expect(client.uploadGroupInvite(UPLOAD)).rejects.toBeInstanceOf(DiscoveryHttpError);
  });
});

describe("the surfaces say 'try again later'", () => {
  it("the /g/ join request failure copy for a wrapped 429, with a retry offered", () => {
    const wrapped = ContactsError.wrap(new DiscoveryRateLimitedError("/x", null));
    expect(joinFailureNotice(wrapped)).toEqual({
      message: INVITE_LINK_RATE_LIMITED,
      offerRetry: true,
    });
  });

  it("resolveGroupInviteSlug surfaces a 429 as rate limited, not as not-found", async () => {
    stubFetch(429);
    const contacts = new ContactsClient(new DiscoveryClient("https://d.test"));
    const err = await contacts
      .resolveGroupInviteSlug(base64UrlEncodeNoPad(ID))
      .catch((e: unknown) => e);
    expect(isDiscoveryRateLimited(err)).toBe(true);
  });

  it("resolveGroupInviteSlug refuses an old 43-character pointer without a request", async () => {
    const calls = stubFetch(200);
    const contacts = new ContactsClient(new DiscoveryClient("https://d.test"));
    await expect(
      contacts.resolveGroupInviteSlug(base64UrlEncodeNoPad(new Uint8Array(32))),
    ).rejects.toThrow();
    expect(calls).toHaveLength(0);
  });

  it("the call-link acceptor reports rateLimited on a 429 fetch", async () => {
    stubFetch(429);
    const result = await acceptCallInviteFromSlug({
      slug: base64UrlEncodeNoPad(ID),
      hash: `#${base64UrlEncodeNoPad(new Uint8Array(12).fill(0x05))}`,
      discoveryClient: new DiscoveryClient("https://d.test", { tokens: fakeTokenPool().pool }),
      joinerAccountId: ACCOUNT,
      // The fetch refuses before the identity is touched.
      joinerIdentity: {} as never,
    });
    expect(result).toEqual({ kind: "rateLimited" });
  });
});
