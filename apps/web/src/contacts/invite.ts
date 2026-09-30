// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the invite-link flow (Phase 1.18c).
 *
 * Two surfaces:
 *
 *   * URL plumbing — `composeInviteUrl(...)` + `parseInviteSlug(...)`.
 *     The capability bytes are base64url-no-pad encoded into the URL
 *     itself (the V1 inline form — see ROADMAP 1.18a for the
 *     trade-off). The short-pointer + server-blob form landed in
 *     1.18d1b (`composePointerInviteUrl` below + `DiscoveryClient`
 *     upload/fetch); inline is now the back-compat fallback.
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

/** Lower bound on the inline-bytes form. The 1.18a wire format
 *  is `1 + 32 + 16 + 32 + 8 + 4 + 1 + 2 + label(0..64) + 64 =
 *  160..224 bytes`; an empty label (the smallest legal payload)
 *  is 160 bytes. Far above the 6-byte ledger id (ADR-0028
 *  short-link amendment), so a slug-byte-length test cleanly
 *  discriminates inline vs pointer URLs. */
export const INVITE_INLINE_MIN_LEN = 160;

/** Auto-pick the URL form by argument shape — pointer
 *  (`blob_id`, 6 bytes) when the caller has uploaded the
 *  capability and wants the short form, inline-bytes (≥160
 *  bytes) when the caller is staying server-free.
 *
 *  Both produce a `https://<origin>/i/<base64url>` shape; the
 *  joiner side discriminates via `parseInviteSlug` on the byte
 *  length — see `INVITE_INLINE_MIN_LEN` for the gap that makes
 *  this safe.
 *
 *  Phase 1.18d1b: prefers the pointer form (shorter URLs, server-
 *  side max-uses enforcement in 1.18d2). Inline is the back-compat
 *  fallback for when `/discovery/invite` is unreachable (e.g. the
 *  censored-region path). */
export function composeInviteUrl(
  bytes: Uint8Array,
  origin: string = typeof location !== "undefined"
    ? location.origin
    : "https://lakko.me",
): string {
  return `${origin}${INVITE_URL_PREFIX}${base64UrlEncodeNoPad(bytes)}`;
}

/** Builder for the **pointer** URL form — the short
 *  `https://<host>/i/<blob_id_b64url>` shape backed by
 *  `/discovery/invite/<blob_id>` lookup. Equivalent to
 *  `composeInviteUrl` (just a thin name wrapper) but reads
 *  better at the call site. */
export function composePointerInviteUrl(
  blobId: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  requireBlobId(blobId, widths);
  return composeInviteUrl(blobId, origin);
}

/** Builder for the **inline-bytes** URL form — the long shape
 *  that carries the capability inside the URL itself, no server
 *  round-trip on open. 1.18c default; 1.18d1b's back-compat. */
export function composeInlineInviteUrl(
  capabilityBytes: Uint8Array,
  origin?: string,
): string {
  if (capabilityBytes.length < INVITE_INLINE_MIN_LEN) {
    throw new Error(
      `capabilityBytes must be ≥ ${INVITE_INLINE_MIN_LEN} bytes, got ${capabilityBytes.length}`,
    );
  }
  return composeInviteUrl(capabilityBytes, origin);
}

/** Discriminated parse of a URL slug. Returns the wire-form +
 *  the decoded bytes so the caller can dispatch:
 *
 *    * `kind: "pointer"` — fetch from
 *      `DiscoveryClient.fetchInvite(blobId)` → run
 *      `decodeAndVerify` on the returned bytes.
 *    * `kind: "inline"` — run `decodeAndVerify` directly on the
 *      bytes, no server round-trip.
 *
 *  The dispatch rule (`invite-link.ts`): 8 characters decoding
 *  to 6 bytes ⇒ pointer, ≥ 160 bytes ⇒ inline (1.18a wire-format
 *  minimum). Anything else, the pre-amendment 43-character id
 *  included, is refused. */
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
