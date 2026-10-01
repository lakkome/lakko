// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One row of the channel list pane: a 40 px initials avatar, the name, the
 * time of the last post on the right, the handle and subscriber count under
 * it. The same shape as a chat row (`chat/ChatList.tsx`), with the same
 * badge for the channel's count of new posts (`docs/ux/feed.md` section 5).
 */
import { Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { badgeText } from "../feed/feed-marks";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import type { ChannelRowModel } from "./channel-row";

export function ChannelRow(props: {
  readonly id: string;
  readonly href: string;
  readonly model: ChannelRowModel;
  readonly selected: boolean;
  /** Posts newer than the newest one read; 0 draws no badge. */
  readonly unread?: number;
}): JSX.Element {
  const unread = (): number => props.unread ?? 0;
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
          <span class="lk-row-title" classList={{ unread: unread() > 0 }}>
            <span class="lk-ellipsis">{props.model.title}</span>
          </span>
          <Show when={props.model.time.length > 0}>
            <span class="lk-row-time">{props.model.time}</span>
          </Show>
        </div>
        <div class="lk-row-line">
          <span class="lk-row-sub">{props.model.subtitle}</span>
          <Show when={unread() > 0}>
            <span class="lk-row-trail">
              <span class="lk-badge" title={t("channels.row.new_posts_a11y", { count: unread() })}>
                {badgeText(unread())}
              </span>
            </span>
          </Show>
        </div>
      </div>
    </A>
  );
}
