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
 * The slug AND the fragment of a pasted call link. The fragment is the link
 * secret the capability is sealed under (sealed-link amendment of ADR-0028 /
 * ADR-0009, 2026-09-28), so the landing page must receive it: dropping it
 * would turn every pasted link into a damaged one. `hash` is `""` when the
 * paste carried none (the landing page then shows the damaged state).
 */
export function callLinkTarget(input: string): { slug: string; hash: string } | null {
  const slug = callLinkSlug(input);
  if (slug === null) return null;
  let hash = "";
  try {
    hash = new URL(input.trim(), "https://lakko.me").hash;
  } catch {
    hash = "";
  }
  return { slug, hash };
}
