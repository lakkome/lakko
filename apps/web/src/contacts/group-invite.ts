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
 *   * **inline** — the full `GroupInviteCapability` wire bytes (>= 110 bytes).
 *     The fallback for a transport failure at upload: the capability rides
 *     inside the URL itself.
 *
 * Either form may carry the 12-byte **link secret** as the URL fragment
 * (`#<16 characters>`); the preview key is derived from it bound to the
 * capability's group id and issuer. See `appendGroupInviteLinkSecret` /
 * `splitGroupInviteLink` below.
 */
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  classifyInviteSlug,
  parseGroupInviteFragment,
  requireBlobId,
  type ClassifiedInviteSlug,
  type GroupInviteFragment,
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
  capabilityBytes: Uint8Array,
  origin: string = defaultOrigin(),
): string {
  if (capabilityBytes.length < GROUP_INVITE_INLINE_MIN_LEN) {
    throw new Error(
      `capabilityBytes must be >= ${GROUP_INVITE_INLINE_MIN_LEN} bytes, ` +
        `got ${capabilityBytes.length}`,
    );
  }
  return `${origin}${GROUP_INVITE_URL_PREFIX}${base64UrlEncodeNoPad(capabilityBytes)}`;
}

/** Discriminated parse of a `/g/<slug>` slug:
 *
 *    * `kind: "pointer"` — fetch from
 *      `DiscoveryClient.fetchGroupInvite(blobId)` -> run
 *      `wasm.GroupInviteCapability.decodeAndVerify` on the returned bytes.
 *    * `kind: "inline"` — run `decodeAndVerify` directly on the bytes, no
 *      server round-trip.
 *
 *  Throws on anything else, the pre-amendment 43-character id included. */
export type ParsedGroupInvite = ClassifiedInviteSlug;

export function parseGroupInviteSlug(slug: string, widths: InviteLinkWidths): ParsedGroupInvite {
  return classifyInviteSlug(slug, widths, GROUP_INVITE_INLINE_MIN_LEN, "group-invite");
}

// ----------------------------------------------------------------------------
// The link secret (ADR-0028 v2 + short-link amendment, docs/ux/group-invite-page.md section 4)
// ----------------------------------------------------------------------------

/** Append the link secret to a `/g/` URL as its **fragment**:
 *  `<origin>/g/<slug>#<secret, 16 base64url characters>`. The fragment is the
 *  one part of a URL a browser never sends, so the discovery ledger and every
 *  access log see only the sealed preview. Both the pointer and the inline
 *  form take the same fragment. */
export function appendGroupInviteLinkSecret(
  url: string,
  secret: Uint8Array,
  widths: InviteLinkWidths,
): string {
  if (secret.length !== widths.linkSecretLen) {
    throw new Error(`link secret must be ${widths.linkSecretLen} bytes, got ${secret.length}`);
  }
  if (url.includes("#")) {
    throw new Error("url already carries a fragment");
  }
  return `${url}#${base64UrlEncodeNoPad(secret)}`;
}

/** Read a `/g/` fragment. See `parseGroupInviteFragment` in `invite-link.ts`:
 *  `none`, `secret`, or `damaged` (which the page refuses). */
export function decodeGroupInviteFragment(
  fragment: string | null | undefined,
  widths: InviteLinkWidths,
): GroupInviteFragment {
  return parseGroupInviteFragment(fragment, widths);
}

/** Split a full `/g/` link into its slug and its fragment. `null` when the
 *  string is not a group-invite link at all. The revoke path reads the slug
 *  from here, so the fragment can never be mistaken for part of the id. */
export function splitGroupInviteLink(
  link: string,
  widths: InviteLinkWidths,
): { readonly slug: string; readonly fragment: GroupInviteFragment } | null {
  const idx = link.lastIndexOf(GROUP_INVITE_URL_PREFIX);
  if (idx < 0) return null;
  const rest = link.slice(idx + GROUP_INVITE_URL_PREFIX.length);
  const hashAt = rest.indexOf("#");
  const slug = hashAt < 0 ? rest : rest.slice(0, hashAt);
  if (slug.length === 0) return null;
  const fragment =
    hashAt < 0 ? ({ kind: "none" } as const) : parseGroupInviteFragment(rest.slice(hashAt), widths);
  return { slug, fragment };
}
