// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The auth links a signed-out visitor sees on the `/g/<slug>` group invite.
 *
 * Both carry the invite as `?return=` so the visitor lands back on it after
 * signing up or logging in, the same round-trip the `/i/` contact invite uses
 * (`auth/return-url.ts`). The return path keeps the URL fragment — the link
 * key of ADR-0028 v2 — so the preview the visitor saw before signing up is
 * still there when they come back. Kept out of `JoinGroup.tsx` because
 * `vitest` cannot load a `.tsx` module.
 */
import { buildAuthUrlWithReturn } from "../auth/return-url";
import { paths } from "./paths";

/** Sign-up and log-in hrefs that return to `/g/<slug>`, carrying `hash`
 *  (`location.hash`, `#…` or empty) along. */
export function joinGroupAuthLinks(
  slug: string,
  hash: string = "",
): {
  readonly signup: string;
  readonly login: string;
} {
  const fragment = hash.length === 0 || hash.startsWith("#") ? hash : `#${hash}`;
  const returnTo = `/g/${slug}${fragment}`;
  return {
    signup: buildAuthUrlWithReturn(paths.signup, returnTo),
    login: buildAuthUrlWithReturn(paths.login, returnTo),
  };
}
