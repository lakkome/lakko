// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side helpers for the link-based call-invite flow
 * (Phase 3c3-url-helpers).
 *
 * Sibling of `apps/web/src/contacts/invite.ts` for the
 * `CallInviteCapability` ecosystem. Two surfaces:
 *
 *   * URL plumbing — `composeCallInviteUrl(...)` +
 *     `composePointerCallInviteUrl(...)` +
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
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";

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

/** Length of a content-addressed `blob_id` from
 *  `services/discovery/src/call_invite.rs` — SHA-256, 32
 *  bytes. Same shape as the contact-invite ledger; the dedup
 *  tables on the server are partitioned by table, not by
 *  id-namespace, so the layout is allowed to collide. */
export const CALL_INVITE_BLOB_ID_LEN = 32;

/** Lower bound on the inline-bytes form. The 3c3 wire format
 *  with a 256-byte one-time KP tops out around 500 bytes; with
 *  a 4 KiB KP around 4308 bytes. The smallest legal payload
 *  (256-byte KP + empty label) is well above 32, so a slug-
 *  byte-length test cleanly discriminates inline vs pointer
 *  URLs. We pin the floor at 400 to leave headroom for KP
 *  formats that might shrink slightly. */
export const CALL_INVITE_INLINE_MIN_LEN = 400;

/** Auto-pick the URL form by argument shape — pointer (32
 *  bytes) when the caller has uploaded the capability and
 *  wants the short form, inline-bytes (≥ 400 bytes) when the
 *  caller is staying server-free.
 *
 *  Both produce a `https://<origin>/c/<base64url>` shape; the
 *  joiner side discriminates via `parseCallInviteSlug` on the
 *  byte length. */
export function composeCallInviteUrl(
  bytes: Uint8Array,
  origin: string = typeof location !== "undefined"
    ? location.origin
    : "https://lakko.me",
): string {
  return `${origin}${CALL_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(bytes)}`;
}

/** Builder for the **pointer** URL form — the short
 *  `https://<host>/c/<blob_id_b64url>` shape backed by
 *  `/discovery/call-invite/<blob_id>` lookup. Equivalent to
 *  `composeCallInviteUrl` (just a thin name wrapper) but reads
 *  better at the call site after a `uploadCallInvite(...)`
 *  round-trip. */
export function composePointerCallInviteUrl(
  blobId: Uint8Array,
  origin?: string,
): string {
  if (blobId.length !== CALL_INVITE_BLOB_ID_LEN) {
    throw new Error(
      `blobId must be ${CALL_INVITE_BLOB_ID_LEN} bytes, got ${blobId.length}`,
    );
  }
  return composeCallInviteUrl(blobId, origin);
}

/** Builder for the **inline-bytes** URL form — the long shape
 *  that carries the capability inside the URL itself, no
 *  server round-trip on open. Reserved for the censored-region
 *  fallback where `/discovery/call-invite` is unreachable; the
 *  pointer form is the production default. */
export function composeInlineCallInviteUrl(
  capabilityBytes: Uint8Array,
  origin?: string,
): string {
  if (capabilityBytes.length < CALL_INVITE_INLINE_MIN_LEN) {
    throw new Error(
      `capabilityBytes must be ≥ ${CALL_INVITE_INLINE_MIN_LEN} bytes, got ${capabilityBytes.length}`,
    );
  }
  return composeCallInviteUrl(capabilityBytes, origin);
}

/** Discriminated parse of a URL slug. Returns the wire-form +
 *  the decoded bytes so the caller can dispatch:
 *
 *    * `kind: "pointer"` — fetch from
 *      `DiscoveryClient.fetchCallInvite(blobId)` → run
 *      `decodeAndVerifyCallInviteCapability` on the returned
 *      bytes.
 *    * `kind: "inline"` — run
 *      `decodeAndVerifyCallInviteCapability` directly on the
 *      bytes, no server round-trip.
 *
 *  The dispatch rule is a clean byte-length test: 32 ⇒
 *  pointer (SHA-256), ≥ 400 ⇒ inline (3c3 wire-format minimum
 *  with a 256-byte KP). Anything else is invalid input. */
export type ParsedCallInvite =
  | { readonly kind: "pointer"; readonly blobId: Uint8Array }
  | { readonly kind: "inline"; readonly capabilityBytes: Uint8Array };

export function parseCallInviteSlug(slug: string): ParsedCallInvite {
  const bytes = base64UrlDecodeNoPad(slug);
  if (bytes.length === CALL_INVITE_BLOB_ID_LEN) {
    return { kind: "pointer", blobId: bytes };
  }
  if (bytes.length >= CALL_INVITE_INLINE_MIN_LEN) {
    return { kind: "inline", capabilityBytes: bytes };
  }
  throw new Error(
    `call-invite slug decoded to ${bytes.length} bytes — neither a ${CALL_INVITE_BLOB_ID_LEN}-byte pointer ` +
      `nor a ≥ ${CALL_INVITE_INLINE_MIN_LEN}-byte inline capability`,
  );
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
