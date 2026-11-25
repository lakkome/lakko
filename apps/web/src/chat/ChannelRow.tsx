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
 * this draws the same avatar, title, time and second line as every other row,
 * with the same pin / mute / archive glyphs and hover actions as a group's row
 * (`docs/ux/group-and-channel-privacy.md` section 12); those flags are the
 * synced flags keyed on the channel id (`channels/channel-flags.ts`). The
 * badge is
 * the channel's count of new posts (docs/ux/feed.md section 5, "Channel
 * posts"): the posts newer than the newest one read, in the feed or in the
 * channel, drawn like a chat's unread badge.
 */
export function ChannelRow(props: {
  row: ConversationRow;
  /** The newest post's time in the chat-list form, or "" when nothing has
   *  been posted (`channelRowTime`). */
  time: string;
  /** Pinned in the open tab: the folder's own pin inside a folder, the
   *  synced pin in All chats. */
  pinned: boolean;
  muted: boolean;
  archived: boolean;
  /** Draw the row as archived (dimmed, the archive glyph): false inside the
   *  archive view, where every row is archived (navigation.md 2.2 step 3).
   *  Defaults to `archived`. */
  markArchived?: boolean;
  /** Pin is disabled: the open folder already holds its most pins. */
  pinBlocked: boolean;
  /** The channel is the one open beside the list. */
  selected: boolean;
  onMenu: (x: number, y: number) => void;
  onToggle: (field: "pinned" | "muted" | "archived") => void;
}): JSX.Element {
  const press = createLongPress((x, y) => props.onMenu(x, y));
  return (
    <A
      href={paths.channel(props.row.id)}
      class="lk-row"
      classList={{ selected: props.selected, archived: props.markArchived ?? props.archived }}
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
            <Show when={props.muted}>
              <Icon name="bell-slash-fill" size={14} title={t("chats.row.muted")} />
            </Show>
            <Show when={props.pinned}>
              <Icon name="pin-fill" size={14} title={t("chats.row.pinned")} />
            </Show>
            <Show when={props.markArchived ?? props.archived}>
              <Icon name="archive-fill" size={14} />
            </Show>
            <Show when={props.row.unread > 0}>
              <span
                class="lk-badge"
                classList={{ muted: props.muted }}
                title={t("channels.row.new_posts_a11y", { count: props.row.unread })}
              >
                {badgeText(props.row.unread)}
              </span>
            </Show>
          </span>
          <span class="lk-row-actions" role="group" aria-label={t("chats.row.actions")}>
            <button
              type="button"
              title={props.pinned ? t("chats.action.unpin") : t("chats.action.pin")}
              aria-label={props.pinned ? t("chats.action.unpin") : t("chats.action.pin")}
              aria-pressed={props.pinned}
              disabled={props.pinBlocked}
              onClick={(e) => {
                e.preventDefault();
                props.onToggle("pinned");
              }}
            >
              <Icon name={props.pinned ? "pin-slash" : "pin"} size={20} />
            </button>
            <button
              type="button"
              title={props.muted ? t("chats.action.unmute") : t("chats.action.mute")}
              aria-label={props.muted ? t("chats.action.unmute") : t("chats.action.mute")}
              aria-pressed={props.muted}
              onClick={(e) => {
                e.preventDefault();
                props.onToggle("muted");
              }}
            >
              <Icon name={props.muted ? "bell" : "bell-slash"} size={20} />
            </button>
            <button
              type="button"
              title={props.archived ? t("chats.action.unarchive") : t("chats.action.archive")}
              aria-label={props.archived ? t("chats.action.unarchive") : t("chats.action.archive")}
              aria-pressed={props.archived}
              onClick={(e) => {
                e.preventDefault();
                props.onToggle("archived");
              }}
            >
              <Icon name={props.archived ? "unarchive" : "archive"} size={20} />
            </button>
          </span>
        </div>
      </div>
    </A>
  );
}
