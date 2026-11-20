// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Read-only mode of a suspended account (ADR-0099 step 3, UX
 * `reporting-and-moderation.md` section 4), after "Read my chats":
 *
 *   - `SuspendedComposerLine` stands in every composer's slot: one muted
 *     line, "Your account is suspended", and no send control. Static text in
 *     the place the person types, so a screen reader reaching it hears why;
 *   - `SuspendedBanner` sits at the top of the chat list and brings the
 *     suspended screen back (the reason, the appeal, sign out).
 *
 * Both render nothing outside suspended mode.
 */

import { Show, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { accountSuspended } from "./suspension-state";
import { Icon } from "../ui/icons";
import { t } from "../l10n";

/** The line in place of a composer while the account is suspended. */
export function SuspendedComposerLine(): JSX.Element {
  return (
    <Show when={accountSuspended()}>
      <div class="lk-suspended-line">{t("suspended.self.composer")}</div>
    </Show>
  );
}

/** The row at the top of the chat list that shows the suspended screen. */
export function SuspendedBanner(): JSX.Element {
  const auth = useAuth();
  return (
    <Show when={accountSuspended() && auth.suspension()?.reading === true}>
      <button type="button" class="lk-suspended-banner" onClick={() => auth.showSuspensionScreen()}>
        <Icon name="info-circle" size={18} />
        <span>{t("suspended.self.title")}</span>
        <Icon name="chevron-forward" size={16} />
      </button>
    </Show>
  );
}
