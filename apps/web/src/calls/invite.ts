// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the link-based call-invite flow
 * (Phase 3c3-url-helpers).
 *
 * Sibling of `apps/web/src/contacts/invite.ts` for the
 * `CallInviteCapability` ecosystem. Two surfaces:
 *
 *   * URL plumbing — `composePointerCallInviteUrl(...)` /
 *     `composeInlineCallInviteUrl(...)` +
 *     `parseCallInviteSlug(...)`. The pointer form is the
 *     production default — owner uploads via
 *     `DiscoveryClient.uploadCallInvite(...)` and substitutes
 *     the returned `blob_id` into `/c/<slug>`. Inline form
 *     is reserved as the censored-region fallback (no
 *     `/discovery/call-invite` round-trip needed).
 *   * Landing-page rendering — `describeMaxUses(...)` and
 *     `describeExpiry(...)` put a decoded capability's fields
 *     into words.
 *
 * The form-side enums that used to live here (`ExpiryPreset`,
 * `MaxUsesPreset`, `CallMediaPreset` and their translators) are
 * gone with the form that asked those four questions
 * (`docs/ux/call-links.md` sections 1 and 6). The values a link
 * carries are now fixed in `call-invite-values.ts`, and the
 * composer takes wire-level values directly.
 *
 * URL prefix is `/c/` (mirrors contact invite's `/i/`), so the
 * server-side route table and the joiner-side landing-page
 * dispatcher can branch on a one-character lookahead.
 */
import {
  classifyInviteSlug,
  composeInlineInviteLink,
  composePointerInviteLink,
  type ClassifiedInviteSlug,
  type InviteLinkWidths,
} from "../contacts/invite-link";

/** Path prefix every call-invite URL starts with — kept in
 *  lock-step with `routes/paths.ts::callInviteRoutePattern`
 *  (will land alongside the `/c/<slug>` UI route in the next
 *  sprint). The `/c/` shape contrasts with contact invite's
 *  `/i/` so the joiner-side landing-page dispatcher can branch
 *  on the path segment without parsing the slug. */
export const CALL_INVITE_URL_PREFIX = "/c/";

/** Length of a `call_id`, in bytes — the width
 *  `CallInviteCapability`, the call-admission JWT and the MLS
 *  exporter context all carry. */
export const CALL_ID_LEN = 16;

/** Hard cap on the human-visible label, in bytes (UTF-8).
 *  Mirrors `lakko_contacts::invite::MAX_LABEL_LEN` — the
 *  contact-invite codec re-uses its 64-byte ceiling, so the
 *  call-invite ledger does too. Duplicated here so the Web UI
 *  can validate the form before crossing the WASM boundary. */
export const CALL_INVITE_LABEL_MAX_LEN = 64;

/** Lower bound on the inline-bytes form. The 3c3 wire format
 *  with a 256-byte one-time KP tops out around 500 bytes; with
 *  a 4 KiB KP around 4308 bytes. The smallest legal payload
 *  (256-byte KP + empty label) is well above the 6-byte ledger
 *  id, so a slug-
 *  byte-length test cleanly discriminates inline vs pointer
 *  URLs. We pin the floor at 400 to leave headroom for KP
 *  formats that might shrink slightly. */
export const CALL_INVITE_INLINE_MIN_LEN = 400;

/** Builder for the **pointer** link — `https://<host>/c/<8-char id>#<secret>`,
 *  backed by the `/discovery/call-invite/<id>` lookup of the sealed blob. */
export function composePointerCallInviteUrl(
  blobId: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composePointerInviteLink("c", blobId, linkSecret, widths, origin);
}

/** Builder for the **inline** link — `https://<host>/c/<base64url sealed>#<secret>`,
 *  used only when the upload failed on transport. */
export function composeInlineCallInviteUrl(
  sealed: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composeInlineInviteLink("c", sealed, linkSecret, widths, CALL_INVITE_INLINE_MIN_LEN, origin);
}

/** Discriminated parse of a URL slug. Returns the wire-form +
 *  the decoded bytes so the caller can dispatch:
 *
 *    * `kind: "pointer"` — fetch the sealed blob from
 *      `DiscoveryClient.fetchCallInvite(blobId)`.
 *    * `kind: "inline"` — the slug is the sealed blob.
 *
 *  Either way the blob is opened with
 *  `CallInviteCapability.openLink(sealed, secret)`.
 *
 *  The dispatch rule (`contacts/invite-link.ts`): 8 characters
 *  decoding to 6 bytes ⇒ pointer, ≥ 400 bytes ⇒ inline (3c3
 *  wire-format minimum with a 256-byte KP). Anything else, the
 *  pre-amendment 43-character id included, is refused. */
export type ParsedCallInvite = ClassifiedInviteSlug;

export function parseCallInviteSlug(slug: string, widths: InviteLinkWidths): ParsedCallInvite {
  return classifyInviteSlug(slug, widths, CALL_INVITE_INLINE_MIN_LEN, "call-invite");
}

/** Convert a wire `max_uses` u32 back into a human-visible
 *  rendering for the landing page. Same recipe as the
 *  contact-invite helper — kept duplicated to keep the two
 *  modules independent. */
export function describeMaxUses(maxUses: number): string {
  if (maxUses === 0) return "single-use";
  if (maxUses === 0xff_ff_ff_ff) return "unlimited";
  return `up to ${maxUses} uses`;
}

/** Render an `expiry_ms` (UNIX millis) for the landing page —
 *  `"never expires"` for the `CALL_NEVER_EXPIRES` sentinel,
 *  otherwise a relative "in 6 days" / "expired 3 hours ago"
 *  string. */
export function describeExpiry(expiryMs: bigint, nowMs: number): string {
  if (expiryMs === 0n) return "never expires";
  const exp = Number(expiryMs);
  const deltaMs = exp - nowMs;
  const futureLabel = deltaMs >= 0 ? "in" : "expired";
  const absMs = Math.abs(deltaMs);
  const day = 86_400_000;
  const hour = 3_600_000;
  const minute = 60_000;
  if (absMs >= day) {
    const days = Math.round(absMs / day);
    return `${futureLabel} ${days} day${days === 1 ? "" : "s"}${deltaMs >= 0 ? "" : " ago"}`;
  }
  if (absMs >= hour) {
    const hours = Math.round(absMs / hour);
    return `${futureLabel} ${hours} hour${hours === 1 ? "" : "s"}${deltaMs >= 0 ? "" : " ago"}`;
  }
  const mins = Math.max(1, Math.round(absMs / minute));
  return `${futureLabel} ${mins} minute${mins === 1 ? "" : "s"}${deltaMs >= 0 ? "" : " ago"}`;
}
