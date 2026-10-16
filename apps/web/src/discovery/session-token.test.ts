// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The bearer token on discovery's `require_session` routes.
 *
 * A `DiscoveryClient` built once and handed to a long-lived `ContactsClient`
 * (the chat route's) had no token at all, so the group-invite upload and
 * revoke were 401 on every call. A client built with a `sessionToken`
 * provider reads it at REQUEST time: the current session's token, a new one
 * after a re-login, none after sign-out.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DiscoveryClient } from "./client";

let authHeaders: (string | undefined)[];

beforeEach(() => {
  authHeaders = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
      const headers = (init?.headers ?? {}) as Record<string, string>;
      authHeaders.push(headers.Authorization);
      if (init?.method === "DELETE") return new Response(null, { status: 204 });
      return new Response(JSON.stringify({ blob_id: "x" }), { status: 200 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

const REVOKE = { signed_at_ms: 1, signature: "c2ln" };

describe("DiscoveryClient session token", () => {
  it("a provider is read at request time, surviving a re-login and a sign-out", async () => {
    let token: string | null = "first";
    const client = new DiscoveryClient("/api/discovery", { sessionToken: () => token });

    await client.groupInviteRevoke("slug", REVOKE);
    token = "second";
    await client.groupInviteRevoke("slug", REVOKE);
    token = null;
    await client.groupInviteRevoke("slug", REVOKE);

    expect(authHeaders).toEqual(["Bearer first", "Bearer second", undefined]);
  });

  it("the group-invite upload carries the provider's token", async () => {
    const client = new DiscoveryClient("/api/discovery", { sessionToken: () => "tok" });
    await client.groupInviteUpload({
      sealed: "",
      revocation_pubkey: "",
      expiry_ms: 0,
      max_uses: 0,
    });
    expect(authHeaders).toEqual(["Bearer tok"]);
  });

  it("without a provider, `setSessionToken` still sets the header", async () => {
    const client = new DiscoveryClient("/api/discovery");
    await client.groupInviteRevoke("slug", REVOKE);
    client.setSessionToken("set");
    await client.groupInviteRevoke("slug", REVOKE);
    expect(authHeaders).toEqual([undefined, "Bearer set"]);
  });
});
