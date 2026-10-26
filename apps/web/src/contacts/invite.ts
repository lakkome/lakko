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

/** The unit of a custom expiry period. */
export type ExpiryUnit = "hours" | "days";

/** The longest custom period a link may be given: 365 days. Chosen, not
 *  measured; the same bound on iOS and Android. */
export const CUSTOM_EXPIRY_MAX_MS = 365 * 86_400_000;

/** The wire expiry for a custom period of `amount` `unit`s from `nowMs`, or
 *  `null` when the text is not a whole number or the period is outside
 *  1 hour .. 365 days. */
export function customExpiryMs(amount: string, unit: ExpiryUnit, nowMs: number): bigint | null {
  const n = parseWholeNumber(amount);
  if (n === null) return null;
  const ms = n * (unit === "hours" ? 3_600_000 : 86_400_000);
  if (ms < 3_600_000 || ms > CUSTOM_EXPIRY_MAX_MS) return null;
  return BigInt(nowMs + ms);
}

/** The largest custom number of people a link may admit. Chosen, not
 *  measured; the same bound on iOS and Android. */
export const CUSTOM_MAX_USES = 10_000;

/** The wire `max_uses` for a custom number of people, or `null` when the text
 *  is not a whole number in 1 .. {@link CUSTOM_MAX_USES}. One person is the
 *  single-use sentinel `0`, as the preset writes it. */
export function customMaxUses(count: string): number | null {
  const n = parseWholeNumber(count);
  if (n === null || n < 1 || n > CUSTOM_MAX_USES) return null;
  return n === 1 ? 0 : n;
}

/** The default terms of a link made by Create link (2026-10-26): no label,
 *  seven days, one person. */
export const DEFAULT_LINK_EXPIRY: ExpiryPreset = "7d";
export const DEFAULT_LINK_USES: MaxUsesPreset = "single";

/** The terms form of a link, as its fields hold them. */
export interface LinkTermsForm {
  readonly label: string;
  readonly expiry: ExpiryPreset | "custom";
  readonly customExpiryAmount: string;
  readonly customExpiryUnit: ExpiryUnit;
  readonly maxUses: MaxUsesPreset | "custom";
  readonly customUses: string;
}

/** The terms form prefilled from a link (UX section 9): the period it was
 *  given at minting (a preset when it is one within a minute, else custom in
 *  days when whole, else hours) and its use limit. Pure. */
export function termsFormOf(link: {
  readonly label: string;
  readonly expiryMs: number;
  readonly mintedAtMs: number;
  readonly maxUses: number;
}): LinkTermsForm {
  const base = { label: link.label, customExpiryAmount: "", customExpiryUnit: "days" as ExpiryUnit };
  let expiry: Pick<LinkTermsForm, "expiry" | "customExpiryAmount" | "customExpiryUnit">;
  if (link.expiryMs === 0) {
    expiry = { ...base, expiry: "never" };
  } else {
    const period = Math.max(0, link.expiryMs - link.mintedAtMs);
    const near = (days: number): boolean => Math.abs(period - days * 86_400_000) <= 60_000;
    if (near(1)) expiry = { ...base, expiry: "1d" };
    else if (near(7)) expiry = { ...base, expiry: "7d" };
    else if (near(30)) expiry = { ...base, expiry: "30d" };
    else {
      const days = Math.round(period / 86_400_000);
      const wholeDays = days >= 1 && Math.abs(period - days * 86_400_000) <= 60_000;
      expiry = wholeDays
        ? { expiry: "custom", customExpiryAmount: String(days), customExpiryUnit: "days" }
        : {
            expiry: "custom",
            customExpiryAmount: String(Math.max(1, Math.round(period / 3_600_000))),
            customExpiryUnit: "hours",
          };
    }
  }
  const uses: Pick<LinkTermsForm, "maxUses" | "customUses"> =
    link.maxUses === 0 || link.maxUses === 1
      ? { maxUses: "single", customUses: "" }
      : link.maxUses === 10
        ? { maxUses: "ten", customUses: "" }
        : link.maxUses === 0xff_ff_ff_ff
          ? { maxUses: "unlimited", customUses: "" }
          : { maxUses: "custom", customUses: String(link.maxUses) };
  return { label: link.label, ...expiry, ...uses };
}

function parseWholeNumber(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d{1,9}$/.test(trimmed)) return null;
  return Number(trimmed);
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
