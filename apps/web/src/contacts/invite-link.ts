// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The shape rules every invite link shares (`/i/`, `/c/`, `/g/`), after the
 * short-link amendment of ADR-0028 (2026-09-27, flag day).
 *
 *   * A **pointer** slug is the server-assigned ledger id: 6 random bytes,
 *     8 base64url characters, no padding.
 *   * An **inline** slug is the whole capability, never shorter than the
 *     per-kind floor (always >= 100 bytes).
 *   * Anything else is refused, including the pre-amendment 32-byte /
 *     43-character content-addressed id.
 *   * A `/g/` link may carry the 12-byte link secret as its fragment, exactly
 *     16 base64url characters. Any other non-empty fragment is damaged, never
 *     "no fragment".
 *
 * The widths come from the wasm core (`inviteBlobIdLen` and friends) so the
 * client cannot drift from the Rust codec; every helper here takes them as an
 * argument, read once from the loaded wasm namespace via
 * {@link inviteLinkWidths}. The copy for the two link-level failures the
 * landing pages show lives here too, in a plain module `vitest` can load.
 */
import { base64UrlDecodeNoPad } from "../auth/account-id";
import type { Wasm } from "../wasm";

/** The four widths a link is checked against. */
export interface InviteLinkWidths {
  /** Ledger id, bytes (6). */
  readonly blobIdLen: number;
  /** Ledger id in a URL, base64url characters (8). */
  readonly blobIdUrlLen: number;
  /** `/g/` link secret, bytes (12). */
  readonly linkSecretLen: number;
  /** `/g/` link secret in a URL fragment, base64url characters (16). */
  readonly linkSecretUrlLen: number;
}

/** The part of the wasm namespace the widths are read from. */
export type InviteLinkWidthSource = Pick<
  Wasm,
  "inviteBlobIdLen" | "inviteBlobIdUrlLen" | "groupInviteLinkSecretLen" | "groupInviteLinkSecretUrlLen"
>;

/** Read the widths from the loaded wasm core. Throws when the core reports a
 *  width this client cannot represent, which only a broken build can do. */
export function inviteLinkWidths(wasm: InviteLinkWidthSource): InviteLinkWidths {
  const widths: InviteLinkWidths = {
    blobIdLen: wasm.inviteBlobIdLen(),
    blobIdUrlLen: wasm.inviteBlobIdUrlLen(),
    linkSecretLen: wasm.groupInviteLinkSecretLen(),
    linkSecretUrlLen: wasm.groupInviteLinkSecretUrlLen(),
  };
  for (const [name, value] of Object.entries(widths)) {
    if (!Number.isInteger(value) || value <= 0) {
      throw new Error(`invite link width ${name} is not a positive integer: ${String(value)}`);
    }
  }
  return widths;
}

/** The floor every inline capability clears, whatever its kind. Each kind
 *  pins its own (higher or equal) floor; none is ever below this. */
export const INVITE_INLINE_FLOOR = 100;

/** The base64url alphabet, no padding. A slug or fragment with any other
 *  character is refused before it is decoded. */
const BASE64URL = /^[A-Za-z0-9_-]+$/;

/** What a slug turned out to be. */
export type ClassifiedInviteSlug =
  | { readonly kind: "pointer"; readonly blobId: Uint8Array }
  | { readonly kind: "inline"; readonly capabilityBytes: Uint8Array };

/** Classify a `/i/`, `/c/` or `/g/` slug. A pointer is exactly
 *  `widths.blobIdUrlLen` characters decoding to `widths.blobIdLen` bytes;
 *  an inline capability decodes to at least `inlineMinLen` bytes. Throws on
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
    return { kind: "inline", capabilityBytes: bytes };
  }
  throw new Error(
    `${what} slug decoded to ${bytes.length} bytes: neither a ${widths.blobIdLen}-byte ` +
      `pointer nor a >= ${floor}-byte inline capability`,
  );
}

/** Refuse a ledger id of the wrong width before it reaches a URL or a
 *  request. */
export function requireBlobId(blobId: Uint8Array, widths: InviteLinkWidths): void {
  if (blobId.length !== widths.blobIdLen) {
    throw new Error(`blobId must be ${widths.blobIdLen} bytes, got ${blobId.length}`);
  }
}

/** What a `/g/` link's fragment carries. `none` is a link without one (a chat
 *  app that stripped it, a hand-typed URL); `damaged` is a fragment that is
 *  present but is not a link secret, which the page refuses. */
export type GroupInviteFragment =
  | { readonly kind: "none" }
  | { readonly kind: "secret"; readonly secret: Uint8Array }
  | { readonly kind: "damaged" };

/** Read a `/g/` fragment (`location.hash`, with or without its `#`). An
 *  absent or empty fragment is `none`; exactly `widths.linkSecretUrlLen`
 *  base64url characters decoding to `widths.linkSecretLen` bytes is a secret;
 *  anything else, the pre-amendment 43-character key included, is
 *  `damaged`. Never throws. */
export function parseGroupInviteFragment(
  fragment: string | null | undefined,
  widths: InviteLinkWidths,
): GroupInviteFragment {
  if (fragment === null || fragment === undefined) return { kind: "none" };
  const raw = fragment.startsWith("#") ? fragment.slice(1) : fragment;
  if (raw.length === 0) return { kind: "none" };
  if (raw.length !== widths.linkSecretUrlLen || !BASE64URL.test(raw)) return { kind: "damaged" };
  let bytes: Uint8Array;
  try {
    bytes = base64UrlDecodeNoPad(raw);
  } catch {
    return { kind: "damaged" };
  }
  return bytes.length === widths.linkSecretLen ? { kind: "secret", secret: bytes } : { kind: "damaged" };
}

// ----------------------------------------------------------------------------
// Copy (the same sentences ship on iOS and Android)
// ----------------------------------------------------------------------------

/** A `/g/` link whose fragment is present but does not open the preview: a
 *  truncated copy, a tampered link, or a capability a server swapped in under
 *  a live id. Refused rather than shown without a preview. */
export const INVITE_LINK_DAMAGED = "This invite link is damaged. Ask for a new link.";

/** The discovery service answered 429 on fetch or consume: the link may be
 *  fine, this network asked too often. Never reported as an invalid link. */
export const INVITE_LINK_RATE_LIMITED =
  "Too many requests from this network right now. Try again in a minute.";

/** The button under {@link INVITE_LINK_RATE_LIMITED}; it re-runs the fetch. */
export const INVITE_LINK_TRY_AGAIN = "Try again";
