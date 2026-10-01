// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The copy of the account-creation working state (docs/ux/anti-abuse.md
 * section 1, ADR-0054 S4): the one proof of work an account ever pays. The
 * same row, spinner, sentence and `Cancel` on web, iOS and Android.
 */

import { t } from "../l10n";

/** The disabled row while the signup works. */
export const signupWorkingTitle = (): string => t("auth.signup.working.title");
/** The guest account of a call link, same work. */
export const guestWorkingTitle = (): string => t("call.guest.working.title");
/** The one line below either. */
export const signupWorkingLine = (): string => t("auth.signup.working.line");
