// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-authentication deep-link return helpers (Phase 1.18e).
 *
 * The `/i/:slug` invite-link landing page (1.18c) presents
 * "Sign up or log in to accept" to anonymous visitors. Before
 * 1.18e, clicking either button navigated to `/signup` /
 * `/login` and lost the original URL — the joiner had to
 * manually re-open the invite link after authenticating.
 *
 * This module gives the three touchpoints — the auth-form
 * links on the invite landing, plus `Login.tsx` and
 * `Signup.tsx` after success — a single convention to use:
 *
 *   * **Build** a sign-in / sign-up URL that preserves the
 *     current location via [`buildAuthUrlWithReturn`].
 *   * **Read** the `?return=` query on the auth form and
 *     navigate to it on success via [`readReturnUrl`].
 *
 * Returns are encoded as a `?return=<path>` query parameter
 * for transparency (one look at the URL bar shows where you
 * end up). The parser refuses anything that doesn't look like
 * an own-origin path — defense against an open-redirect
 * vector where an attacker hands a joiner an URL like
 * `https://lakko.me/login?return=https://evil.com`.
 *
 * ## Open-redirect threat model
 *
 * Without validation, an attacker could craft
 * `/login?return=https://evil.com/lookalike` and trick a
 * just-authenticated user into a phishing page that looks
 * like Lakko. The validator below enforces:
 *
 *   1. Must start with `/`.
 *   2. Must NOT start with `//` (which would parse as a
 *      protocol-relative URL).
 *   3. Must NOT start with `/\` (Edge-case for legacy IE
 *      parsing; defensive belt-and-suspenders).
 *
 * Anything else falls back to the default home path. The
 * shape of the rejection (silent fallback vs. visible error)
 * is "silent" by design — a malformed `?return=` is usually
 * a stale link or copy-paste accident, not a deliberate
 * attack we want to surface to the legitimate user.
 */
import type { Location } from "@solidjs/router";

/** Default landing if `?return=` is absent or malformed. */
const DEFAULT_RETURN = "/";

/**
 * Read the `?return=` query parameter off the current
 * location and validate it. Returns the validated path
 * (always starts with `/`, always own-origin), or
 * [`DEFAULT_RETURN`] if absent / malformed / unsafe.
 *
 * Caller drives `navigate(readReturnUrl(location), { replace: true })`.
 * `replace: true` is load-bearing: the auth page should not
 * sit in browser history after a successful redirect.
 */
export function readReturnUrl(location: Location): string {
  // `location.search` is the raw query string including the
  // leading `?` (e.g. "?return=%2Fi%2Fabc"). URLSearchParams
  // handles the decode + edge cases.
  const params = new URLSearchParams(location.search);
  const raw = params.get("return");
  if (raw === null || raw === "") return DEFAULT_RETURN;

  // Validate: own-origin path only.
  if (!raw.startsWith("/")) return DEFAULT_RETURN;
  if (raw.startsWith("//")) return DEFAULT_RETURN;
  if (raw.startsWith("/\\")) return DEFAULT_RETURN;

  return raw;
}

/**
 * Build a URL pointing at `authPath` (`/login` or `/signup`)
 * that carries the **current** path as `?return=`. Used by
 * the anon-side auth links on the invite landing page so a
 * joiner who clicks "Sign up" gets bounced back to the
 * invite after the OPAQUE flow completes.
 *
 * @param authPath    `paths.login` or `paths.signup` (always
 *                    absolute, always starts with `/`).
 * @param returnPath  Path to come back to after auth. Pass
 *                    `window.location.pathname` for the
 *                    canonical use case.
 *
 * Skips the `?return=` part entirely when `returnPath` is
 * the default home — keeps `/login` / `/signup` URLs clean
 * when the user clicks them from the navbar instead of from
 * a deep link.
 */
export function buildAuthUrlWithReturn(
  authPath: string,
  returnPath: string,
): string {
  if (returnPath === DEFAULT_RETURN) return authPath;
  return `${authPath}?return=${encodeURIComponent(returnPath)}`;
}
