// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side URL helpers for the group join-ticket flow (ADR-0028, G-3).
 *
 * Sibling of `apps/web/src/calls/invite.ts` for the `GroupInviteCapability`
 * ecosystem. The `/g/<slug>` slug comes in two forms, discriminated by the
 * shared rules of `invite-link.ts` (ADR-0028 short-link amendment,
 * 2026-09-27):
 *
 *   * **pointer** — the 6-byte ledger id (8 base64url characters) the
 *     discovery service assigned at `DiscoveryClient.uploadGroupInvite(...)`.
 *     The production default; the joiner fetches the capability from
 *     `/discovery/group-invite/<id>`. This is the OQ1 indirection that adds the
 *     server-side anti-abuse ledger (consume dedup + soft max_uses).
 *   * **inline** — the whole SEALED link blob. The fallback for a transport
 *     failure at upload: the sealed capability rides inside the URL itself.
 *
 * Both forms carry the 12-byte **link secret** as the URL fragment
 * (`#<16 characters>`): the capability is sealed under it
 * (`GroupInviteCapability.openLink`) and the preview opens with the same
 * secret. A link without it is damaged.
 */
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  appendInviteLinkSecret,
  classifyInviteSlug,
  requireBlobId,
  splitInviteLink,
  type ClassifiedInviteSlug,
  type InviteLinkFragment,
  type InviteLinkWidths,
} from "./invite-link";

/** Path prefix every group-invite URL starts with — kept in lock-step with
 *  `routes/paths.ts::groupJoinRoutePattern` (`/g/:slug`). The `/g/` shape
 *  contrasts with contact-invite `/i/` and call-invite `/c/` so the joiner-side
 *  landing dispatcher branches on the path segment without parsing the slug. */
export const GROUP_INVITE_URL_PREFIX = "/g/";

/** Floor on the inline-bytes form. The real minimum is 110 bytes (a 1-byte
 *  group_id); the discriminator floor is the shared 100. */
export const GROUP_INVITE_INLINE_MIN_LEN = 100;

function defaultOrigin(): string {
  return typeof location !== "undefined" ? location.origin : "https://lakko.me";
}

/** Build the **pointer** URL form — `https://<host>/g/<8-char id>`, backed by
 *  the `/discovery/group-invite/<id>` lookup. */
export function composePointerGroupInviteUrl(
  blobId: Uint8Array,
  widths: InviteLinkWidths,
  origin: string = defaultOrigin(),
): string {
  requireBlobId(blobId, widths);
  return `${origin}${GROUP_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(blobId)}`;
}

/** Build the **inline-bytes** URL form — the long shape that carries the
 *  capability inside the URL itself, no server round-trip on open. Used only
 *  when the upload failed on a transport error. */
export function composeInlineGroupInviteUrl(
  sealed: Uint8Array,
  origin: string = defaultOrigin(),
): string {
  if (sealed.length < GROUP_INVITE_INLINE_MIN_LEN) {
    throw new Error(
      `sealed must be >= ${GROUP_INVITE_INLINE_MIN_LEN} bytes, got ${sealed.length}`,
    );
  }
  return `${origin}${GROUP_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(sealed)}`;
}

/** Discriminated parse of a `/g/<slug>` slug:
 *
 *    * `kind: "pointer"` — fetch the sealed blob from
 *      `DiscoveryClient.fetchGroupInvite(blobId)`, then open it with the
 *      fragment's secret (`GroupInviteCapability.openLink`).
 *    * `kind: "inline"` — the sealed blob is the slug itself.
 *
 *  Throws on anything else, the pre-amendment 43-character id included. */
export type ParsedGroupInvite = ClassifiedInviteSlug;

export function parseGroupInviteSlug(slug: string, widths: InviteLinkWidths): ParsedGroupInvite {
  return classifyInviteSlug(slug, widths, GROUP_INVITE_INLINE_MIN_LEN, "group-invite");
}

// ----------------------------------------------------------------------------
// The link secret (sealed-link amendment of ADR-0028, 2026-09-28)
// ----------------------------------------------------------------------------

/** Append the link secret to a `/g/` URL as its fragment. */
export function appendGroupInviteLinkSecret(
  url: string,
  secret: Uint8Array,
  widths: InviteLinkWidths,
): string {
  return appendInviteLinkSecret(url, secret, widths);
}

/** Split a full `/g/` link into its slug and its fragment (missing or
 *  malformed = damaged). `null` when the string is not a group-invite link. */
export function splitGroupInviteLink(
  link: string,
  widths: InviteLinkWidths,
): { readonly slug: string; readonly fragment: InviteLinkFragment } | null {
  return splitInviteLink(link, GROUP_INVITE_URL_PREFIX, widths);
}
