// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The auth links a signed-out visitor sees on the `/g/<slug>` group invite.
 *
 * Both carry the invite as `?return=` so the visitor lands back on it after
 * signing up or logging in, the same round-trip the `/i/` contact invite uses
 * (`auth/return-url.ts`). Kept out of `JoinGroup.tsx` because `vitest` cannot
 * load a `.tsx` module.
 */
import { buildAuthUrlWithReturn } from "../auth/return-url";
import { paths } from "./paths";

/** Sign-up and log-in hrefs that return to `/g/<slug>`. */
export function joinGroupAuthLinks(slug: string): {
  readonly signup: string;
  readonly login: string;
} {
  const returnTo = `/g/${slug}`;
  return {
    signup: buildAuthUrlWithReturn(paths.signup, returnTo),
    login: buildAuthUrlWithReturn(paths.login, returnTo),
  };
}
