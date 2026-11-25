// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Chats destination: the list of one-to-one conversations on the left,
 * the open chat (or the empty hint) on the right. Wraps `/` and `/chat/:id`
 * and `/channel/:id` so the list stays mounted while the user moves between
 * conversations, channels included. On a
 * narrow screen one pane shows at a time: the list, or the open chat.
 *
 * Groups open here too (`/groups/:id`, `/groups/new`), so moving between a
 * chat and a group never unmounts the list.
 */
import type { JSX } from "solid-js";
import { useLocation, useParams, type RouteSectionProps } from "@solidjs/router";
import { ChatList } from "../chat/ChatList";
import { paths } from "./paths";

export default function ChatsLayout(props: RouteSectionProps): JSX.Element {
  const params = useParams<{ id?: string; hex?: string }>();
  // `hex`: the pending chat of ADR-0079 (`/invited/:hex`).
  const location = useLocation();
  // `/channel/:id` and `/channel/new` (ADR-0087 section 12) open beside the
  // same list.
  const open = (): boolean =>
    params.id !== undefined ||
    params.hex !== undefined ||
    location.pathname.startsWith("/channel/") ||
    location.pathname === paths.newGroup;
  return (
    <div class="lk-split lk-chats-split" data-open={open() ? "true" : "false"}>
      <ChatList activeGroupIdB64={params.id ?? null} />
      <div class="lk-main">{props.children}</div>
    </div>
  );
}
