// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Groups destination: the list of group conversations on the left, the
 * open group, the new-group form or the empty hint on the right. Wraps
 * `/groups`, `/groups/new` and `/groups/:id` so the list stays mounted; a
 * narrow screen shows one pane at a time.
 */
import type { JSX } from "solid-js";
import { useLocation, useParams, type RouteSectionProps } from "@solidjs/router";
import { ChatList } from "../chat/ChatList";
import { t } from "../l10n";
import { paths } from "./paths";

export default function GroupsLayout(props: RouteSectionProps): JSX.Element {
  const params = useParams<{ id?: string }>();
  const location = useLocation();
  const open = (): boolean => params.id !== undefined || location.pathname === paths.newGroup;
  return (
    <div class="lk-split" data-open={open() ? "true" : "false"}>
      <ChatList activeGroupIdB64={params.id ?? null} />
      <div class="lk-main">{props.children}</div>
    </div>
  );
}

/** `/groups` with nothing open: one hint in the main pane. */
export function GroupsIndex(): JSX.Element {
  return (
    <div class="lk-main-center">
      <span class="lk-hint-pill">{t("chats.empty.select")}</span>
    </div>
  );
}
