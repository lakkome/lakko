// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  GROUP_INVITE_BLOB_ID_LEN,
  GROUP_INVITE_INLINE_MIN_LEN,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  parseGroupInviteSlug,
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
