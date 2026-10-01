// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/` inside the Chats layout: nothing is open yet, so the main pane carries
 * one hint. The list itself is `chat/ChatList.tsx`, rendered by
 * `ChatsLayout`.
 */
import type { JSX } from "solid-js";
import { t } from "../l10n";

export default function Home(): JSX.Element {
  return (
    <div class="lk-main-center">
      <span class="lk-hint-pill">{t("chats.empty.select")}</span>
    </div>
  );
}
