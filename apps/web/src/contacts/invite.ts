// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the invite-link flow (Phase 1.18c).
 *
 * Two surfaces:
 *
 *   * URL plumbing — `composePointerInviteUrl(...)` +
 *     `parseInviteSlug(...)`. The link is the short ledger pointer
 *     (`DiscoveryClient` upload/fetch); the inline form that carried
 *     the capability in the URL was removed on 2026-10-19.
 *   * Form-side enums — `ExpiryPreset` and `MaxUsesPreset` —
 *     translate UI dropdown choices into the wire-level
 *     `InviteCapability.compose` arguments. Putting them here
 *     instead of inside the route component keeps the route file
 *     focused on rendering and gives unit-test coverage for the
 *     defaulting logic without spinning up Solid.
 */
import { t } from "../l10n";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  classifyInviteSlug,
  requireBlobId,
  type ClassifiedInviteSlug,
  type InviteLinkWidths,
} from "./invite-link";

/** Path prefix every invite URL starts with — kept in lock-step
 *  with `routes/paths.ts::inviteRoutePattern`. */
export const INVITE_URL_PREFIX = "/i/";

/** Hard cap on the human-visible label, in bytes (UTF-8). Mirrors
 *  `lakko_contacts::invite::MAX_LABEL_LEN`; duplicated here so the
 *  Web UI can validate the form before crossing the WASM boundary
 *  (cleaner UX than waiting for a `JsError` round-trip). The two
 *  values stay in sync — a sprint that bumps the Rust constant
 *  must also bump this one. */
export const INVITE_LABEL_MAX_LEN = 64;

/** Builder for the URL — the short `https://<host>/i/<blob_id_b64url>`
 *  pointer backed by the `/discovery/invite/<blob_id>` lookup. */
export function composePointerInviteUrl(
  blobId: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = typeof location !== "undefined" ? location.origin : "https://lakko.me",
): string {
  requireBlobId(blobId, widths);
  return `${origin}${INVITE_URL_PREFIX}${base64UrlEncodeNoPad(blobId)}`;
}

/** Parse a URL slug into its ledger id (`invite-link.ts`: 8 characters
 *  decoding to 6 bytes). Anything else, the pre-amendment 43-character id
 *  and an inline sealed blob included, is refused. */
export type ParsedInvite = ClassifiedInviteSlug;

export function parseInviteSlug(slug: string, widths: InviteLinkWidths): ParsedInvite {
  return classifyInviteSlug(slug, widths, "invite");
}

/** Discrete UI choices the "Generate invite link" form exposes
 *  for the `expiry_ms` field. Recipe-driven instead of free-form
 *  to keep the UX simple and to put the stalking-risk warning
 *  for `"never"` somewhere obvious. */
export type ExpiryPreset = "1d" | "7d" | "30d" | "never";

/** Translate an [`ExpiryPreset`] into the wire-level UNIX-millis
 *  argument `InviteCapability.compose` expects. `"never"` resolves
 *  to `0n` — the `NEVER_EXPIRES` sentinel — and the UI MUST surface
 *  a warning before allowing it (ADR-0005 §13.3). */
export function expiryPresetToMs(preset: ExpiryPreset, nowMs: number): bigint {
  switch (preset) {
    case "1d":
      return BigInt(nowMs + 1 * 86_400_000);
    case "7d":
      return BigInt(nowMs + 7 * 86_400_000);
    case "30d":
      return BigInt(nowMs + 30 * 86_400_000);
    case "never":
      return 0n;
  }
}

/** Discrete UI choices for `max_uses`. */
export type MaxUsesPreset = "single" | "ten" | "unlimited";

/** Translate a [`MaxUsesPreset`] into the `max_uses` u32 the wire
 *  format carries. `"single"` resolves to `0` — the `SINGLE_USE`
 *  sentinel. `"unlimited"` saturates at `u32::MAX`. */
export function maxUsesPresetToValue(preset: MaxUsesPreset): number {
  switch (preset) {
    case "single":
      return 0;
    case "ten":
      return 10;
    case "unlimited":
      return 0xff_ff_ff_ff;
  }
}

/** Convert a wire `max_uses` u32 back into a human-visible
 *  rendering for the landing page. */
export function describeMaxUses(maxUses: number): string {
  if (maxUses === 0) return t("invite.uses.single");
  if (maxUses === 0xff_ff_ff_ff) return t("invite.uses.unlimited");
  return t("invite.uses.up_to", { count: maxUses });
}

/** Render an `expiry_ms` (UNIX millis) for the landing page —
 *  `"never expires"` for the `NEVER_EXPIRES` sentinel, otherwise
 *  a relative "in 6 days" / "expired 3 hours ago" string. */
export function describeExpiry(expiryMs: bigint, nowMs: number): string {
  if (expiryMs === 0n) return t("invite.expiry.never");
  const exp = Number(expiryMs);
  const deltaMs = exp - nowMs;
  const tense = deltaMs >= 0 ? "in" : "ago";
  const absMs = Math.abs(deltaMs);
  const day = 86_400_000;
  const hour = 3_600_000;
  const minute = 60_000;
  if (absMs >= day) {
    return t(`invite.expiry.${tense}_days`, { count: Math.round(absMs / day) });
  }
  if (absMs >= hour) {
    return t(`invite.expiry.${tense}_hours`, { count: Math.round(absMs / hour) });
  }
  return t(`invite.expiry.${tense}_minutes`, { count: Math.max(1, Math.round(absMs / minute)) });
}
