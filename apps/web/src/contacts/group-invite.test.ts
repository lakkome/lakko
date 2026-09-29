// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Sealed invite links (the 2026-09-28 amendment of ADR-0028 and ADR-0009,
 * flag day) over the short links of 2026-09-27: every kind (`/i/`, `/c/`,
 * `/g/`) reads `https://<host>/<k>/<slug>#<16-char secret>`, the capability is
 * encrypted under the secret, and a missing or wrong fragment is damaged on
 * every kind. Runs against the real wasm core, so the widths and the seal are
 * the ones the Rust codec exports, not a copy.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import {
  GROUP_INVITE_INLINE_MIN_LEN,
  composeInlineGroupInviteUrl,
  composePointerGroupInviteUrl,
  parseGroupInviteSlug,
  splitGroupInviteLink,
} from "./group-invite";
import {
  INVITE_INLINE_MIN_LEN,
  composeInlineInviteUrl,
  composePointerInviteUrl,
  parseInviteSlug,
} from "./invite";
import {
  CALL_INVITE_INLINE_MIN_LEN,
  composeInlineCallInviteUrl,
  composePointerCallInviteUrl,
  parseCallInviteSlug,
} from "../calls/invite";
import {
  inviteLinkWidths,
  isInviteLinkDamagedError,
  openSealedInviteLink,
  parseInviteLinkFragment,
  splitInviteLink,
  takeInviteLinkMint,
  withLinkFragment,
  type InviteLinkMintParts,
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

/** The slug and the fragment of a composed link. */
function partsOf(url: string, kind: "i" | "c" | "g"): { slug: string; fragment: string } {
  const prefix = `${ORIGIN}/${kind}/`;
  expect(url.startsWith(prefix)).toBe(true);
  const rest = url.slice(prefix.length);
  const hashAt = rest.indexOf("#");
  expect(hashAt).toBeGreaterThan(0);
  return { slug: rest.slice(0, hashAt), fragment: rest.slice(hashAt) };
}

describe("the widths come from the wasm core", () => {
  it("are 6 / 8 / 12 / 16, the secret shared by every kind", () => {
    expect(W).toEqual({ blobIdLen: 6, blobIdUrlLen: 8, linkSecretLen: 12, linkSecretUrlLen: 16 });
    expect(wasm.groupInviteLinkSecretLen()).toBe(W.linkSecretLen);
    expect(wasm.groupInviteLinkSecretUrlLen()).toBe(W.linkSecretUrlLen);
  });
});

describe("the link builder: pointer links", () => {
  const secret = bytes(12, 0x5c);
  const cases = [
    ["i", (id: Uint8Array, s: Uint8Array) => composePointerInviteUrl(id, s, W, ORIGIN)],
    ["c", (id: Uint8Array, s: Uint8Array) => composePointerCallInviteUrl(id, s, W, ORIGIN)],
    ["g", (id: Uint8Array, s: Uint8Array) => composePointerGroupInviteUrl(id, s, W, ORIGIN)],
  ] as const;

  it.each(cases)("/%s/ reads <host>/<k>/<8-char id>#<16-char secret>", (kind, compose) => {
    const blobId = bytes(W.blobIdLen, 0x11);
    const url = compose(blobId, secret);
    const { slug, fragment } = partsOf(url, kind);
    expect(slug).toBe(base64UrlEncodeNoPad(blobId));
    expect(slug).toHaveLength(8);
    expect(fragment).toHaveLength(17);
    expect(base64UrlDecodeNoPad(fragment.slice(1))).toEqual(secret);
    expect(url.length).toBe(`${ORIGIN}/${kind}/`.length + 8 + 1 + 16);
  });

  it.each(cases)("/%s/ refuses an id or a secret of any other width", (_, compose) => {
    for (const len of [5, 7, 32]) {
      expect(() => compose(bytes(len), secret)).toThrow();
    }
    for (const len of [0, 11, 13, 32]) {
      expect(() => compose(bytes(W.blobIdLen), bytes(len))).toThrow();
    }
  });

  it("the three parsers read the pointer back", () => {
    const blobId = bytes(W.blobIdLen, 0x22);
    const slug = base64UrlEncodeNoPad(blobId);
    expect(parseInviteSlug(slug, W)).toEqual({ kind: "pointer", blobId });
    expect(parseCallInviteSlug(slug, W)).toEqual({ kind: "pointer", blobId });
    expect(parseGroupInviteSlug(slug, W)).toEqual({ kind: "pointer", blobId });
  });
});

describe("the link builder: inline links", () => {
  const secret = bytes(12, 0x5c);

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

  it("the inline builders carry the sealed blob and the secret, and refuse an under-floor blob", () => {
    const sealed = bytes(500, 0x22);
    const g = partsOf(composeInlineGroupInviteUrl(sealed, secret, W, ORIGIN), "g");
    expect(parseGroupInviteSlug(g.slug, W)).toEqual({ kind: "inline", sealed });
    expect(parseInviteLinkFragment(g.fragment, W)).toEqual({ kind: "secret", secret });
    expect(partsOf(composeInlineInviteUrl(sealed, secret, W, ORIGIN), "i").slug).toBe(g.slug);
    expect(partsOf(composeInlineCallInviteUrl(sealed, secret, W, ORIGIN), "c").slug).toBe(g.slug);
    expect(() => composeInlineGroupInviteUrl(bytes(50), secret, W, ORIGIN)).toThrow();
    expect(() => composeInlineCallInviteUrl(bytes(399), secret, W, ORIGIN)).toThrow();
    expect(() => composeInlineInviteUrl(sealed, bytes(32), W, ORIGIN)).toThrow();
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

  it("refuses widths between the pointer and the floor, and a bad alphabet", () => {
    for (const len of [1, 5, 7, 10, 64, 99]) {
      expect(() => parseGroupInviteSlug(base64UrlEncodeNoPad(bytes(len)), W)).toThrow();
    }
    expect(() => parseGroupInviteSlug("AAAAAA==", W)).toThrow();
    expect(() => parseGroupInviteSlug("AAAA+AAA", W)).toThrow();
    expect(() => parseGroupInviteSlug("", W)).toThrow();
  });
});

describe("fragment parsing: a link without its secret is damaged", () => {
  const secret = bytes(12, 0x5c);
  const valid = base64UrlEncodeNoPad(secret);

  it("reads a valid secret, with or without the #", () => {
    expect(valid).toHaveLength(16);
    expect(parseInviteLinkFragment(`#${valid}`, W)).toEqual({ kind: "secret", secret });
    expect(parseInviteLinkFragment(valid, W)).toEqual({ kind: "secret", secret });
  });

  it("no fragment is damaged, not 'no preview'", () => {
    for (const f of [null, undefined, "", "#"]) {
      expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "damaged" });
    }
  });

  it("a wrong length is damaged, the old 43-character key included", () => {
    const old = base64UrlEncodeNoPad(bytes(32));
    for (const f of [`#${old}`, "#abc", `#${"A".repeat(15)}`, `#${"A".repeat(17)}`]) {
      expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "damaged" });
    }
  });

  it("a wrong alphabet is damaged, padding and the standard alphabet included", () => {
    for (const f of ["#AAAAAAAAAAAAAAA=", "#AAAAAAAAAAAAAA+/", "#AAAAAAAAAAAAAAA ", "#AAAAAAAAAAAAAAA."]) {
      expect(parseInviteLinkFragment(f, W)).toEqual({ kind: "damaged" });
    }
  });
});

describe("splitting a full link", () => {
  const secret = bytes(12, 0x5c);

  it("separates the slug from the fragment so revoke never reads the fragment as the id", () => {
    const blobId = bytes(W.blobIdLen, 0x33);
    const url = composePointerGroupInviteUrl(blobId, secret, W, ORIGIN);
    const split = splitGroupInviteLink(url, W);
    expect(split?.slug).toBe(base64UrlEncodeNoPad(blobId));
    expect(split?.fragment).toEqual({ kind: "secret", secret });
    expect(splitInviteLink(composePointerInviteUrl(blobId, secret, W, ORIGIN), "i", W)?.slug).toBe(
      base64UrlEncodeNoPad(blobId),
    );
  });

  it("a link without a fragment splits as damaged; another kind or no slug is null", () => {
    const blobId = bytes(W.blobIdLen);
    expect(splitGroupInviteLink(`${ORIGIN}/g/${base64UrlEncodeNoPad(blobId)}`, W)?.fragment).toEqual({
      kind: "damaged",
    });
    expect(splitGroupInviteLink(`${ORIGIN}/i/abc#${"A".repeat(16)}`, W)).toBeNull();
    expect(splitGroupInviteLink(`${ORIGIN}/g/`, W)).toBeNull();
    expect(splitGroupInviteLink(`${ORIGIN}/g/#${"A".repeat(16)}`, W)).toBeNull();
  });
});

describe("the auth round trip keeps the fragment", () => {
  it("appends the fragment to the return path", () => {
    expect(withLinkFragment("/i/abcdefgh", "#AAAAAAAAAAAAAAAA")).toBe("/i/abcdefgh#AAAAAAAAAAAAAAAA");
    expect(withLinkFragment("/c/abcdefgh", "AAAAAAAAAAAAAAAA")).toBe("/c/abcdefgh#AAAAAAAAAAAAAAAA");
    for (const h of ["", "#", null, undefined]) {
      expect(withLinkFragment("/i/abcdefgh", h)).toBe("/i/abcdefgh");
    }
  });
});

// ----------------------------------------------------------------------------
// Compose, then open, through the real FFI
// ----------------------------------------------------------------------------

const OWNER_SEED = bytes(32, 0x01);
const OTHER_SEED = bytes(32, 0x02);
const ACCOUNT = bytes(16, 0x07);
const JOINER = bytes(16, 0x09);

function mintContact(seed = OWNER_SEED): InviteLinkMintParts {
  const identity = wasm.Identity.fromSeed(seed);
  try {
    return takeInviteLinkMint(
      wasm.InviteCapability.compose(seed, ACCOUNT, identity.x25519PublicKey, 0n, 0, false, "Hi"),
    );
  } finally {
    identity.free();
  }
}

function mintCall(seed = OWNER_SEED): InviteLinkMintParts {
  const identity = wasm.Identity.fromSeed(seed);
  try {
    const kp = identity.createKeyPackage();
    return takeInviteLinkMint(
      wasm.composeCallInviteCapability(seed, ACCOUNT, bytes(16, 0xca), 0n, 0, true, false, kp, ""),
    );
  } finally {
    identity.free();
  }
}

function mintGroup(seed = OWNER_SEED, group = bytes(32, 0xa1)): InviteLinkMintParts {
  return takeInviteLinkMint(
    wasm.composeGroupInviteCapabilityWithPreview(seed, group, 0n, "Team", "Anna", 4),
  );
}

type Opened = { wire: Uint8Array; ownerKey: Uint8Array };

const KINDS = [
  {
    kind: "i" as const,
    mint: mintContact,
    minLen: INVITE_INLINE_MIN_LEN,
    open: (sealed: Uint8Array, secret: Uint8Array): Opened => {
      const cap = wasm.InviteCapability.openLink(sealed, secret);
      return { wire: cap.wire, ownerKey: cap.ownerSignaturePubkey };
    },
    pointer: (id: Uint8Array, s: Uint8Array) => composePointerInviteUrl(id, s, W, ORIGIN),
    inline: (b: Uint8Array, s: Uint8Array) => composeInlineInviteUrl(b, s, W, ORIGIN),
  },
  {
    kind: "c" as const,
    mint: mintCall,
    minLen: CALL_INVITE_INLINE_MIN_LEN,
    open: (sealed: Uint8Array, secret: Uint8Array): Opened => {
      const cap = wasm.CallInviteCapability.openLink(sealed, secret);
      return { wire: cap.wire, ownerKey: cap.ownerSignaturePubkey };
    },
    pointer: (id: Uint8Array, s: Uint8Array) => composePointerCallInviteUrl(id, s, W, ORIGIN),
    inline: (b: Uint8Array, s: Uint8Array) => composeInlineCallInviteUrl(b, s, W, ORIGIN),
  },
  {
    kind: "g" as const,
    mint: () => mintGroup(),
    minLen: GROUP_INVITE_INLINE_MIN_LEN,
    open: (sealed: Uint8Array, secret: Uint8Array): Opened => {
      const cap = wasm.GroupInviteCapability.openLink(sealed, secret);
      return { wire: cap.wire, ownerKey: cap.issuerSignaturePubkey };
    },
    pointer: (id: Uint8Array, s: Uint8Array) => composePointerGroupInviteUrl(id, s, W, ORIGIN),
    inline: (b: Uint8Array, s: Uint8Array) => composeInlineGroupInviteUrl(b, s, W, ORIGIN),
  },
];

describe.each(KINDS)("/$kind/ compose then open through the real FFI", (k) => {
  const blobId = bytes(6, 0x3c);

  function openUrl(url: string, stored: Uint8Array | null) {
    const { slug, fragment } = partsOf(url, k.kind);
    const fetchSealed = vi.fn(async () => stored);
    return {
      fetchSealed,
      result: openSealedInviteLink({
        slug,
        fragment,
        widths: W,
        inlineMinLen: k.minLen,
        what: k.kind,
        fetchSealed,
        open: k.open,
      }),
    };
  }

  it("the mint carries a 12-byte secret and the issuer key", () => {
    const mint = k.mint();
    expect(mint.linkSecret).toHaveLength(12);
    expect(mint.issuerPubkey).toHaveLength(32);
    expect(mint.sealed.length).toBeGreaterThan(30);
  });

  it("a pointer link fetches the blob by id and opens to the signed capability", async () => {
    const mint = k.mint();
    const { fetchSealed, result } = openUrl(k.pointer(blobId, mint.linkSecret), mint.sealed);
    const opened = await result;
    expect(fetchSealed).toHaveBeenCalledWith(blobId);
    expect(opened.kind).toBe("opened");
    if (opened.kind !== "opened") return;
    expect(opened.blobId).toEqual(blobId);
    expect(opened.capability.ownerKey).toEqual(mint.issuerPubkey);
    // The joiner tag is computed over the PLAINTEXT wire the open returns.
    const tag = wasm.computeJoinerInviteTag(opened.capability.wire, JOINER);
    expect(tag).toHaveLength(32);
    expect(wasm.computeJoinerInviteTag(opened.capability.wire, JOINER)).toEqual(tag);
    expect(tag).not.toEqual(wasm.computeJoinerInviteTag(mint.sealed, JOINER));
  });

  it("an inline link opens without a fetch and has no ledger id", async () => {
    const mint = k.mint();
    const { fetchSealed, result } = openUrl(k.inline(mint.sealed, mint.linkSecret), null);
    const opened = await result;
    expect(fetchSealed).not.toHaveBeenCalled();
    expect(opened.kind).toBe("opened");
    if (opened.kind === "opened") expect(opened.blobId).toBeNull();
  });

  it("a link without its fragment is damaged and never fetches", async () => {
    const mint = k.mint();
    const url = k.pointer(blobId, mint.linkSecret);
    const bare = url.slice(0, url.indexOf("#"));
    const fetchSealed = vi.fn(async () => mint.sealed);
    const opened = await openSealedInviteLink({
      slug: bare.slice(`${ORIGIN}/${k.kind}/`.length),
      fragment: "",
      widths: W,
      inlineMinLen: k.minLen,
      what: k.kind,
      fetchSealed,
      open: k.open,
    });
    expect(opened).toEqual({ kind: "damaged" });
    expect(fetchSealed).not.toHaveBeenCalled();
  });

  it("a wrong secret is damaged, and the core names it as such", async () => {
    const mint = k.mint();
    const { result } = openUrl(k.pointer(blobId, bytes(12, 0x77)), mint.sealed);
    expect(await result).toEqual({ kind: "damaged" });
    let thrown: unknown;
    try {
      k.open(mint.sealed, bytes(12, 0x77));
    } catch (err) {
      thrown = err;
    }
    expect(isInviteLinkDamagedError(thrown)).toBe(true);
  });

  it("a tampered or truncated blob is damaged", async () => {
    const mint = k.mint();
    const tampered = mint.sealed.slice();
    tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 0x01;
    expect(await openUrl(k.pointer(blobId, mint.linkSecret), tampered).result).toEqual({
      kind: "damaged",
    });
    expect(
      await openUrl(k.pointer(blobId, mint.linkSecret), mint.sealed.slice(0, 20)).result,
    ).toEqual({ kind: "damaged" });
  });

  it("a 404 from the ledger is not-found", async () => {
    const mint = k.mint();
    expect(await openUrl(k.pointer(blobId, mint.linkSecret), null).result).toEqual({
      kind: "not-found",
    });
  });
});

describe("a blob of another kind behind the path is damaged", () => {
  it("each kind refuses the other two kinds' blobs under their own secret", async () => {
    const mints = { i: mintContact(), c: mintCall(), g: mintGroup() };
    for (const k of KINDS) {
      for (const other of KINDS) {
        if (other.kind === k.kind) continue;
        const m = mints[other.kind];
        expect(() => k.open(m.sealed, m.linkSecret)).toThrow();
      }
    }
  });
});

describe("the /g/ preview opens under the link secret", () => {
  it("the secret that opened the capability opens its preview", () => {
    const mint = mintGroup();
    const cap = wasm.GroupInviteCapability.openLink(mint.sealed, mint.linkSecret);
    expect(resolveInvitePreview(cap, mint.linkSecret)).toEqual({
      kind: "preview",
      preview: { groupName: "Team", inviterName: "Anna", memberCount: 4 },
    });
  });

  it("a preview that does not open under the given secret is damaged", () => {
    const mint = mintGroup();
    const cap = wasm.GroupInviteCapability.openLink(mint.sealed, mint.linkSecret);
    expect(resolveInvitePreview(cap, bytes(12, 0x77))).toEqual({ kind: "damaged" });
  });

  it("a capability minted without a preview shows the page without one", () => {
    const mint = takeInviteLinkMint(wasm.composeGroupInviteCapability(OTHER_SEED, bytes(32, 0xb2), 0n));
    const cap = wasm.GroupInviteCapability.openLink(mint.sealed, mint.linkSecret);
    expect(cap.hasPreview).toBe(false);
    expect(resolveInvitePreview(cap, mint.linkSecret)).toEqual({ kind: "none" });
  });

  it("openPreview itself refuses the pre-amendment 32-byte key", () => {
    const mint = mintGroup();
    const cap = wasm.GroupInviteCapability.openLink(mint.sealed, mint.linkSecret);
    expect(() => cap.openPreview(bytes(32))).toThrow();
  });
});
