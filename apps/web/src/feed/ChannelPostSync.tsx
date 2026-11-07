// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channels join the foreground poll (ADR-0063 section 4): while the app is
 * in front, the posts of every channel this device can read are fetched,
 * decrypted and filed into the channel-post log the feed ranks.
 *
 * Renders nothing; mounted once in `App.tsx` beside `ChannelsHydration`.
 * A channel post is not a conversation turn, so the cadence is a minute
 * (chosen), not the chat's long poll; a sync also runs as soon as the
 * channel state is hydrated and whenever the tab comes back into view. A
 * hidden tab does not sync. When ADR-0055's socket exists its notices
 * replace the timer.
 *
 * Only one sync runs at a time per page, and it is the only writer of the
 * log, so there is no merge between writers. A second tab may run its own;
 * both write a whole log built from the server, so the later write is
 * complete, never a mix.
 *
 * A post is opened under the seed of its own epoch, never another (ADR-0031,
 * invariant #4). A post whose epoch is newer than every seed this device
 * holds stops the cursor, so it is tried again once the seed arrives; an
 * older one without a seed is past the retained window and is passed over.
 */

import { createEffect, onCleanup, onMount, type Component } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { fetchPosts } from "../channels/api";
import { foldChannelMeta } from "../channels/channel-meta";
import { decodeAndDecryptPost } from "../channels/composer";
import { toPostDecode, type PostDecode } from "../channels/post-decode";
import { parseBodyEnvelopeWith } from "../chat/attachments";
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import {
  emptyChannelPostLog,
  retainChannels,
  syncChannelSlot,
  type ChannelPostLog,
  type PostOutcome,
} from "./channel-post-log";
import { seedChannelReads } from "./feed-marks";
import { updateFeedMarks } from "./feed-marks-store";
import type { CheckTally } from "./feed-refresh";
import { reportChannelCheck } from "./feed-refresh-store";
import {
  loadChannelPostLog,
  publishChannelPostLogChanged,
  saveChannelPostLog,
} from "./channel-post-log-store";

/** Between foreground syncs (chosen). */
const SYNC_INTERVAL_MS = 60_000;

/** Callbacks of the mounted sync, for {@link requestChannelPostSync}. */
const syncRequests = new Set<() => void>();

/**
 * Sync now rather than at the next tick. Called after this device publishes
 * a post, so the author sees it in the feed at once instead of up to a
 * minute later. A no-op while no sync is mounted.
 */
export function requestChannelPostSync(): void {
  for (const f of syncRequests) f();
}

interface ReadableChannel {
  readonly idHex: string;
  readonly seedForEpoch: (epoch: number) => Uint8Array | undefined;
  /** Newest epoch a seed is held for; a post above it waits. */
  readonly newestSeedEpoch: number;
}

/** Every channel this device can open posts of, and how. */
function readableChannels(): ReadableChannel[] {
  const out: ReadableChannel[] = [];
  const own = channelsState.keysByChannel();
  const seeds = channelsState.subscribedSeedsByChannel();
  for (const c of channelsState.channels()) {
    const keys = own.get(c.id);
    if (keys !== undefined) {
      // A publisher holds the current content seed only, as the channel
      // view does; a post it cannot open under it is passed over.
      const seed = keys.seed;
      out.push({ idHex: c.id, seedForEpoch: () => seed, newestSeedEpoch: Number.MAX_SAFE_INTEGER });
      continue;
    }
    const map = seeds.get(c.id);
    if (map === undefined || map.size === 0) continue;
    out.push({
      idHex: c.id,
      seedForEpoch: (epoch) => map.get(epoch),
      newestSeedEpoch: Math.max(...map.keys()),
    });
  }
  return out;
}

/**
 * Syncs every readable channel; returns how many were tried and how many
 * the server answered (`feed-refresh.ts`), or `null` when nothing ran.
 *
 * The channel state it reads and folds into (`channelsState`) is the
 * signed-in account's and is swapped on sign-out, while this runs for a
 * minute's worth of fetches. `isCurrent` says whether `accountIdB64` is still
 * the session the run was started for; it is asked after every await that
 * precedes a read of that state or a write, and a run that is no longer
 * current stops with `null`: it folds no metadata into the next account's
 * channels, reports no tally to its feed, and writes no row (after Forget
 * device that write would re-create the row the wipe removed).
 */
export async function syncAll(
  accountIdB64: string,
  isCurrent: () => boolean = () => true,
): Promise<CheckTally | null> {
  const deviceKey = await getDeviceKey(accountIdB64);
  if (deviceKey === null || !isCurrent()) return null;
  const wasm = await loadWasm();
  if (!isCurrent()) return null;
  const channels = readableChannels();
  const before = (await loadChannelPostLog(accountIdB64, deviceKey)) ?? emptyChannelPostLog();
  // `channelsState.channels()` below is read for the retain: after a change
  // of account it would prune this account's log down to the other's list.
  if (!isCurrent()) return null;
  let log: ChannelPostLog = retainChannels(
    before,
    new Set(channelsState.channels().map((c) => c.id)),
  );
  let changed = Object.keys(log.channels).length !== Object.keys(before.channels).length;
  let answered = 0;

  for (const channel of channels) {
    const metas: PostDecode[] = [];
    const decode = async (idHex: string, payloadB64: string): Promise<PostOutcome> => {
      const res = await decodeAndDecryptPost(idHex, payloadB64, channel.seedForEpoch);
      if (!res.hadSeed) {
        return res.epochId > channel.newestSeedEpoch ? { kind: "wait" } : { kind: "skip" };
      }
      // ADR-0087 section 13 — a post's plaintext is the chat envelope: the
      // feed files its text; an edit or delete op, a ban list and a record
      // are not posts.
      const d = toPostDecode(
        res,
        (bytes) => wasm.classifyChannelPostPlaintext(bytes),
        (bytes) => parseBodyEnvelopeWith(wasm, bytes),
      );
      if (d.kind === "meta") metas.push(d);
      return d.kind === "text" ? { kind: "text", text: d.text } : { kind: "skip" };
    };
    const slot = log.channels[channel.idHex];
    try {
      const next = await syncChannelSlot(
        channel.idHex,
        slot,
        { fetchPage: (id, after, limit) => fetchPosts(id, after, limit), decode },
        Date.now(),
      );
      answered += 1;
      if (next.cursor !== slot?.cursor || next.posts.length !== slot.posts.length) {
        log = { version: log.version, channels: { ...log.channels, [channel.idHex]: next } };
        changed = true;
      }
    } catch (err) {
      // A channel the server does not answer for this time; the next sync
      // tries again from the same cursor.
      // eslint-disable-next-line no-console
      console.warn("[feed] channel post sync failed", err);
    }
    if (!isCurrent()) return null;
    // A metadata record read here names the channel in the feed and the
    // list without opening it (ADR-0051).
    if (metas.length > 0) {
      await foldChannelMeta(channel.idHex, metas);
      if (!isCurrent()) return null;
    }
  }

  if (!isCurrent()) return null;
  // docs/ux/feed.md section 5 — a channel's first slot marks what it held
  // at subscription as read, so only posts arriving afterwards count as new.
  // Idempotent: a channel already marked is left alone and nothing is written.
  const slots = log.channels;
  await updateFeedMarks(accountIdB64, (m) => seedChannelReads(m, slots)).catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("[feed] seeding channel read markers failed", err);
  });

  if (!isCurrent()) return null;
  const tally: CheckTally = { tried: channels.length, answered };
  if (!changed) return tally;
  await saveChannelPostLog(accountIdB64, deviceKey, log);
  if (!isCurrent()) return null;
  publishChannelPostLogChanged(accountIdB64);
  return tally;
}

export const ChannelPostSync: Component = () => {
  const auth = useAuth();
  let running: Promise<void> | null = null;
  let again = false;
  // Monotone token, as in `channels/ChannelsHydration.tsx`: bumped whenever
  // the signed-in account changes, so a run started for one session stops
  // writing once that session is over (sign-out and back in included).
  let generation = 0;
  let bound: string | null = null;

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const run = (): void => {
    const account = accountId();
    if (account === null || !channelsState.hydrated()) return;
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
    if (running !== null) {
      again = true;
      return;
    }
    const mine = generation;
    running = syncAll(account, () => mine === generation && accountId() === account)
      .then((tally) => {
        // docs/ux/feed.md — the feed's "Couldn't check for new messages."
        if (tally !== null) reportChannelCheck(tally);
      })
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[feed] channel post sync error", err);
      })
      .finally(() => {
        running = null;
        if (again) {
          again = false;
          run();
        }
      });
  };

  // As soon as the channel state is there for a session.
  createEffect(() => {
    const account = accountId();
    if (account !== bound) {
      bound = account;
      generation += 1;
    }
    if (account !== null && channelsState.hydrated()) run();
  });

  onMount(() => {
    const timer = setInterval(run, SYNC_INTERVAL_MS);
    const onVisible = (): void => {
      if (document.visibilityState === "visible") run();
    };
    document.addEventListener("visibilitychange", onVisible);
    syncRequests.add(run);
    onCleanup(() => {
      syncRequests.delete(run);
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  return null;
};
