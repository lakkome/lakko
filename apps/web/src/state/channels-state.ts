// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channels client-side state — Phase 4 G4-2.
 *
 * Stores the user's local channel list (display names +
 * descriptions are NEVER sent to the server — they live
 * only in this in-memory state and, in a future sprint,
 * an encrypted-at-rest device-group-keyed sqlite cache).
 *
 * The server-side state (posts, subscribers, current
 * epoch) is fetched lazily via `apps/web/src/channels/api.ts`
 * and cached here keyed by channel id.
 */

import { createSignal, type Accessor } from "solid-js";

import type { ChannelHandle, ChannelPostView } from "../channels/api";

interface ChannelsStateInternal {
  /** User's local channel list, ordered by `addedAtMs`
   *  descending (newest first). */
  readonly channels: Accessor<ReadonlyArray<ChannelHandle>>;
  /** Per-channel cached post views, keyed by channel id.
   *  Caches the most-recent page; clients re-fetch on
   *  open. */
  readonly postsByChannel: Accessor<
    ReadonlyMap<string, ReadonlyArray<ChannelPostView>>
  >;
  /** Per-channel cached server epoch. `undefined` when
   *  not yet fetched. */
  readonly epochByChannel: Accessor<ReadonlyMap<string, number>>;
}

const [channels, setChannels] = createSignal<ReadonlyArray<ChannelHandle>>([]);
const [postsByChannel, setPostsByChannel] = createSignal<
  ReadonlyMap<string, ReadonlyArray<ChannelPostView>>
>(new Map());
const [epochByChannel, setEpochByChannel] = createSignal<
  ReadonlyMap<string, number>
>(new Map());

export const channelsState: ChannelsStateInternal = {
  channels,
  postsByChannel,
  epochByChannel,
};

export const channelsActions = {
  /** Add a freshly-created or freshly-subscribed channel
   *  to the local list. Idempotent on channel id —
   *  re-adding updates the local name / description
   *  rather than appending a duplicate. */
  addChannel(handle: ChannelHandle): void {
    setChannels((list) => {
      const without = list.filter((c) => c.id !== handle.id);
      return [handle, ...without];
    });
  },
  /** Remove a channel from the local list. The server-
   *  side registry is unaffected — this is purely the
   *  user un-following on their own device. */
  removeChannel(channelId: string): void {
    setChannels((list) => list.filter((c) => c.id !== channelId));
    setPostsByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    setEpochByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
  },
  /** Replace the local name / description for a channel.
   *  No-op if the channel id is unknown. */
  updateLocalMeta(
    channelId: string,
    patch: Pick<Partial<ChannelHandle>, "localName" | "localDescription">,
  ): void {
    setChannels((list) =>
      list.map((c) => (c.id === channelId ? { ...c, ...patch } : c)),
    );
  },
  /** Replace the post cache for a channel after a fetch. */
  setPosts(channelId: string, posts: ReadonlyArray<ChannelPostView>): void {
    setPostsByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, posts);
      return next;
    });
  },
  /** Cache the current epoch the server returned. */
  setEpoch(channelId: string, epoch: number): void {
    setEpochByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, epoch);
      return next;
    });
  },
  /** Wipe everything — used on logout. */
  reset(): void {
    setChannels([]);
    setPostsByChannel(new Map());
    setEpochByChannel(new Map());
  },
};
