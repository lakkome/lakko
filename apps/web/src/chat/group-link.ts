// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether a message is one `/g/` link of this deployment (ADR-0087 section
 * 14): the whole text, on this origin, with a slug and a fragment secret. Such
 * a message may be a channel invitation (`ChannelInviteCard.tsx`).
 */

const SLUG = /^[A-Za-z0-9_-]{1,512}$/;
const SECRET = /^[A-Za-z0-9_-]{1,128}$/;

/** The `/g/<slug>#<secret>` link a message is made of, on this deployment's
 *  origin, or `null`. The whole text must be the link. */
export function groupLinkOf(text: string, ownOrigin: string): { readonly href: string; readonly url: string } | null {
  const trimmed = text.trim();
  if (trimmed === "" || /\s/.test(trimmed)) return null;
  let url: URL;
  let own: URL;
  try {
    url = new URL(trimmed);
    own = new URL(ownOrigin);
  } catch {
    return null;
  }
  if (url.origin !== own.origin || url.username !== "" || url.password !== "" || url.search !== "") return null;
  const parts = url.pathname.split("/");
  if (parts.length !== 3 || parts[1] !== "g") return null;
  const slug = parts[2] ?? "";
  const secret = url.hash.startsWith("#") ? url.hash.slice(1) : "";
  if (!SLUG.test(slug) || !SECRET.test(secret)) return null;
  return { href: `/g/${slug}#${secret}`, url: url.href };
}

