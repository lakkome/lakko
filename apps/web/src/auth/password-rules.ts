// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one check every "choose a password" form runs (signup, recovery with
 * a new password), and the sentence it shows. A strength meter is unbuilt;
 * this is a floor.
 */

import { t } from "../l10n";

/** The shortest password the forms accept. */
export const MIN_PASSWORD_LENGTH = 8;

/** `null` when the pair is acceptable, else the sentence to show. Length is
 *  checked before the match so a short password typed twice is told the
 *  useful thing. */
export function newPasswordProblem(password: string, confirm: string): string | null {
  if (password.length < MIN_PASSWORD_LENGTH) {
    return t("auth.password.error.too_short", { count: MIN_PASSWORD_LENGTH });
  }
  if (password !== confirm) {
    return t("auth.password.error.mismatch");
  }
  return null;
}
