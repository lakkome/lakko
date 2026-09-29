// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web-side URL helpers for the group join-ticket flow (ADR-0028, G-3).
 *
 * Sibling of `apps/web/src/calls/invite.ts` for the `GroupInviteCapability`
 * ecosystem, over the shared sealed-link rules of `invite-link.ts` (the
 * short-link amendment of 2026-09-27 and the sealed-link amendment of
 * 2026-09-28). A `/g/` link reads `https://<host>/g/<slug>#<secret>`:
 *
 *   * **pointer** slug — the 6-byte ledger id (8 base64url characters) the
 *     discovery service assigned at `DiscoveryClient.uploadGroupInvite(...)`;
 *     the joiner fetches the sealed blob from `/discovery/group-invite/<id>`.
 *   * **inline** slug — the whole sealed blob, the fallback for a transport
 *     failure at upload.
 *
 * The 12-byte secret in the fragment opens the sealed blob
 * (`GroupInviteCapability.openLink`) and, under an independent derivation,
 * the capability's sealed preview (`openPreview`). A link without it is
 * damaged.
 */
import {
  classifyInviteSlug,
  composeInlineInviteLink,
  composePointerInviteLink,
  splitInviteLink,
  type ClassifiedInviteSlug,
  type InviteLinkFragment,
  type InviteLinkWidths,
} from "./invite-link";

/** Path prefix every group-invite URL starts with — kept in lock-step with
 *  `routes/paths.ts::groupJoinRoutePattern` (`/g/:slug`). */
export const GROUP_INVITE_URL_PREFIX = "/g/";

/** Floor on the inline form. A sealed `/g/` blob is at least the 110-byte
 *  capability plus the 30-byte seal envelope; the discriminator floor is the
 *  shared 100. */
export const GROUP_INVITE_INLINE_MIN_LEN = 100;

/** Build the **pointer** link — `https://<host>/g/<8-char id>#<secret>`. */
export function composePointerGroupInviteUrl(
  blobId: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composePointerInviteLink("g", blobId, linkSecret, widths, origin);
}

/** Build the **inline** link — `https://<host>/g/<base64url sealed>#<secret>`,
 *  used only when the upload failed on transport. */
export function composeInlineGroupInviteUrl(
  sealed: Uint8Array,
  linkSecret: Uint8Array,
  widths: InviteLinkWidths,
  origin?: string,
): string {
  return composeInlineInviteLink(
    "g",
    sealed,
    linkSecret,
    widths,
    GROUP_INVITE_INLINE_MIN_LEN,
    origin,
  );
}

/** Discriminated parse of a `/g/<slug>` slug: `pointer` (fetch the sealed
 *  blob) or `inline` (the slug is the sealed blob). Throws on anything else,
 *  the pre-amendment 43-character id included. */
export type ParsedGroupInvite = ClassifiedInviteSlug;

export function parseGroupInviteSlug(slug: string, widths: InviteLinkWidths): ParsedGroupInvite {
  return classifyInviteSlug(slug, widths, GROUP_INVITE_INLINE_MIN_LEN, "group-invite");
}

/** Split a full `/g/` link into its slug and its fragment. `null` when the
 *  string is not a group-invite link at all; a link without a fragment splits
 *  with a `damaged` fragment. */
export function splitGroupInviteLink(
  link: string,
  widths: InviteLinkWidths,
): { readonly slug: string; readonly fragment: InviteLinkFragment } | null {
  return splitInviteLink(link, "g", widths);
}
