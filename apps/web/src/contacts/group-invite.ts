// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side URL helpers for the group join-ticket flow (ADR-0028, G-3).
 *
 * Sibling of `apps/web/src/calls/invite.ts` for the `GroupInviteCapability`
 * ecosystem. The `/g/<slug>` slug comes in two forms, discriminated purely by
 * the decoded byte length:
 *
 *   * **pointer** — the 32-byte content-addressed `blob_id` returned by
 *     `DiscoveryClient.uploadGroupInvite(...)`. The production default: short
 *     URL, the joiner fetches the capability from
 *     `/discovery/group-invite/<blob_id>`. This is the OQ1 indirection that
 *     adds the server-side anti-abuse ledger (consume dedup + soft max_uses).
 *   * **inline** — the full `GroupInviteCapability` wire bytes (>= 108 bytes).
 *     The censored-region fallback: no `/discovery/group-invite` round-trip on
 *     open, the capability rides inside the URL itself.
 *
 * The smallest legal inline capability is 1 (version) + 2 (gid_len) + 1 (min
 * gid) + 32 (issuer_pubkey) + 8 (expiry) + 64 (signature) = 108 bytes, well
 * above the 32-byte pointer, so a byte-length test cleanly discriminates.
 */
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";

/** Path prefix every group-invite URL starts with — kept in lock-step with
 *  `routes/paths.ts::groupJoinRoutePattern` (`/g/:slug`). The `/g/` shape
 *  contrasts with contact-invite `/i/` and call-invite `/c/` so the joiner-side
 *  landing dispatcher branches on the path segment without parsing the slug. */
export const GROUP_INVITE_URL_PREFIX = "/g/";

/** Length of a content-addressed `blob_id` from
 *  `services/discovery/src/group_invite.rs` — SHA-256, 32 bytes. */
export const GROUP_INVITE_BLOB_ID_LEN = 32;

/** Floor on the inline-bytes form. The real minimum is 108 bytes (a 1-byte
 *  group_id); we pin the discriminator floor at 100 — comfortably above the
 *  32-byte pointer and below any real capability, so the byte-length test never
 *  misclassifies. */
export const GROUP_INVITE_INLINE_MIN_LEN = 100;

/** Build the **pointer** URL form — the short
 *  `https://<host>/g/<blob_id_b64url>` shape backed by
 *  `/discovery/group-invite/<blob_id>` lookup. */
export function composePointerGroupInviteUrl(
  blobId: Uint8Array,
  origin: string = typeof location !== "undefined"
    ? location.origin
    : "https://lakko.me",
): string {
  if (blobId.length !== GROUP_INVITE_BLOB_ID_LEN) {
    throw new Error(
      `blobId must be ${GROUP_INVITE_BLOB_ID_LEN} bytes, got ${blobId.length}`,
    );
  }
  return `${origin}${GROUP_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(blobId)}`;
}

/** Build the **inline-bytes** URL form — the long shape that carries the
 *  capability inside the URL itself, no server round-trip on open. Reserved for
 *  the censored-region fallback where `/discovery/group-invite` is unreachable;
 *  the pointer form is the production default. */
export function composeInlineGroupInviteUrl(
  capabilityBytes: Uint8Array,
  origin: string = typeof location !== "undefined"
    ? location.origin
    : "https://lakko.me",
): string {
  if (capabilityBytes.length < GROUP_INVITE_INLINE_MIN_LEN) {
    throw new Error(
      `capabilityBytes must be >= ${GROUP_INVITE_INLINE_MIN_LEN} bytes, ` +
        `got ${capabilityBytes.length}`,
    );
  }
  return `${origin}${GROUP_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(capabilityBytes)}`;
}

/** Discriminated parse of a `/g/<slug>` slug:
 *
 *    * `kind: "pointer"` — fetch from
 *      `DiscoveryClient.fetchGroupInvite(blobId)` -> run
 *      `wasm.GroupInviteCapability.decodeAndVerify` on the returned bytes.
 *    * `kind: "inline"` — run `decodeAndVerify` directly on the bytes, no
 *      server round-trip.
 *
 *  The dispatch rule is a byte-length test: 32 => pointer (SHA-256), >= 100 =>
 *  inline. Anything else is invalid input. */
export type ParsedGroupInvite =
  | { readonly kind: "pointer"; readonly blobId: Uint8Array }
  | { readonly kind: "inline"; readonly capabilityBytes: Uint8Array };

export function parseGroupInviteSlug(slug: string): ParsedGroupInvite {
  const bytes = base64UrlDecodeNoPad(slug);
  if (bytes.length === GROUP_INVITE_BLOB_ID_LEN) {
    return { kind: "pointer", blobId: bytes };
  }
  if (bytes.length >= GROUP_INVITE_INLINE_MIN_LEN) {
    return { kind: "inline", capabilityBytes: bytes };
  }
  throw new Error(
    `group-invite slug decoded to ${bytes.length} bytes — neither a ` +
      `${GROUP_INVITE_BLOB_ID_LEN}-byte pointer nor a ` +
      `>= ${GROUP_INVITE_INLINE_MIN_LEN}-byte inline capability`,
  );
}
