// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One row of the channel list pane: a 40 px initials avatar, the name, the
 * time of the last post on the right, the handle and subscriber count under
 * it. The same shape as a chat row (`chat/ChatList.tsx`).
 */
import { Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { Avatar } from "../ui/Avatar";
import type { ChannelRowModel } from "./channel-row";

export function ChannelRow(props: {
  readonly id: string;
  readonly href: string;
  readonly model: ChannelRowModel;
  readonly selected: boolean;
}): JSX.Element {
  return (
    <A
      href={props.href}
      class="lk-row"
      classList={{ selected: props.selected }}
      activeClass=""
      inactiveClass=""
    >
      <Avatar label={props.model.title} id={props.id} />
      <div class="lk-row-body">
        <div class="lk-row-line">
          <span class="lk-row-title">
            <span class="lk-ellipsis">{props.model.title}</span>
          </span>
          <Show when={props.model.time.length > 0}>
            <span class="lk-row-time">{props.model.time}</span>
          </Show>
        </div>
        <div class="lk-row-line">
          <span class="lk-row-sub">{props.model.subtitle}</span>
        </div>
      </div>
    </A>
  );
}
