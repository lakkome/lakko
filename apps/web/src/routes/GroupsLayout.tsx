// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/groups` with nothing open. The list beside it is `ChatsLayout`'s: groups
 * open under the same layout as every other conversation, so the list stays
 * mounted (`routes.tsx`).
 */
import type { JSX } from "solid-js";
import { t } from "../l10n";

/** `/groups` with nothing open: one hint in the main pane. */
export function GroupsIndex(): JSX.Element {
  return (
    <div class="lk-main-center">
      <span class="lk-hint-pill">{t("chats.empty.select")}</span>
    </div>
  );
}
