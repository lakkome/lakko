// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the invite-link flow (Phase 1.18c).
 *
 * Two surfaces:
 *
 *   * URL plumbing — `composePointerInviteUrl(...)` /
 *     `composeInlineInviteUrl(...)` + `parseInviteSlug(...)`. Since the
 *     2026-09-28 security pass the capability is sealed under a link
 *     secret carried in the URL fragment; the slug is either the ledger
 *     id of the sealed blob (the default) or the sealed blob itself (the
 *     fallback when the upload fails on a transport error).
 *   * Form-side enums — `ExpiryPreset` and `MaxUsesPreset` —
 *     translate UI dropdown choices into the wire-level
 *     `InviteCapability.compose` arguments. Putting them here
 *     instead of inside the route component keeps the route file
 *     focused on rendering and gives unit-test coverage for the
 *     defaulting logic without spinning up Solid.
 */
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  classifyInviteSlug,
  linkFragment,
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

/** Lower bound on the inline form. An inline `/i/` slug carries the sealed
 *  link blob (the signed capability plus the seal's 30 bytes), which is well
 *  over 160 bytes even with an empty label; far above the 6-byte ledger id,
 *  so a slug-byte-length test cleanly discriminates inline vs pointer. */
export const INVITE_INLINE_MIN_LEN = 160;

function defaultOrigin(): string {
  return typeof location !== "undefined" ? location.origin : "https://lakko.me";
}

/** The **pointer** URL, `<origin>/i/<8-char ledger id>#<link secret>`: the
 *  production form, backed by `/discovery/invite/<id>`. The secret rides in
 *  the fragment, which a browser never sends, so the ledger and every access
 *  log see only the sealed blob (2026-09-28 security pass). */
export function composePointerInviteUrl(
  blobId: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = defaultOrigin(),
): string {
  requireBlobId(blobId, widths);
  return `${origin}${INVITE_URL_PREFIX}${base64UrlEncodeNoPad(blobId)}${linkFragment(linkSecret, widths)}`;
}

/** The **inline** URL, `<origin>/i/<base64url sealed blob>#<link secret>`:
 *  no server round-trip on open. The fallback for a transport failure at
 *  upload. */
export function composeInlineInviteUrl(
  sealed: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = defaultOrigin(),
): string {
  if (sealed.length < INVITE_INLINE_MIN_LEN) {
    throw new Error(`sealed link must be >= ${INVITE_INLINE_MIN_LEN} bytes, got ${sealed.length}`);
  }
  return `${origin}${INVITE_URL_PREFIX}${base64UrlEncodeNoPad(sealed)}${linkFragment(linkSecret, widths)}`;
}

/** Discriminated parse of a URL slug:
 *
 *    * `kind: "pointer"` — fetch the sealed blob with
 *      `DiscoveryClient.fetchInvite(blobId)`;
 *    * `kind: "inline"` — the sealed blob is the slug itself.
 *
 *  Either way the joiner opens it with `InviteCapability.openLink(sealed,
 *  linkSecret)`, the secret read from the fragment. The dispatch rule
 *  (`invite-link.ts`): 8 characters decoding to 6 bytes is a pointer,
 *  >= 160 bytes is inline, anything else (the pre-amendment 43-character id
 *  included) is refused. */
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
