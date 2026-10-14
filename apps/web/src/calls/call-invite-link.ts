// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A call invitation sent as a message (`docs/ux/call-links.md` section 3).
 *
 * `Invite a contact` sends the call's link into the conversation with that
 * person. The message is the link and nothing else, so every client can tell
 * it from ordinary text without a new wire field: a message whose whole text
 * is one Lakko call link is drawn as an invitation with a `Join` button,
 * never as a line of characters to copy.
 *
 * The check is strict on purpose. Only a link to THIS deployment counts: the
 * `Join` button navigates inside the app, and a link that merely looks like
 * one but names another host must stay plain text, not gain a button that
 * suggests it is ours.
 *
 * Pure: the caller passes the app's own origin.
 */

/** Where `Join` goes: the in-app path and fragment of the link. */
export interface CallInviteTarget {
  /** `/c/<slug>#<secret>`, ready for the router. */
  readonly href: string;
}

const SLUG = /^[A-Za-z0-9_-]{1,512}$/;
const SECRET = /^[A-Za-z0-9_-]{1,128}$/;

/**
 * The invitation a message text is, or `null` when it is ordinary text.
 *
 * `text` must be exactly one link (surrounding whitespace aside), on
 * `ownOrigin`, of the shape `/c/<slug>#<secret>`.
 */
export function callInviteOf(text: string, ownOrigin: string): CallInviteTarget | null {
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
  if (url.origin !== own.origin) return null;
  if (url.username !== "" || url.password !== "" || url.search !== "") return null;
  const parts = url.pathname.split("/");
  // ["", "c", "<slug>"]
  if (parts.length !== 3 || parts[1] !== "c") return null;
  const slug = parts[2] ?? "";
  const secret = url.hash.startsWith("#") ? url.hash.slice(1) : "";
  if (!SLUG.test(slug) || !SECRET.test(secret)) return null;
  return { href: `/c/${slug}#${secret}` };
}
