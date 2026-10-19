// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side URL helpers for the group join-ticket flow (ADR-0028, G-3).
 *
 * Sibling of `apps/web/src/calls/invite.ts` for the `GroupInviteCapability`
 * ecosystem. The `/g/<slug>` slug is one form, checked by the shared rules of
 * `invite-link.ts` (ADR-0028 short-link amendment, 2026-09-27):
 *
 *   * **pointer** — the 6-byte ledger id (8 base64url characters) the
 *     discovery service assigned at `DiscoveryClient.uploadGroupInvite(...)`.
 *     The production default; the joiner fetches the capability from
 *     `/discovery/group-invite/<id>`. This is the OQ1 indirection that adds the
 *     server-side anti-abuse ledger (consume dedup + soft max_uses).
 *
 * The inline form (the whole sealed blob in the path) was removed on
 * 2026-10-19: a failed upload mints no link, and an inline URL is refused as a
 * malformed slug.
 *
 * The link carries the 12-byte **link secret** as the URL fragment
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

/** Parse a `/g/<slug>` slug into its ledger id: fetch the sealed blob from
 *  `DiscoveryClient.fetchGroupInvite(blobId)`, then open it with the
 *  fragment's secret (`GroupInviteCapability.openLink`). Throws on anything
 *  else, the pre-amendment 43-character id and an inline sealed blob
 *  included. */
export type ParsedGroupInvite = ClassifiedInviteSlug;

export function parseGroupInviteSlug(slug: string, widths: InviteLinkWidths): ParsedGroupInvite {
  return classifyInviteSlug(slug, widths, "group-invite");
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
