// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealed-link amendment of ADR-0028 / ADR-0009 (2026-09-28) at the edges
 * of the web client: a pasted call link keeps its fragment on the way to the
 * landing page, the call acceptor refuses a link without its secret before
 * any request, and the owner's list remembers the secrets this device minted
 * (the ledger never holds them).
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { callLinkTarget } from "../calls/call-link-slug";
import { acceptCallInviteFromSlug } from "../calls/acceptor";
import { DiscoveryClient } from "../discovery/client";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  inviteLinkSecretFor,
  rememberInviteLinkSecret,
  type SecretStorage,
} from "./invite-link-secrets";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("a pasted call link keeps its fragment", () => {
  it("returns the slug and the #secret", () => {
    expect(callLinkTarget("https://lakko.me/c/AbCdEfGh#0123456789abcdef")).toEqual({
      slug: "AbCdEfGh",
      hash: "#0123456789abcdef",
    });
  });

  it("a paste without a fragment still routes, with an empty hash (the page shows damaged)", () => {
    expect(callLinkTarget("  /c/AbCdEfGh ")).toEqual({ slug: "AbCdEfGh", hash: "" });
  });

  it("is null for something that is not a call link", () => {
    expect(callLinkTarget("https://lakko.me/g/AbCdEfGh#x")).toBeNull();
    expect(callLinkTarget("")).toBeNull();
  });
});

describe("the call acceptor refuses a link without its secret", () => {
  it.each([[null], [""], ["#short"]])("hash %p is damaged, before any request", async (hash) => {
    const fetchMock = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const result = await acceptCallInviteFromSlug({
      slug: base64UrlEncodeNoPad(new Uint8Array(6).fill(1)),
      hash,
      discoveryClient: new DiscoveryClient("https://d.test"),
      joinerAccountId: new Uint8Array(16),
      joinerIdentity: {} as never,
    });
    expect(result).toEqual({ kind: "damaged" });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("the owner's list remembers the secrets this device minted", () => {
  function memoryStorage(): SecretStorage & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (k) => data.get(k) ?? null,
      setItem: (k, v) => void data.set(k, v),
    };
  }

  it("round-trips per account and per ledger id", () => {
    const storage = memoryStorage();
    rememberInviteLinkSecret("acct-a", "AAAAAAAA", "0123456789abcdef", storage);
    rememberInviteLinkSecret("acct-a", "BBBBBBBB", "fedcba9876543210", storage);
    expect(inviteLinkSecretFor("acct-a", "AAAAAAAA", storage)).toBe("0123456789abcdef");
    expect(inviteLinkSecretFor("acct-a", "BBBBBBBB", storage)).toBe("fedcba9876543210");
    expect(inviteLinkSecretFor("acct-b", "AAAAAAAA", storage)).toBeNull();
  });

  it("a storage that throws degrades to 'not known here', never an exception", () => {
    const broken: SecretStorage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    expect(() => rememberInviteLinkSecret("a", "AAAAAAAA", "x", broken)).not.toThrow();
    expect(inviteLinkSecretFor("a", "AAAAAAAA", broken)).toBeNull();
    expect(inviteLinkSecretFor("a", "AAAAAAAA", null)).toBeNull();
  });

  it("a corrupt record reads as empty", () => {
    const storage = memoryStorage();
    storage.data.set("lakko/invite-link-secrets/v1/a", "not json");
    expect(inviteLinkSecretFor("a", "AAAAAAAA", storage)).toBeNull();
  });
});
