// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KT-log S4 — reset-aware copy for a fail-closed `SupersessionRefused`.
 *
 * A `SupersessionRefused` is thrown when a peer's discovery bundle
 * carries an AccountIdentity key that DIFFERS from the locally-pinned
 * one and the change cannot be authorised by a supersession proof
 * (ADR-0016, fail-closed). KT-log S4 enriches that refusal with a
 * best-effort transparency-log marker (`loggedReset`) so the UI can let
 * a contact DISTINGUISH an owner-initiated identity RESET (the
 * lost-everything fresh-start, ADR-0017 — logged with `is_reset=true`)
 * from an unexplained key change (a possible compromise).
 *
 * This module is the PURE mapping from `(loggedReset, peerLabel)` to the
 * user-facing string. It lives in a plain-`.ts` module (not inline in
 * the SolidJS route) so it can be unit-tested in the plugin-free vitest
 * `node` environment without dragging in the JSX/Solid transform.
 *
 * The refusal itself is NEVER changed by the log — gate 1 throws
 * regardless of `loggedReset`; this only colours the message.
 */

/** The best-effort transparency-log reset marker for the refused key
 *  (the shape `SupersessionRefused.loggedReset` carries):
 *   * `true`  — the log confirms an owner-initiated reset.
 *   * `false` — the log does NOT show an owner reset.
 *   * `null`  — the log is off / unreachable / absent / inconclusive. */
export type LoggedReset = boolean | null;

/**
 * Render the reset-aware refusal copy.
 *
 * @param loggedReset the best-effort log marker attached to the refusal.
 * @param peerLabel   the handle the user typed (e.g. `@alice`), rendered
 *                    verbatim as the subject; falls back to
 *                    "This contact" when `undefined` / empty.
 */
export function supersessionRefusedMessage(
  loggedReset: LoggedReset,
  peerLabel?: string,
): string {
  const who =
    peerLabel !== undefined && peerLabel.length > 0
      ? peerLabel
      : "This contact";
  if (loggedReset === true) {
    return (
      `${who} reset their secure identity (for example, they lost ` +
      `their device). The transparency log confirms this was an ` +
      `owner-initiated reset. If you trust this is really them, ` +
      `re-verify their safety number out of band before continuing.`
    );
  }
  if (loggedReset === false) {
    return (
      `${who}'s secure identity changed, and the transparency log does ` +
      `NOT show an owner reset — treat this as a possible account ` +
      `compromise. Verify out of band before trusting.`
    );
  }
  return (
    `${who}'s secure identity changed and could not be confirmed ` +
    `against the transparency log. Verify out of band before trusting.`
  );
}
