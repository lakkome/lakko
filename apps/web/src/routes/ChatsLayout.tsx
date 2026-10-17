// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The Chats destination: the list of one-to-one conversations on the left,
 * the open chat (or the empty hint) on the right. Wraps `/` and `/chat/:id`
 * so the list stays mounted while the user moves between conversations. On a
 * narrow screen one pane shows at a time: the list, or the open chat.
 *
 * `GroupsLayout` is the same shell over the group conversations.
 */
import type { JSX } from "solid-js";
import { useParams, type RouteSectionProps } from "@solidjs/router";
import { ChatList } from "../chat/ChatList";

export default function ChatsLayout(props: RouteSectionProps): JSX.Element {
  const params = useParams<{ id?: string; hex?: string }>();
  // `hex`: the pending chat of ADR-0079 (`/invited/:hex`).
  const open = (): boolean => params.id !== undefined || params.hex !== undefined;
  return (
    <div class="lk-split" data-open={open() ? "true" : "false"}>
      <ChatList activeGroupIdB64={params.id ?? null} />
      <div class="lk-main">{props.children}</div>
    </div>
  );
}
