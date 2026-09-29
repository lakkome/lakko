// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shape rules every invite link shares (`/i/`, `/c/`, `/g/`), after the
 * short-link amendment of ADR-0028 (2026-09-27) and the sealed-link amendment
 * of the 2026-09-28 security pass (both flag days, no dual format).
 *
 *   * Every link reads `https://<host>/<k>/<slug>#<secret>`.
 *   * The **secret** is 12 random bytes, exactly 16 base64url characters in
 *     the fragment. The capability is encrypted under it, so a link whose
 *     fragment is missing, of the wrong length or of the wrong alphabet is
 *     **damaged** on every kind: there is nothing to open it with.
 *   * A **pointer** slug is the server-assigned ledger id: 6 random bytes,
 *     8 base64url characters, no padding. The ledger holds the sealed blob.
 *   * An **inline** slug is the whole sealed blob (the fallback when the
 *     upload fails on transport), never shorter than the per-kind floor
 *     (always >= 100 bytes).
 *   * Anything else is refused, including the pre-amendment 32-byte /
 *     43-character content-addressed id.
 *
 * The widths come from the wasm core (`inviteBlobIdLen`,
 * `inviteLinkSecretLen` and friends) so the client cannot drift from the
 * Rust codec; every helper here takes them as an argument, read once from the
 * loaded wasm namespace via {@link inviteLinkWidths}. The copy for the
 * link-level failures the landing pages show lives here too, in a plain
 * module `vitest` can load.
 */
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import type { Wasm } from "../wasm";

/** The four widths a link is checked against. */
export interface InviteLinkWidths {
  /** Ledger id, bytes (6). */
  readonly blobIdLen: number;
  /** Ledger id in a URL, base64url characters (8). */
  readonly blobIdUrlLen: number;
  /** Link secret, bytes (12), every kind. */
  readonly linkSecretLen: number;
  /** Link secret in a URL fragment, base64url characters (16). */
  readonly linkSecretUrlLen: number;
}

/** The part of the wasm namespace the widths are read from. */
export type InviteLinkWidthSource = Pick<
  Wasm,
  "inviteBlobIdLen" | "inviteBlobIdUrlLen" | "inviteLinkSecretLen" | "inviteLinkSecretUrlLen"
>;

/** Read the widths from the loaded wasm core. Throws when the core reports a
 *  width this client cannot represent, which only a broken build can do. */
export function inviteLinkWidths(wasm: InviteLinkWidthSource): InviteLinkWidths {
  const widths: InviteLinkWidths = {
    blobIdLen: wasm.inviteBlobIdLen(),
    blobIdUrlLen: wasm.inviteBlobIdUrlLen(),
    linkSecretLen: wasm.inviteLinkSecretLen(),
    linkSecretUrlLen: wasm.inviteLinkSecretUrlLen(),
  };
  for (const [name, value] of Object.entries(widths)) {
    if (!Number.isInteger(value) || value <= 0) {
      throw new Error(`invite link width ${name} is not a positive integer: ${String(value)}`);
    }
  }
  return widths;
}

/** The floor every inline sealed blob clears, whatever its kind. Each kind
 *  pins its own (higher or equal) floor; none is ever below this. */
export const INVITE_INLINE_FLOOR = 100;

/** The base64url alphabet, no padding. A slug or fragment with any other
 *  character is refused before it is decoded. */
const BASE64URL = /^[A-Za-z0-9_-]+$/;

/** The path letter of each link kind. */
export type InviteLinkKind = "i" | "c" | "g";

/** The default origin: the page's own, or the production host in tests. */
export function defaultInviteOrigin(): string {
  return typeof location !== "undefined" ? location.origin : "https://lakko.me";
}

/** What a slug turned out to be. */
export type ClassifiedInviteSlug =
  | { readonly kind: "pointer"; readonly blobId: Uint8Array }
  | { readonly kind: "inline"; readonly sealed: Uint8Array };

/** Classify a `/i/`, `/c/` or `/g/` slug. A pointer is exactly
 *  `widths.blobIdUrlLen` characters decoding to `widths.blobIdLen` bytes;
 *  an inline sealed blob decodes to at least `inlineMinLen` bytes. Throws on
 *  anything else, fail-closed, so an old 43-character id is refused rather
 *  than fetched. */
export function classifyInviteSlug(
  slug: string,
  widths: InviteLinkWidths,
  inlineMinLen: number,
  what: string,
): ClassifiedInviteSlug {
  if (!BASE64URL.test(slug)) {
    throw new Error(`${what} slug is not base64url`);
  }
  const bytes = base64UrlDecodeNoPad(slug);
  if (slug.length === widths.blobIdUrlLen && bytes.length === widths.blobIdLen) {
    return { kind: "pointer", blobId: bytes };
  }
  const floor = Math.max(inlineMinLen, INVITE_INLINE_FLOOR);
  if (bytes.length >= floor) {
    return { kind: "inline", sealed: bytes };
  }
  throw new Error(
    `${what} slug decoded to ${bytes.length} bytes: neither a ${widths.blobIdLen}-byte ` +
      `pointer nor a >= ${floor}-byte inline sealed link`,
  );
}

/** Refuse a ledger id of the wrong width before it reaches a URL or a
 *  request. */
export function requireBlobId(blobId: Uint8Array, widths: InviteLinkWidths): void {
  if (blobId.length !== widths.blobIdLen) {
    throw new Error(`blobId must be ${widths.blobIdLen} bytes, got ${blobId.length}`);
  }
}

/** The three parts of an `InviteLinkMint`, copied out of wasm memory. */
export interface InviteLinkMintParts {
  /** The sealed blob to upload, or to put inline in the path. */
  readonly sealed: Uint8Array;
  /** The 12-byte link secret — the URL fragment only, never uploaded. */
  readonly linkSecret: Uint8Array;
  /** The issuer's 32-byte Ed25519 key, uploaded in clear for the revoke. */
  readonly issuerPubkey: Uint8Array;
}

/** Copy the parts out of a wasm `InviteLinkMint` and free it. */
export function takeInviteLinkMint(mint: {
  readonly sealed: Uint8Array;
  readonly linkSecret: Uint8Array;
  readonly issuerPubkey: Uint8Array;
  free?(): void;
}): InviteLinkMintParts {
  const parts: InviteLinkMintParts = {
    sealed: mint.sealed,
    linkSecret: mint.linkSecret,
    issuerPubkey: mint.issuerPubkey,
  };
  try {
    mint.free?.();
  } catch {
    // Freeing is best-effort; the parts are already copied.
  }
  return parts;
}

/** Refuse a link secret of the wrong width before it reaches a URL. */
export function requireLinkSecret(secret: Uint8Array, widths: InviteLinkWidths): void {
  if (secret.length !== widths.linkSecretLen) {
    throw new Error(`link secret must be ${widths.linkSecretLen} bytes, got ${secret.length}`);
  }
}

/** What a link's fragment carries. There is no "none": a link without its
 *  secret (a chat app that stripped it, a hand-typed URL) is `damaged`, the
 *  same as a fragment that is not a link secret. */
export type InviteLinkFragment =
  | { readonly kind: "secret"; readonly secret: Uint8Array }
  | { readonly kind: "damaged" };

/** Read a link fragment (`location.hash`, with or without its `#`). Exactly
 *  `widths.linkSecretUrlLen` base64url characters decoding to
 *  `widths.linkSecretLen` bytes is a secret; anything else, an absent or
 *  empty fragment and the pre-amendment 43-character key included, is
 *  `damaged`. Never throws. */
export function parseInviteLinkFragment(
  fragment: string | null | undefined,
  widths: InviteLinkWidths,
): InviteLinkFragment {
  if (fragment === null || fragment === undefined) return { kind: "damaged" };
  const raw = fragment.startsWith("#") ? fragment.slice(1) : fragment;
  if (raw.length !== widths.linkSecretUrlLen || !BASE64URL.test(raw)) return { kind: "damaged" };
  let bytes: Uint8Array;
  try {
    bytes = base64UrlDecodeNoPad(raw);
  } catch {
    return { kind: "damaged" };
  }
  return bytes.length === widths.linkSecretLen ? { kind: "secret", secret: bytes } : { kind: "damaged" };
}

/** Build a link: `<origin>/<k>/<base64url(slugBytes)>#<base64url(secret)>`. */
function composeInviteLink(
  kind: InviteLinkKind,
  slugBytes: Uint8Array,
  secret: Uint8Array,
  widths: InviteLinkWidths,
  origin: string,
): string {
  requireLinkSecret(secret, widths);
  return `${origin}/${kind}/${base64UrlEncodeNoPad(slugBytes)}#${base64UrlEncodeNoPad(secret)}`;
}

/** Build the **pointer** link: `https://<host>/<k>/<8-char id>#<16-char secret>`. */
export function composePointerInviteLink(
  kind: InviteLinkKind,
  blobId: Uint8Array,
  secret: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = defaultInviteOrigin(),
): string {
  requireBlobId(blobId, widths);
  return composeInviteLink(kind, blobId, secret, widths, origin);
}

/** Build the **inline** link, used only when the upload failed on transport:
 *  `https://<host>/<k>/<base64url sealed blob>#<16-char secret>`. */
export function composeInlineInviteLink(
  kind: InviteLinkKind,
  sealed: Uint8Array,
  secret: Uint8Array,
  widths: InviteLinkWidths,
  inlineMinLen: number,
  origin: string = defaultInviteOrigin(),
): string {
  const floor = Math.max(inlineMinLen, INVITE_INLINE_FLOOR);
  if (sealed.length < floor) {
    throw new Error(`sealed link must be >= ${floor} bytes, got ${sealed.length}`);
  }
  return composeInviteLink(kind, sealed, secret, widths, origin);
}

/** Split a full link of `kind` into its slug and its fragment. `null` when the
 *  string is not a link of that kind at all. A link without a fragment splits
 *  with a `damaged` fragment. The revoke path reads the slug from here, so the
 *  fragment can never be mistaken for part of the id. */
export function splitInviteLink(
  link: string,
  kind: InviteLinkKind,
  widths: InviteLinkWidths,
): { readonly slug: string; readonly fragment: InviteLinkFragment } | null {
  const prefix = `/${kind}/`;
  const trimmed = link.trim();
  const hashAt = trimmed.indexOf("#");
  const beforeHash = hashAt < 0 ? trimmed : trimmed.slice(0, hashAt);
  const idx = beforeHash.lastIndexOf(prefix);
  if (idx < 0) return null;
  let slug = beforeHash.slice(idx + prefix.length);
  if (slug.endsWith("/")) slug = slug.slice(0, -1);
  if (slug.length === 0 || slug.includes("/") || slug.includes("?")) return null;
  const fragment = parseInviteLinkFragment(hashAt < 0 ? null : trimmed.slice(hashAt), widths);
  return { slug, fragment };
}

/** Append a link's fragment (`location.hash`, `#…`, bare, or empty) to an
 *  own-origin path, for the `?return=` round trip through sign-up / log-in: a
 *  link that comes back without its fragment is damaged. */
export function withLinkFragment(path: string, hash: string | null | undefined): string {
  if (hash === null || hash === undefined || hash.length === 0 || hash === "#") return path;
  return `${path}${hash.startsWith("#") ? hash : `#${hash}`}`;
}

/** `true` when `err` is the core's "this link is damaged" refusal (every
 *  `openLink` throws an `Error` carrying `inviteLinkError: "damaged"`). */
export function isInviteLinkDamagedError(err: unknown): boolean {
  return (
    typeof err === "object" &&
    err !== null &&
    (err as { inviteLinkError?: unknown }).inviteLinkError === "damaged"
  );
}

/** The outcome of resolving a link to its capability. */
export type OpenedInviteLink<T> =
  /** The secret opened the blob and the capability inside verified. The
   *  ledger id is `null` for an inline link (nothing to consume). */
  | { readonly kind: "opened"; readonly capability: T; readonly blobId: Uint8Array | null }
  /** No or wrong fragment, an unreadable slug, or a blob the secret does not
   *  open. Never retried without the secret. */
  | { readonly kind: "damaged" }
  /** The ledger answered 404: never issued, expired or revoked. */
  | { readonly kind: "not-found" };

/** Resolve a link: read the fragment, fetch the sealed blob by id (or take the
 *  inline one), and open it with the secret. A damaged fragment is refused
 *  BEFORE any fetch. Transport failures from `fetchSealed` (a 429, a network
 *  error) propagate unchanged, since they say nothing about the link. */
export async function openSealedInviteLink<T>(opts: {
  readonly slug: string;
  readonly fragment: string | null | undefined;
  readonly widths: InviteLinkWidths;
  readonly inlineMinLen: number;
  readonly what: string;
  readonly fetchSealed: (blobId: Uint8Array) => Promise<Uint8Array | null>;
  readonly open: (sealed: Uint8Array, secret: Uint8Array) => T;
}): Promise<OpenedInviteLink<T>> {
  const fragment = parseInviteLinkFragment(opts.fragment, opts.widths);
  if (fragment.kind === "damaged") return { kind: "damaged" };
  let classified: ClassifiedInviteSlug;
  try {
    classified = classifyInviteSlug(opts.slug, opts.widths, opts.inlineMinLen, opts.what);
  } catch {
    return { kind: "damaged" };
  }
  let sealed: Uint8Array;
  let blobId: Uint8Array | null = null;
  if (classified.kind === "pointer") {
    const fetched = await opts.fetchSealed(classified.blobId);
    if (fetched === null) return { kind: "not-found" };
    sealed = fetched;
    blobId = classified.blobId;
  } else {
    sealed = classified.sealed;
  }
  try {
    return { kind: "opened", capability: opts.open(sealed, fragment.secret), blobId };
  } catch {
    // Every open failure is the one refusal: the core folds a wrong secret, a
    // blob of another kind, a tampered or substituted blob and a capability
    // that does not verify into `inviteLinkError: "damaged"`.
    return { kind: "damaged" };
  }
}

// ----------------------------------------------------------------------------
// Copy (the same sentences ship on iOS and Android)
// ----------------------------------------------------------------------------

/** A link (any kind) whose fragment is missing or wrong, or whose sealed blob
 *  the fragment does not open: a truncated copy, a chat app that dropped the
 *  `#` part, a tampered link, or a blob a server swapped in under a live id. */
export const INVITE_LINK_DAMAGED = "This invite link is damaged. Ask for a new link.";

/** The discovery service answered 429 on fetch or consume: the link may be
 *  fine, this network asked too often. Never reported as an invalid link. */
export const INVITE_LINK_RATE_LIMITED =
  "Too many requests from this network right now. Try again in a minute.";

/** The button under {@link INVITE_LINK_RATE_LIMITED}; it re-runs the fetch. */
export const INVITE_LINK_TRY_AGAIN = "Try again";
