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
