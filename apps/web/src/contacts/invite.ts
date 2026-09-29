// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the invite-link flow (Phase 1.18c).
 *
 * Two surfaces:
 *
 *   * URL plumbing — `composePointerInviteUrl(...)` /
 *     `composeInlineInviteUrl(...)` + `parseInviteSlug(...)`, over the
 *     shared sealed-link rules of `invite-link.ts`: every link carries the
 *     12-byte secret in its fragment and the capability is sealed under it.
 *   * Form-side enums — `ExpiryPreset` and `MaxUsesPreset` —
 *     translate UI dropdown choices into the wire-level
 *     `InviteCapability.compose` arguments. Putting them here
 *     instead of inside the route component keeps the route file
 *     focused on rendering and gives unit-test coverage for the
 *     defaulting logic without spinning up Solid.
 */
import {
  classifyInviteSlug,
  composeInlineInviteLink,
  composePointerInviteLink,
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

/** Floor on the inline form: a sealed `/i/` blob is the whole capability
 *  (>= 160 bytes with an empty label) plus the 30-byte seal envelope, far
 *  above the 6-byte ledger id, so the byte length discriminates inline from
 *  pointer. */
export const INVITE_INLINE_MIN_LEN = 160;

/** Build the **pointer** link: `https://<host>/i/<8-char id>#<secret>`. */
export function composePointerInviteUrl(
  blobId: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composePointerInviteLink("i", blobId, linkSecret, widths, origin);
}

/** Build the **inline** link, the fallback when the upload failed on
 *  transport: `https://<host>/i/<base64url sealed>#<secret>`. */
export function composeInlineInviteUrl(
  sealed: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composeInlineInviteLink("i", sealed, linkSecret, widths, INVITE_INLINE_MIN_LEN, origin);
}

/** Discriminated parse of an `/i/` slug: `pointer` (fetch the sealed blob
 *  from `DiscoveryClient.fetchInvite`) or `inline` (the sealed blob itself).
 *  Either is opened with `InviteCapability.openLink(sealed, secret)`.
 *  Anything else, the pre-amendment 43-character id included, is refused. */
export type ParsedInvite = ClassifiedInviteSlug;

export function parseInviteSlug(slug: string, widths: InviteLinkWidths): ParsedInvite {
  return classifyInviteSlug(slug, widths, INVITE_INLINE_MIN_LEN, "invite");
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
  if (maxUses === 0) return "single-use";
  if (maxUses === 0xff_ff_ff_ff) return "unlimited";
  return `up to ${maxUses} uses`;
}

/** Render an `expiry_ms` (UNIX millis) for the landing page —
 *  `"never expires"` for the `NEVER_EXPIRES` sentinel, otherwise
 *  a relative "in 6 days" / "expired 3 hours ago" string. */
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
