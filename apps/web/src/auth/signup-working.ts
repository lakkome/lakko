// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The copy of the account-creation working state (docs/ux/anti-abuse.md
 * section 1, ADR-0054 S4): the one proof of work an account ever pays. The
 * same row, spinner, sentence and `Cancel` on web, iOS and Android.
 */

/** The disabled row while the signup works. */
export const SIGNUP_WORKING_TITLE = "Creating your account...";
/** The guest account of a call link, same work. */
export const GUEST_WORKING_TITLE = "Getting you into the call...";
/** The one line below either. */
export const SIGNUP_WORKING_LINE =
  "This takes a little while, once. You will not wait like this again.";
