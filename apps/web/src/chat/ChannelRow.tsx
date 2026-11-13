// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { channelAvatarKey } from "../channels/channel-avatar";
import { badgeText } from "../feed/feed-marks";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";
import type { ConversationRow } from "./conversation-list";

/**
 * A subscribed channel in the conversation list.
 *
 * Deliberately short: the list says nothing about which kind a row is, so
 * this draws the same avatar, title, time and second line as every other row. What
 * it does NOT have is the global pin / mute glyphs - those are keyed on a
 * conversation group id a channel does not carry (the pin shown is the one
 * inside the open folder). The badge is
 * the channel's count of new posts (docs/ux/feed.md section 5, "Channel
 * posts"): the posts newer than the newest one read, in the feed or in the
 * channel, drawn like a chat's unread badge.
 */
export function ChannelRow(props: {
  row: ConversationRow;
  /** The newest post's time in the chat-list form, or "" when nothing has
   *  been posted (`channelRowTime`). */
  time: string;
  /** Pinned inside the open folder (a channel has no global pin). */
  pinned: boolean;
  /** The channel is the one open beside the list. */
  selected: boolean;
  onMenu: (x: number, y: number) => void;
}): JSX.Element {
  const press = createLongPress((x, y) => props.onMenu(x, y));
  return (
    <A
      href={paths.channel(props.row.id)}
      class="lk-row"
      classList={{ selected: props.selected }}
      activeClass=""
      inactiveClass=""
      onContextMenu={(e) => {
        e.preventDefault();
        props.onMenu(e.clientX, e.clientY);
      }}
      onPointerDown={press.onPointerDown}
      onPointerMove={press.onPointerMove}
      onPointerUp={press.onPointerUp}
      onPointerCancel={press.onPointerCancel}
      onPointerLeave={press.onPointerLeave}
      onClick={(e) => press.swallowClick(e)}
    >
      <Avatar label={props.row.title} id={props.row.id} conversationKey={channelAvatarKey(props.row.id)} playVideo="hover" />
      <div class="lk-row-body">
        <div class="lk-row-line">
          <span class="lk-row-title" classList={{ unread: props.row.unread > 0 }}>
            <span class="lk-ellipsis">{props.row.title}</span>
          </span>
          {/* The newest post's time, where a group's row puts its last
              message's time. */}
          <Show when={props.time.length > 0}>
            <span class="lk-row-time" classList={{ unread: props.row.unread > 0 }}>
              {props.time}
            </span>
          </Show>
        </div>
        <div class="lk-row-line">
          <span class="lk-row-sub">{props.row.preview}</span>
          <span class="lk-row-trail">
            <Show when={props.pinned}>
              <Icon name="pin-fill" size={14} title={t("chats.row.pinned")} />
            </Show>
            <Show when={props.row.unread > 0}>
              <span class="lk-badge" title={t("channels.row.new_posts_a11y", { count: props.row.unread })}>
                {badgeText(props.row.unread)}
              </span>
            </Show>
          </span>
        </div>
      </div>
    </A>
  );
}
