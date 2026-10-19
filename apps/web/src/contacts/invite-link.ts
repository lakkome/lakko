// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shape rules every invite link shares (`/i/`, `/c/`, `/g/`), after the
 * short-link amendment of ADR-0028 (2026-09-27, flag day).
 *
 *   * A **pointer** slug is the server-assigned ledger id: 6 random bytes,
 *     8 base64url characters, no padding.
 *   * Anything else is refused: the pre-amendment 32-byte / 43-character
 *     content-addressed id, and the inline form that carried the whole sealed
 *     blob in the path (removed 2026-10-19: every link is a ledger pointer,
 *     and a failed upload mints no link), which opens as a dead link.
 *   * Every link carries the 12-byte link secret as its fragment, exactly 16
 *     base64url characters (sealed-link amendment of ADR-0028, 2026-09-28):
 *     the capability is encrypted under it, so a link whose fragment is
 *     missing or malformed opens nothing and is DAMAGED, on every kind.
 *
 * The widths come from the wasm core (`inviteBlobIdLen` and friends) so the
 * client cannot drift from the Rust codec; every helper here takes them as an
 * argument, read once from the loaded wasm namespace via
 * {@link inviteLinkWidths}. The copy for the two link-level failures the
 * landing pages show lives here too, in a plain module `vitest` can load.
 */
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import { t } from "../l10n";
import type { Wasm } from "../wasm";

/** The four widths a link is checked against. */
export interface InviteLinkWidths {
  /** Ledger id, bytes (6). */
  readonly blobIdLen: number;
  /** Ledger id in a URL, base64url characters (8). */
  readonly blobIdUrlLen: number;
  /** Link secret, bytes (12), shared by `/i/`, `/c/` and `/g/`. */
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

/** The base64url alphabet, no padding. A slug or fragment with any other
 *  character is refused before it is decoded. */
const BASE64URL = /^[A-Za-z0-9_-]+$/;

/** What a slug turned out to be: always a ledger pointer. */
export interface ClassifiedInviteSlug {
  readonly kind: "pointer";
  readonly blobId: Uint8Array;
}

/** Classify a `/i/`, `/c/` or `/g/` slug. A pointer is exactly
 *  `widths.blobIdUrlLen` characters decoding to `widths.blobIdLen` bytes.
 *  Throws on anything else, fail-closed, before any request: an old
 *  43-character id and an inline sealed blob (the removed form) are refused,
 *  and every landing page shows its dead-link state for them. */
export function classifyInviteSlug(
  slug: string,
  widths: InviteLinkWidths,
  what: string,
): ClassifiedInviteSlug {
  if (!BASE64URL.test(slug)) {
    throw new Error(`${what} slug is not base64url`);
  }
  if (slug.length !== widths.blobIdUrlLen) {
    throw new Error(`${what} slug is ${slug.length} characters, not a ${widths.blobIdUrlLen}-character ledger id`);
  }
  const bytes = base64UrlDecodeNoPad(slug);
  if (bytes.length !== widths.blobIdLen) {
    throw new Error(`${what} slug decoded to ${bytes.length} bytes, not a ${widths.blobIdLen}-byte ledger id`);
  }
  return { kind: "pointer", blobId: bytes };
}

/** Refuse a ledger id of the wrong width before it reaches a URL or a
 *  request. */
export function requireBlobId(blobId: Uint8Array, widths: InviteLinkWidths): void {
  if (blobId.length !== widths.blobIdLen) {
    throw new Error(`blobId must be ${widths.blobIdLen} bytes, got ${blobId.length}`);
  }
}

/** What a link's fragment carries: the link secret, or nothing that opens
 *  the link. A missing fragment is `damaged` too since the sealed-link
 *  amendment (a chat app that stripped it, a hand-typed URL): without the
 *  secret the sealed capability opens nothing. */
export type InviteLinkFragment =
  | { readonly kind: "secret"; readonly secret: Uint8Array }
  | { readonly kind: "damaged" };

/** Read a link fragment (`location.hash`, with or without its `#`). Exactly
 *  `widths.linkSecretUrlLen` base64url characters decoding to
 *  `widths.linkSecretLen` bytes is the secret; anything else, absent or empty
 *  included, is `damaged`. Never throws. */
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

/** Append the link secret to a link as its **fragment**:
 *  `<origin>/<k>/<slug>#<secret, 16 base64url characters>`. The fragment is
 *  the one part of a URL a browser never sends, so the ledger and every access
 *  log see only the sealed blob. */
export function appendInviteLinkSecret(
  url: string,
  secret: Uint8Array,
  widths: InviteLinkWidths,
): string {
  if (secret.length !== widths.linkSecretLen) {
    throw new Error(`link secret must be ${widths.linkSecretLen} bytes, got ${secret.length}`);
  }
  if (url.includes("#")) {
    throw new Error("url already carries a fragment");
  }
  return `${url}#${base64UrlEncodeNoPad(secret)}`;
}

/** Split a full link of the kind `prefix` (`/i/`, `/c/`, `/g/`) into its slug
 *  and its fragment. `null` when the string is not such a link at all. The
 *  revoke path reads the slug from here, so the fragment can never be
 *  mistaken for part of the id. */
export function splitInviteLink(
  link: string,
  prefix: string,
  widths: InviteLinkWidths,
): { readonly slug: string; readonly fragment: InviteLinkFragment } | null {
  const idx = link.lastIndexOf(prefix);
  if (idx < 0) return null;
  const rest = link.slice(idx + prefix.length);
  const hashAt = rest.indexOf("#");
  const slug = hashAt < 0 ? rest : rest.slice(0, hashAt);
  if (slug.length === 0) return null;
  const fragment = parseInviteLinkFragment(hashAt < 0 ? null : rest.slice(hashAt), widths);
  return { slug, fragment };
}

/** A link that does not open: missing or malformed fragment, wrong secret,
 *  another kind's blob, truncated, tampered, substituted, or a capability
 *  that does not verify inside. The one refusal, shown as
 *  {@link INVITE_LINK_DAMAGED}. */
export class InviteLinkDamagedError extends Error {
  override name = "InviteLinkDamagedError";
  constructor() {
    super(INVITE_LINK_DAMAGED());
  }
}

/** `true` when `err` is the damaged-link refusal: ours, or the wasm core's
 *  (`Error` carrying `inviteLinkError: "damaged"`), possibly wrapped. */
export function isInviteLinkDamaged(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (current instanceof InviteLinkDamagedError) return true;
    if (typeof current !== "object") return false;
    const record = current as {
      inviteLinkError?: unknown;
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    if (record.inviteLinkError === "damaged") return true;
    current = record.cause ?? record.variant?.cause;
  }
  return false;
}

/** Open a sealed blob with the fragment's secret through the kind's
 *  `openLink`. Any failure is {@link InviteLinkDamagedError}; a damaged
 *  fragment never calls the opener. */
export function openSealedInvite<T>(
  open: (sealed: Uint8Array, secret: Uint8Array) => T,
  sealed: Uint8Array,
  fragment: InviteLinkFragment,
): T {
  if (fragment.kind !== "secret") throw new InviteLinkDamagedError();
  try {
    return open(sealed, fragment.secret);
  } catch {
    throw new InviteLinkDamagedError();
  }
}

/** What {@link openInviteLinkFromUrl} found. `not-found` is the ledger's 404
 *  (never issued, expired or revoked). */
export type OpenedInviteLink<T> =
  | { readonly kind: "opened"; readonly capability: T; readonly blobId: Uint8Array }
  | { readonly kind: "not-found" };

/**
 * The joiner-side open of any link kind: classify the slug (throws on a
 * malformed one, before any request), refuse a missing or malformed fragment
 * as damaged BEFORE any request, fetch the sealed blob from the ledger, and
 * open it with the fragment's secret.
 * Throws {@link InviteLinkDamagedError} for every damaged case; a ledger
 * error (429, network) propagates unchanged.
 */
export async function openInviteLinkFromUrl<T>(args: {
  readonly slug: string;
  readonly hash: string | null | undefined;
  readonly widths: InviteLinkWidths;
  readonly what: string;
  readonly fetchPointer: (blobId: Uint8Array) => Promise<Uint8Array | null>;
  readonly open: (sealed: Uint8Array, secret: Uint8Array) => T;
}): Promise<OpenedInviteLink<T>> {
  const parsed = classifyInviteSlug(args.slug, args.widths, args.what);
  const fragment = parseInviteLinkFragment(args.hash, args.widths);
  if (fragment.kind !== "secret") throw new InviteLinkDamagedError();
  const sealed = await args.fetchPointer(parsed.blobId);
  if (sealed === null) return { kind: "not-found" };
  return { kind: "opened", capability: openSealedInvite(args.open, sealed, fragment), blobId: parsed.blobId };
}

/** The shareable link for a freshly minted invite: the ledger id the upload
 *  returned, with the secret as the fragment. There is no link without a
 *  ledger id: a failed upload mints nothing. */
export function composeSealedInviteLink(args: {
  readonly origin: string;
  readonly prefix: string;
  readonly blobId: Uint8Array;
  readonly secret: Uint8Array;
  readonly widths: InviteLinkWidths;
}): string {
  requireBlobId(args.blobId, args.widths);
  const slug = base64UrlEncodeNoPad(args.blobId);
  return appendInviteLinkSecret(`${args.origin}${args.prefix}${slug}`, args.secret, args.widths);
}

// ----------------------------------------------------------------------------
// Copy (catalog keys, ADR-0061; the same keys ship on iOS and Android)
// ----------------------------------------------------------------------------

/** A link that does not open under its fragment (every kind): a missing or
 *  truncated fragment, a tampered link, or a blob a server swapped in under a
 *  live id. Refused before anything else is shown. */
export const INVITE_LINK_DAMAGED = (): string => t("invite.link.damaged");

/** The discovery service answered 429 on fetch or consume: the link may be
 *  fine, this network asked too often. Never reported as an invalid link. */
export const INVITE_LINK_RATE_LIMITED = (): string => t("invite.link.rate_limited");

/** The button under {@link INVITE_LINK_RATE_LIMITED}; it re-runs the fetch. */
export const INVITE_LINK_TRY_AGAIN = (): string => t("common.try_again");
