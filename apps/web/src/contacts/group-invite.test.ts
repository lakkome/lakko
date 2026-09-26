// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import {
  GROUP_INVITE_BLOB_ID_LEN,
  GROUP_INVITE_INLINE_MIN_LEN,
  GROUP_INVITE_LINK_KEY_LEN,
  appendGroupInviteLinkKey,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  decodeGroupInviteLinkKey,
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";

const ORIGIN = "https://lakko.me";

function bytes(len: number, fill = 0xab): Uint8Array {
  return new Uint8Array(len).fill(fill);
}

describe("parseGroupInviteSlug", () => {
  it("classifies a 32-byte slug as a pointer", () => {
    const blobId = bytes(GROUP_INVITE_BLOB_ID_LEN);
    const slug = base64UrlEncodeNoPad(blobId);
    const parsed = parseGroupInviteSlug(slug);
    expect(parsed.kind).toBe("pointer");
    if (parsed.kind === "pointer") {
      expect(parsed.blobId).toEqual(blobId);
    }
  });

  it("classifies a large slug as an inline capability", () => {
    const cap = bytes(140);
    const slug = base64UrlEncodeNoPad(cap);
    const parsed = parseGroupInviteSlug(slug);
    expect(parsed.kind).toBe("inline");
    if (parsed.kind === "inline") {
      expect(parsed.capabilityBytes).toEqual(cap);
    }
  });

  it("classifies the minimal legal inline length as inline", () => {
    const cap = bytes(GROUP_INVITE_INLINE_MIN_LEN);
    const parsed = parseGroupInviteSlug(base64UrlEncodeNoPad(cap));
    expect(parsed.kind).toBe("inline");
  });

  it("rejects an ambiguous mid-length slug (33..99 bytes)", () => {
    const ambiguous = bytes(64);
    expect(() => parseGroupInviteSlug(base64UrlEncodeNoPad(ambiguous))).toThrow();
  });

  it("rejects a too-short slug", () => {
    expect(() => parseGroupInviteSlug(base64UrlEncodeNoPad(bytes(10)))).toThrow();
  });
});

describe("compose*GroupInviteUrl round-trips", () => {
  it("pointer URL decodes back to a pointer slug", () => {
    const blobId = bytes(GROUP_INVITE_BLOB_ID_LEN, 0x11);
    const url = composePointerGroupInviteUrl(blobId, ORIGIN);
    expect(url.startsWith(`${ORIGIN}/g/`)).toBe(true);
    const slug = url.slice(`${ORIGIN}/g/`.length);
    const parsed = parseGroupInviteSlug(slug);
    expect(parsed.kind).toBe("pointer");
    if (parsed.kind === "pointer") expect(parsed.blobId).toEqual(blobId);
  });

  it("inline URL decodes back to an inline slug", () => {
    const cap = bytes(150, 0x22);
    const url = composeInlineGroupInviteUrl(cap, ORIGIN);
    const slug = url.slice(`${ORIGIN}/g/`.length);
    const parsed = parseGroupInviteSlug(slug);
    expect(parsed.kind).toBe("inline");
    if (parsed.kind === "inline") expect(parsed.capabilityBytes).toEqual(cap);
  });

  it("pointer composer rejects a non-32-byte blobId", () => {
    expect(() => composePointerGroupInviteUrl(bytes(31), ORIGIN)).toThrow();
  });

  it("inline composer rejects an under-minimum capability", () => {
    expect(() => composeInlineGroupInviteUrl(bytes(50), ORIGIN)).toThrow();
  });
});

describe("the link key in the fragment (ADR-0028 v2)", () => {
  const key = bytes(GROUP_INVITE_LINK_KEY_LEN, 0x5c);

  it("appends the key as a base64url no-pad fragment to a pointer URL", () => {
    const url = appendGroupInviteLinkKey(
      composePointerGroupInviteUrl(bytes(GROUP_INVITE_BLOB_ID_LEN), ORIGIN),
      key,
    );
    const hashAt = url.indexOf("#");
    expect(hashAt).toBeGreaterThan(0);
    const fragment = url.slice(hashAt + 1);
    expect(fragment).toHaveLength(43);
    expect(fragment).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(base64UrlDecodeNoPad(fragment)).toEqual(key);
  });

  it("appends the same fragment to an inline URL", () => {
    const url = appendGroupInviteLinkKey(composeInlineGroupInviteUrl(bytes(150), ORIGIN), key);
    expect(url.endsWith(`#${base64UrlEncodeNoPad(key)}`)).toBe(true);
  });

  it("refuses a key that is not 32 bytes and a URL that already has a fragment", () => {
    const url = composePointerGroupInviteUrl(bytes(GROUP_INVITE_BLOB_ID_LEN), ORIGIN);
    expect(() => appendGroupInviteLinkKey(url, bytes(31))).toThrow();
    expect(() => appendGroupInviteLinkKey(`${url}#x`, key)).toThrow();
  });

  it("reads the key back from location.hash, with or without the #", () => {
    const fragment = base64UrlEncodeNoPad(key);
    expect(decodeGroupInviteLinkKey(`#${fragment}`)).toEqual(key);
    expect(decodeGroupInviteLinkKey(fragment)).toEqual(key);
  });

  it("yields null, never a throw, for an absent, empty, malformed or wrong-length fragment", () => {
    expect(decodeGroupInviteLinkKey(null)).toBeNull();
    expect(decodeGroupInviteLinkKey(undefined)).toBeNull();
    expect(decodeGroupInviteLinkKey("")).toBeNull();
    expect(decodeGroupInviteLinkKey("#")).toBeNull();
    expect(decodeGroupInviteLinkKey("#not base64!")).toBeNull();
    expect(decodeGroupInviteLinkKey(`#${base64UrlEncodeNoPad(bytes(31))}`)).toBeNull();
    expect(decodeGroupInviteLinkKey(`#${base64UrlEncodeNoPad(bytes(33))}`)).toBeNull();
  });

  it("splits a full link into the slug and the key, so revoke never reads the fragment as the blob id", () => {
    const blobId = bytes(GROUP_INVITE_BLOB_ID_LEN, 0x33);
    const url = appendGroupInviteLinkKey(composePointerGroupInviteUrl(blobId, ORIGIN), key);
    const split = splitGroupInviteLink(url);
    expect(split).not.toBeNull();
    expect(split!.slug).toBe(base64UrlEncodeNoPad(blobId));
    expect(split!.linkKey).toEqual(key);
    const parsed = parseGroupInviteSlug(split!.slug);
    expect(parsed.kind).toBe("pointer");
  });

  it("splits a link without a fragment (a stripped link still works, minus the preview)", () => {
    const url = composePointerGroupInviteUrl(bytes(GROUP_INVITE_BLOB_ID_LEN), ORIGIN);
    const split = splitGroupInviteLink(url);
    expect(split!.linkKey).toBeNull();
    expect(split!.slug).toBe(url.slice(`${ORIGIN}/g/`.length));
  });

  it("is null for a string that is not a group-invite link", () => {
    expect(splitGroupInviteLink("https://lakko.me/i/abc")).toBeNull();
    expect(splitGroupInviteLink("https://lakko.me/g/")).toBeNull();
  });
});
