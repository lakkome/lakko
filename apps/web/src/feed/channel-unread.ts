// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The count of new posts on a channel's row (`docs/ux/feed.md` section 5,
 * "Channel posts"), for the two lists that draw channel rows: the merged
 * conversation list and the channel pane.
 *
 * Reads the channel-post log (what this device has of each channel) and the
 * feed marks (the newest post read, in the feed or the channel), reloads on
 * a change to either, and counts with `channelUnreadCount`, the one rule.
 * Call inside a component; the listeners go with it.
 */

import { createSignal, onCleanup, onMount, type Accessor } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { channelsState } from "../state/channels-state";
import type { ChannelPostSlot } from "./channel-post-log";
import { loadChannelPostLog, subscribeChannelPostLogChanged } from "./channel-post-log-store";
import { channelUnreadCount, emptyFeedMarks, type FeedMarks } from "./feed-marks";
import { readFeedMarks, subscribeFeedMarksChanged } from "./feed-marks-store";

export function createChannelUnread(): Accessor<(channelIdHex: string) => number> {
  const auth = useAuth();
  const [posts, setPosts] = createSignal<{ readonly [hex: string]: ChannelPostSlot }>({});
  const [marks, setMarks] = createSignal<FeedMarks>(emptyFeedMarks());

  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  async function reloadPosts(): Promise<void> {
    const a = account();
    if (a === null) return;
    const key = await getDeviceKey(a);
    if (key === null) return;
    const log = await loadChannelPostLog(a, key);
    setPosts(log?.channels ?? {});
  }
  async function reloadMarks(): Promise<void> {
    const a = account();
    if (a === null) return;
    setMarks(await readFeedMarks(a));
  }

  onMount(() => {
    void reloadPosts().catch(() => undefined);
    void reloadMarks().catch(() => undefined);
    const offPosts = subscribeChannelPostLogChanged((a) => {
      if (a === account()) void reloadPosts().catch(() => undefined);
    });
    const offMarks = subscribeFeedMarksChanged((a) => {
      if (a === account()) void reloadMarks().catch(() => undefined);
    });
    onCleanup(() => {
      offPosts();
      offMarks();
    });
  });

  return () => {
    const p = posts();
    const m = marks();
    const own = channelsState.keysByChannel();
    return (hex: string): number =>
      channelUnreadCount(
        (p[hex]?.posts ?? []).map((x) => x.cursor),
        m.channelRead[hex],
        own.has(hex),
      );
  };
}
