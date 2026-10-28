// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the link-based call-invite flow
 * (Phase 3c3-url-helpers).
 *
 * Sibling of `apps/web/src/contacts/invite.ts` for the
 * `CallInviteCapability` ecosystem. Two surfaces:
 *
 *   * URL plumbing — `composePointerCallInviteUrl(...)` +
 *     `parseCallInviteSlug(...)`. The owner uploads via
 *     `DiscoveryClient.uploadCallInvite(...)` and substitutes
 *     the returned `blob_id` into `/c/<slug>`. The inline form
 *     (the capability in the URL) was removed on 2026-10-19: a
 *     failed upload mints no link.
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
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { t } from "../l10n";
import {
  classifyInviteSlug,
  requireBlobId,
  type ClassifiedInviteSlug,
  type InviteLinkWidths,
} from "../contacts/invite-link";
import { publicOrigin } from "../platform";

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

/** Builder for the URL — the short `https://<host>/c/<blob_id_b64url>`
 *  pointer backed by the `/discovery/call-invite/<blob_id>` lookup, after
 *  a `uploadCallInvite(...)` round-trip. */
export function composePointerCallInviteUrl(
  blobId: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = publicOrigin(),
): string {
  requireBlobId(blobId, widths);
  return `${origin}${CALL_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(blobId)}`;
}

/** Parse a URL slug into its ledger id (`contacts/invite-link.ts`: 8
 *  characters decoding to 6 bytes). Anything else, the pre-amendment
 *  43-character id and an inline sealed blob included, is refused. */
export type ParsedCallInvite = ClassifiedInviteSlug;

export function parseCallInviteSlug(slug: string, widths: InviteLinkWidths): ParsedCallInvite {
  return classifyInviteSlug(slug, widths, "call-invite");
}

/** Convert a wire `max_uses` u32 back into a human-visible
 *  rendering for the landing page. Same recipe as the
 *  contact-invite helper — kept duplicated to keep the two
 *  modules independent. */
export function describeMaxUses(maxUses: number): string {
  if (maxUses === 0) return t("invite.uses.single");
  if (maxUses === 0xff_ff_ff_ff) return t("invite.uses.unlimited");
  return t("invite.uses.up_to", { count: maxUses });
}

/** Render an `expiry_ms` (UNIX millis) for the landing page —
 *  `"never expires"` for the `CALL_NEVER_EXPIRES` sentinel,
 *  otherwise a relative "in 6 days" / "expired 3 hours ago"
 *  string. */
export function describeExpiry(expiryMs: bigint, nowMs: number): string {
  if (expiryMs === 0n) return t("invite.expiry.never");
  const exp = Number(expiryMs);
  const deltaMs = exp - nowMs;
  const future = deltaMs >= 0;
  const absMs = Math.abs(deltaMs);
  const day = 86_400_000;
  const hour = 3_600_000;
  const minute = 60_000;
  if (absMs >= day) {
    const count = Math.round(absMs / day);
    return future ? t("invite.expiry.in_days", { count }) : t("invite.expiry.ago_days", { count });
  }
  if (absMs >= hour) {
    const count = Math.round(absMs / hour);
    return future
      ? t("invite.expiry.in_hours", { count })
      : t("invite.expiry.ago_hours", { count });
  }
  const count = Math.max(1, Math.round(absMs / minute));
  return future
    ? t("invite.expiry.in_minutes", { count })
    : t("invite.expiry.ago_minutes", { count });
}
