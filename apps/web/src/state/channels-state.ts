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

import {
  bytesToHex,
  type ChannelPublisherKeys,
  type ChannelSubscriberKeys,
} from "../channels/composer";
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
  /** Per-channel publisher keys — local-only, never sent
   *  to the server. Holds the content-key seed + Ed25519
   *  signing-key seed the WASM composer needs. Present
   *  only on channels where the local user is a
   *  publisher. */
  readonly keysByChannel: Accessor<ReadonlyMap<string, ChannelPublisherKeys>>;
  /** Per-channel monotone publish counter. Bumped on
   *  every successful publish so the next post carries
   *  `message_counter = previous + 1`. Reset on epoch
   *  rotation. */
  readonly counterByChannel: Accessor<ReadonlyMap<string, bigint>>;
  /** Per-channel subscriber-side X25519 keypair the
   *  device generated for joining a channel as a
   *  reader (Phase 4b G4-3c). Persisted in localStorage
   *  under the `lakko.channels.subscriber-keys.v1` key so
   *  a page reload does not lose the secret half (and
   *  with it, access to undecoded posts on the channel). */
  readonly subscriberKeysByChannel: Accessor<
    ReadonlyMap<string, ChannelSubscriberKeys>
  >;
  /** Per-channel recovered content-key seeds, keyed by
   *  MLS epoch (ADR-0031 multi-epoch retention). The
   *  subscriber opens the server's retained set (up to
   *  N=4 most-recent epochs) into this `epoch -> seed`
   *  map, then decrypts each post with the seed matching
   *  the post's own `epoch_id`. An empty / absent inner
   *  map means no seed has been recovered yet — the UI
   *  shows a "waiting for seed delivery" state; a post
   *  whose epoch has no seed renders as an unreadable
   *  placeholder and is NEVER force-decrypted with a
   *  wrong-epoch seed (invariant #4). In-memory only —
   *  never persisted, never sent off-device. */
  readonly subscribedSeedsByChannel: Accessor<
    ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>
  >;
}

const [channels, setChannels] = createSignal<ReadonlyArray<ChannelHandle>>([]);
const [postsByChannel, setPostsByChannel] = createSignal<
  ReadonlyMap<string, ReadonlyArray<ChannelPostView>>
>(new Map());
const [epochByChannel, setEpochByChannel] = createSignal<
  ReadonlyMap<string, number>
>(new Map());
const [keysByChannel, setKeysByChannel] = createSignal<
  ReadonlyMap<string, ChannelPublisherKeys>
>(new Map());
const [counterByChannel, setCounterByChannel] = createSignal<
  ReadonlyMap<string, bigint>
>(new Map());
const [subscriberKeysByChannel, setSubscriberKeysByChannel] = createSignal<
  ReadonlyMap<string, ChannelSubscriberKeys>
>(loadSubscriberKeysFromStorage());
const [subscribedSeedsByChannel, setSubscribedSeedsByChannel] = createSignal<
  ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>
>(new Map());

export const channelsState: ChannelsStateInternal = {
  channels,
  postsByChannel,
  epochByChannel,
  keysByChannel,
  counterByChannel,
  subscriberKeysByChannel,
  subscribedSeedsByChannel,
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
    setKeysByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    setCounterByChannel((map) => {
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
  /** Install fresh publisher keys for a channel. Resets
   *  the publish counter to 0 — every (epoch, sender)
   *  pair starts a new counter sequence per
   *  `lakko_channels::post` invariants. */
  setKeys(channelId: string, keys: ChannelPublisherKeys): void {
    setKeysByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, keys);
      return next;
    });
    setCounterByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, 0n);
      return next;
    });
  },
  /** Atomically bump the publish counter for a channel
   *  and return the value the publisher should use for
   *  the NEXT post. Caller-side composition pattern:
   *
   *  ```ts
   *  const counter = channelsActions.nextCounter(id);
   *  const wire = await composePost(keys, id, epoch, counter, plaintext);
   *  await publishPost(id, wire);
   *  ```
   */
  nextCounter(channelId: string): bigint {
    let value = 0n;
    setCounterByChannel((map) => {
      const next = new Map(map);
      value = next.get(channelId) ?? 0n;
      next.set(channelId, value + 1n);
      return next;
    });
    return value;
  },
  /** Install / replace the subscriber-side X25519 keypair
   *  the device generated when joining `channelId` as a
   *  reader. Also persists to localStorage so a reload
   *  does not strand the user without the secret needed
   *  to open future envelopes. */
  setSubscriberKeys(channelId: string, keys: ChannelSubscriberKeys): void {
    setSubscriberKeysByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, keys);
      persistSubscriberKeys(next);
      return next;
    });
  },
  /** Forget the subscriber-side keypair for `channelId` —
   *  used on un-follow. Clears the recovered seed too,
   *  since it was only useful while the device had the
   *  matching secret half. */
  clearSubscriberKeys(channelId: string): void {
    setSubscriberKeysByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      persistSubscriberKeys(next);
      return next;
    });
    setSubscribedSeedsByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
  },
  /** Record the recovered content-key seed for a specific
   *  epoch after the subscriber opened that epoch's seed
   *  envelope (ADR-0031 multi-epoch retention). Idempotent
   *  per `(channelId, epoch)`. In-memory only — never
   *  persisted, never sent off-device. */
  setSubscribedSeedForEpoch(
    channelId: string,
    epoch: number,
    seed: Uint8Array,
  ): void {
    setSubscribedSeedsByChannel((map) => {
      const next = new Map(map);
      const inner = new Map(next.get(channelId) ?? []);
      inner.set(epoch, seed);
      next.set(channelId, inner);
      return next;
    });
  },
  /** Look up the recovered seed for a channel. With an
   *  `epoch` returns the seed for exactly that epoch (used
   *  by the decrypt-by-epoch path); without one returns the
   *  seed for the highest recovered epoch — the "current"
   *  seed a fresh publish/read expects. `undefined` when no
   *  seed (for that epoch) has been recovered. */
  currentSeed(channelId: string, epoch?: number): Uint8Array | undefined {
    const inner = subscribedSeedsByChannel().get(channelId);
    if (inner === undefined) return undefined;
    if (epoch !== undefined) return inner.get(epoch);
    let bestEpoch: number | undefined;
    for (const e of inner.keys()) {
      if (bestEpoch === undefined || e > bestEpoch) bestEpoch = e;
    }
    return bestEpoch === undefined ? undefined : inner.get(bestEpoch);
  },
  /** Wipe everything — used on logout. Drops subscriber
   *  keypairs from localStorage as well. */
  reset(): void {
    setChannels([]);
    setPostsByChannel(new Map());
    setEpochByChannel(new Map());
    setKeysByChannel(new Map());
    setCounterByChannel(new Map());
    setSubscriberKeysByChannel(new Map());
    setSubscribedSeedsByChannel(new Map());
    persistSubscriberKeys(new Map());
  },
};

// ---------------------------------------------------------
// localStorage persistence (subscriber keypairs only)
// ---------------------------------------------------------
//
// Publisher keys live in `keysByChannel` and are NOT
// persisted to localStorage yet — they will move into the
// encrypted-at-rest device-group-keyed sqlite cache in a
// follow-up storage sprint. Subscriber keys are persisted
// in the meantime because losing the secret half between
// reloads means losing access to every future post on the
// channel; the device-group cache rule still applies and
// the production rewrite will migrate this localStorage
// key into the same store.

const STORAGE_KEY = "lakko.channels.subscriber-keys.v1";

interface PersistedSubscriberKey {
  readonly channelId: string;
  /** Hex-encoded 32-byte X25519 secret. */
  readonly secretHex: string;
  /** Hex-encoded 32-byte X25519 public. */
  readonly publicHex: string;
}

function loadSubscriberKeysFromStorage(): ReadonlyMap<
  string,
  ChannelSubscriberKeys
> {
  if (typeof localStorage === "undefined") {
    return new Map();
  }
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) {
    return new Map();
  }
  try {
    const arr = JSON.parse(raw) as ReadonlyArray<PersistedSubscriberKey>;
    const out = new Map<string, ChannelSubscriberKeys>();
    for (const entry of arr) {
      out.set(entry.channelId, {
        secret: hexToBytes(entry.secretHex),
        public: hexToBytes(entry.publicHex),
      });
    }
    return out;
  } catch {
    // Corrupt JSON — drop the store rather than crashing
    // the app. The user can re-subscribe; the worst case is
    // they get stale envelopes from earlier rotations that
    // they can no longer open.
    return new Map();
  }
}

function persistSubscriberKeys(
  map: ReadonlyMap<string, ChannelSubscriberKeys>,
): void {
  if (typeof localStorage === "undefined") {
    return;
  }
  const arr: ReadonlyArray<PersistedSubscriberKey> = Array.from(
    map.entries(),
  ).map(([channelId, k]) => ({
    channelId,
    secretHex: bytesToHex(k.secret),
    publicHex: bytesToHex(k.public),
  }));
  localStorage.setItem(STORAGE_KEY, JSON.stringify(arr));
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
