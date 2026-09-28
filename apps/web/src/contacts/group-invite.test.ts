// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Short invite links (ADR-0028 amendment of 2026-09-27, flag day): the three
 * link kinds' compose / parse rules, the refusal of every pre-amendment form,
 * and the `/g/` rule that a fragment which does not open the preview refuses
 * the link. Runs against the real wasm core, so the widths are the ones the
 * Rust codec exports, not a copy.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import {
  GROUP_INVITE_INLINE_MIN_LEN,
  appendGroupInviteLinkSecret,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  decodeGroupInviteFragment,
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";
import { INVITE_INLINE_MIN_LEN, composePointerInviteUrl, parseInviteSlug } from "./invite";
import {
  CALL_INVITE_INLINE_MIN_LEN,
  composePointerCallInviteUrl,
  parseCallInviteSlug,
} from "../calls/invite";
import { inviteLinkWidths, type InviteLinkWidths } from "./invite-link";
import { resolveInvitePreview } from "../routes/join-group-preview";

const ORIGIN = "https://lakko.me";
let W: InviteLinkWidths;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  W = inviteLinkWidths(wasm);
});

function bytes(len: number, fill = 0xab): Uint8Array {
  return new Uint8Array(len).fill(fill);
}

describe("the widths come from the wasm core", () => {
  it("are 6 / 8 / 12 / 16", () => {
    expect(W).toEqual({ blobIdLen: 6, blobIdUrlLen: 8, linkSecretLen: 12, linkSecretUrlLen: 16 });
  });
});

describe("pointer links are 8 characters, for all three kinds", () => {
  it("/g/ pointer round-trips", () => {
    const blobId = bytes(W.blobIdLen, 0x11);
    const url = composePointerGroupInviteUrl(blobId, W, ORIGIN);
    const slug = url.slice(`${ORIGIN}/g/`.length);
    expect(slug).toHaveLength(8);
    const parsed = parseGroupInviteSlug(slug, W);
    expect(parsed).toEqual({ kind: "pointer", blobId });
  });

  it("/i/ pointer round-trips", () => {
    const blobId = bytes(W.blobIdLen, 0x22);
    const url = composePointerInviteUrl(blobId, W, ORIGIN);
    const slug = url.slice(`${ORIGIN}/i/`.length);
    expect(slug).toHaveLength(8);
    expect(parseInviteSlug(slug, W)).toEqual({ kind: "pointer", blobId });
  });

  it("/c/ pointer round-trips", () => {
    const blobId = bytes(W.blobIdLen, 0x33);
    const url = composePointerCallInviteUrl(blobId, W, ORIGIN);
    const slug = url.slice(`${ORIGIN}/c/`.length);
    expect(slug).toHaveLength(8);
    expect(parseCallInviteSlug(slug, W)).toEqual({ kind: "pointer", blobId });
  });

  it("composers refuse an id of any other width, the old 32 bytes included", () => {
    for (const len of [5, 7, 32]) {
      expect(() => composePointerGroupInviteUrl(bytes(len), W, ORIGIN)).toThrow();
      expect(() => composePointerInviteUrl(bytes(len), W, ORIGIN)).toThrow();
      expect(() => composePointerCallInviteUrl(bytes(len), W, ORIGIN)).toThrow();
    }
  });
});

describe("inline links keep their shape", () => {
  it("/g/ inline round-trips", () => {
    const cap = bytes(150, 0x22);
    const slug = composeInlineGroupInviteUrl(cap, ORIGIN).slice(`${ORIGIN}/g/`.length);
    expect(parseGroupInviteSlug(slug, W)).toEqual({ kind: "inline", capabilityBytes: cap });
  });

  it("each kind's floor is inline, one byte less is refused", () => {
    const cases: [(s: string) => unknown, number][] = [
      [(s) => parseGroupInviteSlug(s, W), GROUP_INVITE_INLINE_MIN_LEN],
      [(s) => parseInviteSlug(s, W), INVITE_INLINE_MIN_LEN],
      [(s) => parseCallInviteSlug(s, W), CALL_INVITE_INLINE_MIN_LEN],
    ];
    for (const [parse, floor] of cases) {
      expect(floor).toBeGreaterThanOrEqual(100);
      expect(parse(base64UrlEncodeNoPad(bytes(floor)))).toMatchObject({ kind: "inline" });
      expect(() => parse(base64UrlEncodeNoPad(bytes(floor - 1)))).toThrow();
    }
  });

  it("the inline composer refuses an under-minimum capability", () => {
    expect(() => composeInlineGroupInviteUrl(bytes(50), ORIGIN)).toThrow();
  });
});

describe("old and malformed slugs are refused, fail-closed", () => {
  const oldId = base64UrlEncodeNoPad(bytes(32, 0x44));

  it("the pre-amendment 43-character id is refused by all three parsers", () => {
    expect(oldId).toHaveLength(43);
    expect(() => parseGroupInviteSlug(oldId, W)).toThrow();
    expect(() => parseInviteSlug(oldId, W)).toThrow();
    expect(() => parseCallInviteSlug(oldId, W)).toThrow();
  });

  it("refuses widths between the pointer and the floor, and short slugs", () => {
    for (const len of [1, 5, 7, 10, 64, 99]) {
      expect(() => parseGroupInviteSlug(base64UrlEncodeNoPad(bytes(len)), W)).toThrow();
    }
  });

  it("refuses a slug outside the base64url alphabet, padding included", () => {
    expect(() => parseGroupInviteSlug("AAAAAA==", W)).toThrow();
    expect(() => parseGroupInviteSlug("AAAA+AAA", W)).toThrow();
    expect(() => parseGroupInviteSlug("", W)).toThrow();
  });
});

describe("the /g/ link secret in the fragment", () => {
  const secret = bytes(12, 0x5c);

  it("is 16 characters, appended to a pointer link of about 45 characters", () => {
    const url = appendGroupInviteLinkSecret(
      composePointerGroupInviteUrl(bytes(W.blobIdLen), W, ORIGIN),
      secret,
      W,
    );
    const fragment = url.slice(url.indexOf("#") + 1);
    expect(fragment).toHaveLength(16);
    expect(base64UrlDecodeNoPad(fragment)).toEqual(secret);
    expect(url.length).toBe(`${ORIGIN}/g/`.length + 8 + 1 + 16);
  });

  it("refuses a secret of another width (the old 32-byte key included) and a second fragment", () => {
    const url = composePointerGroupInviteUrl(bytes(W.blobIdLen), W, ORIGIN);
    expect(() => appendGroupInviteLinkSecret(url, bytes(32), W)).toThrow();
    expect(() => appendGroupInviteLinkSecret(url, bytes(11), W)).toThrow();
    expect(() => appendGroupInviteLinkSecret(`${url}#x`, secret, W)).toThrow();
  });

  it("reads a secret back, with or without the #", () => {
    const f = base64UrlEncodeNoPad(secret);
    expect(decodeGroupInviteFragment(`#${f}`, W)).toEqual({ kind: "secret", secret });
    expect(decodeGroupInviteFragment(f, W)).toEqual({ kind: "secret", secret });
  });

  it("an absent or empty fragment is none", () => {
    for (const f of [null, undefined, "", "#"]) {
      expect(decodeGroupInviteFragment(f, W)).toEqual({ kind: "none" });
    }
  });

  it("any other fragment is damaged: the old 43-character key, a wrong length, a bad alphabet", () => {
    const old = base64UrlEncodeNoPad(bytes(32));
    expect(old).toHaveLength(43);
    for (const f of [`#${old}`, "#abc", `#${"A".repeat(15)}`, `#${"A".repeat(17)}`, "#AAAAAAAAAAAAAAA="]) {
      expect(decodeGroupInviteFragment(f, W)).toEqual({ kind: "damaged" });
    }
  });

  it("splits a full link so revoke never reads the fragment as the id", () => {
    const blobId = bytes(W.blobIdLen, 0x33);
    const url = appendGroupInviteLinkSecret(composePointerGroupInviteUrl(blobId, W, ORIGIN), secret, W);
    const split = splitGroupInviteLink(url, W);
    expect(split?.slug).toBe(base64UrlEncodeNoPad(blobId));
    expect(split?.fragment).toEqual({ kind: "secret", secret });
    expect(parseGroupInviteSlug(split!.slug, W)).toEqual({ kind: "pointer", blobId });
  });

  it("splits a link without a fragment, and is null for a non-/g/ string", () => {
    const url = composePointerGroupInviteUrl(bytes(W.blobIdLen), W, ORIGIN);
    expect(splitGroupInviteLink(url, W)?.fragment).toEqual({ kind: "none" });
    expect(splitGroupInviteLink("https://lakko.me/i/abc", W)).toBeNull();
    expect(splitGroupInviteLink("https://lakko.me/g/", W)).toBeNull();
  });
});

describe("a fragment that does not open the preview refuses the link", () => {
  const seedA = bytes(32, 0x01);
  const seedB = bytes(32, 0x02);
  const groupA = bytes(32, 0xa1);
  const groupB = bytes(32, 0xb2);

  function mint(seed: Uint8Array, group: Uint8Array) {
    const m = wasm.composeGroupInviteCapabilityWithPreview(seed, group, 0n, "Team", "Anna", 4);
    const out = { wire: m.wire, secret: m.linkKey };
    m.free();
    return out;
  }

  function decode(wire: Uint8Array) {
    return wasm.GroupInviteCapability.decodeAndVerify(wire);
  }

  it("the minted secret is 12 bytes and opens its own preview", () => {
    const { wire, secret } = mint(seedA, groupA);
    expect(secret).toHaveLength(12);
    const outcome = resolveInvitePreview(decode(wire), { kind: "secret", secret });
    expect(outcome).toEqual({
      kind: "preview",
      preview: { groupName: "Team", inviterName: "Anna", memberCount: 4 },
    });
  });

  it("no fragment shows the page without a preview", () => {
    const { wire } = mint(seedA, groupA);
    expect(resolveInvitePreview(decode(wire), { kind: "none" })).toEqual({ kind: "none" });
  });

  it("a wrong secret is damaged", () => {
    const { wire } = mint(seedA, groupA);
    expect(resolveInvitePreview(decode(wire), { kind: "secret", secret: bytes(12, 0x77) })).toEqual({
      kind: "damaged",
    });
  });

  it("a capability swapped in under a live id (another issuer or another group) is damaged", () => {
    const { secret } = mint(seedA, groupA);
    const otherIssuer = mint(seedB, groupA).wire;
    const otherGroup = mint(seedA, groupB).wire;
    for (const wire of [otherIssuer, otherGroup]) {
      expect(resolveInvitePreview(decode(wire), { kind: "secret", secret })).toEqual({
        kind: "damaged",
      });
    }
  });

  it("a capability without a preview under a secret is damaged, never a preview-less join", () => {
    const wire = wasm.composeGroupInviteCapability(seedA, groupA, 0n);
    const cap = decode(wire);
    expect(cap.hasPreview).toBe(false);
    expect(resolveInvitePreview(cap, { kind: "secret", secret: bytes(12) })).toEqual({
      kind: "damaged",
    });
    expect(resolveInvitePreview(cap, { kind: "none" })).toEqual({ kind: "none" });
  });

  it("a malformed fragment is damaged before the capability is consulted", () => {
    const { wire } = mint(seedA, groupA);
    expect(resolveInvitePreview(decode(wire), { kind: "damaged" })).toEqual({ kind: "damaged" });
  });

  it("openPreview itself refuses the pre-amendment 32-byte key", () => {
    const { wire } = mint(seedA, groupA);
    expect(() => decode(wire).openPreview(bytes(32))).toThrow();
  });
});
