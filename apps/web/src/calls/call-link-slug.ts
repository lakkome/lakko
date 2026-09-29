// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `/c/<slug>#<secret>` link from whatever the user pasted into "Join a
 * call": a full URL, a bare path, or nothing usable. The fragment is kept
 * verbatim: since the sealed-link amendment of 2026-09-28 it is the secret the
 * capability is sealed under, and a link routed without it opens as damaged.
 * Tested in `call-link-slug.test.ts`.
 */
export interface CallLinkTarget {
  /** The path segment after `/c/`. */
  readonly slug: string;
  /** The fragment with its `#`, or `""` when the pasted link had none. */
  readonly fragment: string;
}

export function callLinkTarget(input: string): CallLinkTarget | null {
  const trimmed = input.trim();
  if (trimmed.length === 0) return null;
  let url: URL;
  try {
    url = new URL(trimmed, "https://lakko.me");
  } catch {
    return null;
  }
  const m = /^\/c\/([A-Za-z0-9_-]+)\/?$/.exec(url.pathname);
  const slug = m === null ? null : (m[1] ?? null);
  if (slug === null) return null;
  return { slug, fragment: url.hash };
}

/** The slug alone. */
export function callLinkSlug(input: string): string | null {
  return callLinkTarget(input)?.slug ?? null;
}
