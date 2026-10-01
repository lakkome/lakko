// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Short, sealed invite links (ADR-0028 amendments of 2026-09-27 and
 * 2026-09-28, flag days): the three link kinds' compose / parse rules, the
 * refusal of every pre-amendment form, the sealed blob that opens only under
 * the fragment's secret (a missing, malformed or wrong fragment and a
 * substituted blob are all DAMAGED), and the `/g/` rule that a preview which
 * does not open refuses the link. Runs against the real wasm core, so the
 * widths are the ones the Rust codec exports, not a copy.
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
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";
import { INVITE_INLINE_MIN_LEN, composePointerInviteUrl, parseInviteSlug } from "./invite";
import {
  CALL_INVITE_INLINE_MIN_LEN,
  composePointerCallInviteUrl,
  parseCallInviteSlug,
} from "../calls/invite";
import {
  InviteLinkDamagedError,
  composeSealedInviteLink,
  inviteLinkWidths,
  isInviteLinkDamaged,
  openInviteLinkFromUrl,
  parseInviteLinkFragment,
  type InviteLinkWidths,
} from "./invite-link";
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
    expect(parseGroupInviteSlug(slug, W)).toEqual({ kind: "inline", sealed: cap });
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

describe("the link secret in the fragment", () => {
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
    expect(parseInviteLinkFragment(`#${f}`, W)).toEqual({ kind: "secret", secret });
    expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "secret", secret });
  });

  it("an absent or empty fragment is damaged: without the secret nothing opens", () => {
    for (const f of [null, undefined, "", "#"]) {
      expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "damaged" });
    }
  });

  it("any other fragment is damaged: the old 43-character key, a wrong length, a bad alphabet", () => {
    const old = base64UrlEncodeNoPad(bytes(32));
    expect(old).toHaveLength(43);
    for (const f of [`#${old}`, "#abc", `#${"A".repeat(15)}`, `#${"A".repeat(17)}`, "#AAAAAAAAAAAAAAA="]) {
      expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "damaged" });
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

  it("splits a link without a fragment as damaged, and is null for a non-/g/ string", () => {
    const url = composePointerGroupInviteUrl(bytes(W.blobIdLen), W, ORIGIN);
    expect(splitGroupInviteLink(url, W)?.fragment).toEqual({ kind: "damaged" });
    expect(splitGroupInviteLink("https://lakko.me/i/abc", W)).toBeNull();
    expect(splitGroupInviteLink("https://lakko.me/g/", W)).toBeNull();
  });
});

describe("sealed links: compose, upload shape, open, damaged (all three kinds)", () => {
  const seedA = bytes(32, 0x01);
  const seedB = bytes(32, 0x02);
  const groupA = bytes(32, 0xa1);
  const account = bytes(16, 0x07);
  const x25519 = bytes(32, 0x09);

  function mintGroup(seed = seedA, group = groupA) {
    const m = wasm.composeGroupInviteCapabilityWithPreview(seed, group, 0n, "Team", "Anna", 4);
    const out = { sealed: m.sealed, secret: m.linkSecret, issuer: m.issuerPubkey };
    m.free();
    return out;
  }
  function mintContact() {
    // ADR-0060 section 6: every /i/ and /c/ mint carries its link's reach grant.
    const linkId = wasm.generateLinkId();
    const grant = wasm.deriveReachGrant(seedA, wasm.ReachClass.Link, linkId, 0);
    const m = wasm.InviteCapability.compose(seedA, account, x25519, 0n, 0, false, "hi", linkId, grant);
    grant.free();
    const out = { sealed: m.sealed, secret: m.linkSecret, issuer: m.issuerPubkey };
    m.free();
    return out;
  }
  function mintCall() {
    const identity = new wasm.Identity("alice");
    const kp = identity.createKeyPackage();
    const linkId = wasm.generateLinkId();
    const grant = wasm.deriveReachGrant(seedA, wasm.ReachClass.Link, linkId, 0);
    const m = wasm.composeCallInviteCapability(
      seedA,
      account,
      bytes(16, 0x3c),
      0n,
      0,
      true,
      false,
      kp,
      "",
      linkId,
      grant,
    );
    grant.free();
    const out = { sealed: m.sealed, secret: m.linkSecret, issuer: m.issuerPubkey };
    m.free();
    identity.free();
    return out;
  }

  it("every mint returns a 12-byte secret and the issuer's 32-byte key, never plaintext", () => {
    for (const m of [mintGroup(), mintContact(), mintCall()]) {
      expect(m.secret).toHaveLength(W.linkSecretLen);
      expect(m.issuer).toHaveLength(32);
      // The sealed blob is not the capability: it does not verify as one.
      expect(() => wasm.GroupInviteCapability.decodeAndVerify(m.sealed)).toThrow();
    }
  });

  it("each kind opens under its own secret and exposes the plaintext wire", () => {
    const g = mintGroup();
    const gc = wasm.GroupInviteCapability.openLink(g.sealed, g.secret);
    expect(gc.groupId).toEqual(groupA);
    expect(wasm.GroupInviteCapability.decodeAndVerify(gc.wire).groupId).toEqual(groupA);
    const c = mintContact();
    const cc = wasm.InviteCapability.openLink(c.sealed, c.secret);
    expect(cc.ownerAccountId).toEqual(account);
    expect(cc.label).toBe("hi");
    const k = mintCall();
    const kc = wasm.CallInviteCapability.openLink(k.sealed, k.secret);
    expect(kc.callId).toEqual(bytes(16, 0x3c));
  });

  it("a wrong secret, another kind's blob, or a tampered blob is the damaged refusal", () => {
    const g = mintGroup();
    const c = mintContact();
    const refusals = [
      () => wasm.GroupInviteCapability.openLink(g.sealed, bytes(12, 0x77)),
      () => wasm.InviteCapability.openLink(g.sealed, g.secret),
      () => wasm.GroupInviteCapability.openLink(c.sealed, c.secret),
      () => {
        const t = g.sealed.slice();
        t[t.length - 1] = (t[t.length - 1] ?? 0) ^ 1;
        return wasm.GroupInviteCapability.openLink(t, g.secret);
      },
    ];
    for (const refuse of refusals) {
      let caught: unknown = null;
      try {
        refuse();
      } catch (err) {
        caught = err;
      }
      expect(caught).not.toBeNull();
      expect(isInviteLinkDamaged(caught)).toBe(true);
    }
  });

  it("the composed pointer and inline links carry the secret and open end to end", async () => {
    const g = mintGroup();
    const blobId = bytes(W.blobIdLen, 0x42);
    for (const pointer of [blobId, null]) {
      const link = composeSealedInviteLink({
        origin: ORIGIN,
        prefix: "/g/",
        blobId: pointer,
        sealed: g.sealed,
        secret: g.secret,
        widths: W,
      });
      const split = splitGroupInviteLink(link, W)!;
      const hash = link.slice(link.indexOf("#"));
      expect(hash).toHaveLength(17);
      const fetched: Uint8Array[] = [];
      const opened = await openInviteLinkFromUrl({
        slug: split.slug,
        hash,
        widths: W,
        inlineMinLen: GROUP_INVITE_INLINE_MIN_LEN,
        what: "group-invite",
        fetchPointer: async (id) => {
          fetched.push(id);
          return g.sealed;
        },
        open: (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
      });
      expect(opened.kind).toBe("opened");
      if (opened.kind !== "opened") throw new Error("unreachable");
      expect(opened.capability.groupId).toEqual(groupA);
      expect(opened.blobId).toEqual(pointer);
      expect(fetched).toEqual(pointer === null ? [] : [blobId]);
    }
  });

  it("a missing or malformed fragment is damaged before any request", async () => {
    const g = mintGroup();
    for (const hash of [null, "", "#", "#abc", `#${base64UrlEncodeNoPad(bytes(32))}`]) {
      let requests = 0;
      await expect(
        openInviteLinkFromUrl({
          slug: base64UrlEncodeNoPad(bytes(W.blobIdLen)),
          hash,
          widths: W,
          inlineMinLen: GROUP_INVITE_INLINE_MIN_LEN,
          what: "group-invite",
          fetchPointer: async () => {
            requests += 1;
            return g.sealed;
          },
          open: (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
        }),
      ).rejects.toBeInstanceOf(InviteLinkDamagedError);
      expect(requests).toBe(0);
    }
  });

  it("a blob a server swapped in under a live id is damaged", async () => {
    const mine = mintGroup(seedA);
    const theirs = mintGroup(seedB);
    await expect(
      openInviteLinkFromUrl({
        slug: base64UrlEncodeNoPad(bytes(W.blobIdLen)),
        hash: `#${base64UrlEncodeNoPad(mine.secret)}`,
        widths: W,
        inlineMinLen: GROUP_INVITE_INLINE_MIN_LEN,
        what: "group-invite",
        fetchPointer: async () => theirs.sealed,
        open: (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
      }),
    ).rejects.toBeInstanceOf(InviteLinkDamagedError);
  });

  it("a 404 is not-found, not damaged", async () => {
    const g = mintGroup();
    const opened = await openInviteLinkFromUrl({
      slug: base64UrlEncodeNoPad(bytes(W.blobIdLen)),
      hash: `#${base64UrlEncodeNoPad(g.secret)}`,
      widths: W,
      inlineMinLen: GROUP_INVITE_INLINE_MIN_LEN,
      what: "group-invite",
      fetchPointer: async () => null,
      open: (sealed, secret) => wasm.GroupInviteCapability.openLink(sealed, secret),
    });
    expect(opened).toEqual({ kind: "not-found" });
  });
});

describe("the /g/ preview opens under the same secret", () => {
  const seedA = bytes(32, 0x01);
  const groupA = bytes(32, 0xa1);

  function mint() {
    const m = wasm.composeGroupInviteCapabilityWithPreview(seedA, groupA, 0n, "Team", "Anna", 4);
    const cap = wasm.GroupInviteCapability.openLink(m.sealed, m.linkSecret);
    const out = { cap, secret: m.linkSecret };
    m.free();
    return out;
  }

  it("the link secret opens the preview", () => {
    const { cap, secret } = mint();
    expect(resolveInvitePreview(cap, secret)).toEqual({
      kind: "preview",
      preview: { groupName: "Team", inviterName: "Anna", memberCount: 4 },
    });
  });

  it("a preview that does not open under the secret is damaged", () => {
    const { cap } = mint();
    expect(resolveInvitePreview(cap, bytes(12, 0x77))).toEqual({ kind: "damaged" });
  });

  it("a link minted without a preview shows the page without one", () => {
    const m = wasm.composeGroupInviteCapability(seedA, groupA, 0n);
    const cap = wasm.GroupInviteCapability.openLink(m.sealed, m.linkSecret);
    expect(cap.hasPreview).toBe(false);
    expect(resolveInvitePreview(cap, m.linkSecret)).toEqual({ kind: "none" });
  });

  it("openPreview itself refuses the pre-amendment 32-byte key", () => {
    const { cap } = mint();
    expect(() => cap.openPreview(bytes(32))).toThrow();
  });
});
