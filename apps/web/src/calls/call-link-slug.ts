// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `/c/<slug>` slug from whatever the user pasted into "Join a call": a
 * full URL, a bare path, or nothing usable. Tested in `call-link-slug.test.ts`.
 */
export function callLinkSlug(input: string): string | null {
  const trimmed = input.trim();
  if (trimmed.length === 0) return null;
  let path: string;
  try {
    path = new URL(trimmed, "https://lakko.me").pathname;
  } catch {
    return null;
  }
  const m = /^\/c\/([A-Za-z0-9_-]+)\/?$/.exec(path);
  return m === null ? null : (m[1] ?? null);
}

/**
 * The in-app path to open a pasted call link: `/c/<slug>#<fragment>`. The
 * fragment is the link secret the whole capability is sealed under (the
 * 2026-09-28 security pass), so it comes along verbatim; a link pasted
 * without one still navigates, and the landing screen says it is damaged.
 * `null` when the input is not a call link at all.
 */
export function callLinkPath(input: string): string | null {
  const slug = callLinkSlug(input);
  if (slug === null) return null;
  const trimmed = input.trim();
  const hashAt = trimmed.indexOf("#");
  const fragment = hashAt < 0 ? "" : trimmed.slice(hashAt);
  return `/c/${slug}${fragment}`;
}
