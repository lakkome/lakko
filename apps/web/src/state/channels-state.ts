// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channels client-side state — Phase 4 G4-2.
 *
 * Stores the user's local channel list (display names +
 * descriptions are NEVER sent to the server), the reader-side
 * keys + recovered seeds, and the publisher keys of the channels
 * this device posts to. All of it persists per account in the
 * encrypted `channel-log-store.ts` blob (device-root-key AES-GCM
 * in IndexedDB) and is hydrated once per session by
 * `channels/ChannelsHydration.tsx`; every mutation below writes
 * through (coalesced, serialised).
 *
 * The server-side state (posts, subscribers, current epoch) is
 * fetched lazily via `apps/web/src/channels/api.ts` and cached
 * here keyed by channel id; it is not persisted.
 *
 * ## The PublisherGroup lives in the ACCOUNT identity's provider
 *
 * The channel content seed is an MLS exporter over a persistent
 * `PublisherGroup` — persistent in the OpenMLS provider of whichever
 * `Identity` created it. Every web call site used to build a
 * throwaway `Identity.fromSeed(...)` and free it, so the group (and
 * with it the seed every subscriber holds) was re-minted on the next
 * open. The channel paths now use the session-long account identity
 * (`useChats().accountIdentity()`), and every flush of this store
 * captures that identity's `snapshotStorage()` into the blob; the
 * hydration restores it before re-opening any publisher keys.
 */

import type { DeviceCipher } from "../auth/device-cipher";
import { createSignal, untrack, type Accessor } from "solid-js";
import {
  forgetOrphanedByReset,
  loadOrphanedByReset,
  recordOrphanedByReset,
} from "../channels/orphaned-by-reset";

import type { ChannelPublisherKeys, ChannelSubscriberKeys, WasmIdentity } from "../channels/composer";
import type { ChannelHandle, ChannelPostView } from "../channels/api";
import {
  decodeChannelLog,
  emptyChannelLogSnapshot,
  encodeChannelLog,
  type ChannelLogSnapshot,
  type ChannelSharedMeta,
} from "../channels/channel-log-codec";
import { deleteChannelLog, loadChannelLog, saveChannelLog } from "../channels/channel-log-store";
import { deleteChannelPostLog } from "../feed/channel-post-log-store";
import { channelIdFromHex } from "../channels/composer";
import {
  clearPublishCountersForAccount,
  issueAndAdvance,
  loadAllCounters,
  seedCounter,
} from "../channels/publish-counter-store";
import { base64Encode } from "../auth/account-id";
import { registerAccountMemory } from "../auth/account-memory";
import { loadWasm } from "../wasm";
import type { PublisherCommitEntry } from "../channels/publisher-commit-outbox";
import { emptyAccessRecord, type ChannelAccessRecord } from "../channels/channel-access-record";

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
   *  publisher. The signing seed is persisted; the content
   *  seed is re-derived at hydration from the restored
   *  PublisherGroup. */
  readonly keysByChannel: Accessor<ReadonlyMap<string, ChannelPublisherKeys>>;
  /** ADR-0073 phase 2 (UX 16.6): the channels this device owned under
   *  account keys that were since reset — their publisher state is in the
   *  log, but the provider it lives in was signed by the previous identity,
   *  so no key can be re-opened and no post, rotation or subscriber change
   *  is possible any more. Derived at every hydration from that mismatch;
   *  never persisted. */
  readonly orphanedByKeyReset: Accessor<ReadonlySet<string>>;
  /** Per-channel monotone publish counter. Bumped on
   *  every successful publish so the next post carries
   *  `message_counter = previous + 1`. FOREVER monotone —
   *  NOT reset on epoch rotation (the `epoch_id` is folded
   *  into the post AEAD's HKDF, so a rotation already
   *  disambiguates the key + nonce). Account data: loaded
   *  from the session account's persisted floors when its
   *  channels session hydrates (`publish-counter-store.ts`),
   *  written through on every `nextCounter`, and dropped from
   *  memory when the session ends, so a page reload resumes
   *  the floor instead of rewinding to 0 under the
   *  reload-stable channel seed and sender key, and another
   *  account signed in next never starts from this one's
   *  floor. */
  readonly counterByChannel: Accessor<ReadonlyMap<string, bigint>>;
  /** Per-channel subscriber-side Ed25519 keypair the
   *  device generated for joining a channel as a
   *  reader (Phase 4b G4-3c; Ed25519 since ADR-0060, so it
   *  can sign member-card requests - a publisher holds one
   *  too, for its own member card). Persisted in the encrypted
   *  channel log so a page reload does not lose the secret
   *  half (and with it, access to undecoded posts on the
   *  channel). */
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
   *  wrong-epoch seed (invariant #4). Persisted in the
   *  encrypted channel log; never sent off-device. */
  readonly subscribedSeedsByChannel: Accessor<
    ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>
  >;
  /** ADR-0051 §4 — per-channel newest shared metadata record
   *  (name, description, stamp): what a subscriber titles the
   *  channel from, and a publisher's own copy, the source of the
   *  next record it posts. Persisted in the encrypted channel
   *  log. */
  readonly metaByChannel: Accessor<ReadonlyMap<string, ChannelSharedMeta>>;
  /** `true` once the per-account hydration has settled for
   *  the current session (whether or not a row existed).
   *  Routes that mint keys on mount await
   *  `awaitChannelsHydration()` so they do not race a
   *  reload's restore. */
  readonly hydrated: Accessor<boolean>;
  /** ADR-0087 phase B — per-channel access record (visibility, @name,
   *  photo, read key, waiting, admission seeds, links, publishers).
   *  Persisted in the encrypted channel log. */
  readonly accessByChannel: Accessor<ReadonlyMap<string, ChannelAccessRecord>>;
}

/** Plaintext `localStorage` key the subscriber keypairs used to live
 *  under. Removed on hydration — nothing is deployed to users, so the
 *  old value is not migrated, only wiped so a plaintext secret does
 *  not linger. */
const LEGACY_SUBSCRIBER_KEYS_STORAGE_KEY = "lakko.channels.subscriber-keys.v1";

const [channels, setChannels] = createSignal<ReadonlyArray<ChannelHandle>>([]);
const [postsByChannel, setPostsByChannel] = createSignal<
  ReadonlyMap<string, ReadonlyArray<ChannelPostView>>
>(new Map());
const [epochByChannel, setEpochByChannel] = createSignal<
  ReadonlyMap<string, number>
>(new Map());
const [orphanedByKeyReset, setOrphanedByKeyReset] = createSignal<ReadonlySet<string>>(new Set());
const [keysByChannel, setKeysByChannel] = createSignal<
  ReadonlyMap<string, ChannelPublisherKeys>
>(new Map());
const [counterByChannel, setCounterByChannel] = createSignal<
  ReadonlyMap<string, bigint>
>(new Map());
const [subscriberKeysByChannel, setSubscriberKeysByChannel] = createSignal<
  ReadonlyMap<string, ChannelSubscriberKeys>
>(new Map());
const [subscribedSeedsByChannel, setSubscribedSeedsByChannel] = createSignal<
  ReadonlyMap<string, ReadonlyMap<number, Uint8Array>>
>(new Map());
const [metaByChannel, setMetaByChannel] = createSignal<
  ReadonlyMap<string, ChannelSharedMeta>
>(new Map());
const [hydrated, setHydrated] = createSignal(false);
const [accessByChannel, setAccessByChannel] = createSignal<ReadonlyMap<string, ChannelAccessRecord>>(new Map());
/** ADR-0078 section 6 — co-published channel id -> PublisherGroup id hex. */
const [coPublisherGroupIds, setCoPublisherGroupIds] = createSignal<ReadonlyMap<string, string>>(new Map());
/** G-50 — the PublisherGroup commits whose steps are still owed. */
const [publisherCommits, setPublisherCommits] = createSignal<readonly PublisherCommitEntry[]>([]);
/**
 * G-50 — a durable write of a PublisherGroup commit was refused, so the
 * account identity's provider holds an N+1 that is not on disk with its
 * record. Every later channel-log write is refused for the rest of the
 * session: a debounced flush must never make that N+1 durable WITHOUT its
 * record (the device-ahead case). A reload rehydrates the durable N. Set
 * only by {@link channelsActions.recordPublisherCommit}.
 */
let poisoned = false;

export const channelsState: ChannelsStateInternal = {
  channels,
  postsByChannel,
  epochByChannel,
  keysByChannel,
  orphanedByKeyReset,
  counterByChannel,
  subscriberKeysByChannel,
  subscribedSeedsByChannel,
  metaByChannel,
  hydrated,
  accessByChannel,
};

// ---------------------------------------------------------
// Persistence sink
// ---------------------------------------------------------

/** What a flush needs: where to write, the wrap key, and a way to read
 *  the account identity's provider snapshot at flush time. */
interface PersistSink {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  /** `Identity.snapshotStorage()` of the account identity, or `null`
   *  when it is not available (logged out mid-flush). */
  readonly snapshotProvider: () => Uint8Array | null;
  /** base64 of the account identity's signature public key. */
  readonly leafKeyB64: string;
}

let sink: PersistSink | null = null;
/** The account whose publish counters the in-memory map holds, set when
 *  its hydration starts and cleared with the session. `null` falls back
 *  to the signed-in account bound in `contacts/local-store.ts`. */
let counterOwner: string | null = null;
/** The last provider snapshot this session captured or restored. Reused
 *  when `snapshotProvider` yields `null` so a flush that races a logout
 *  never overwrites a good snapshot with nothing. */
let lastProviderSnapshot: Uint8Array | null = null;
let lastProviderLeafKeyB64: string | null = null;
let flushTimer: ReturnType<typeof setTimeout> | null = null;
let flushChain: Promise<void> = Promise.resolve();
/** Debounce window for coalescing a burst of mutations into one write. */
const FLUSH_DELAY_MS = 150;

/** Capture the persisted view of the in-memory state, synchronously. */
function currentSnapshot(
  s: PersistSink,
  provider: Uint8Array | null = s.snapshotProvider(),
): ChannelLogSnapshot {
  const signing = new Map<string, Uint8Array>();
  for (const [id, k] of keysByChannel()) signing.set(id, k.signingKeySeed);
  if (provider !== null) {
    lastProviderSnapshot = provider;
    lastProviderLeafKeyB64 = s.leafKeyB64;
  }
  return {
    channels: channels(),
    subscriberKeys: subscriberKeysByChannel(),
    subscribedSeeds: subscribedSeedsByChannel(),
    publisherSigningSeeds: signing,
    channelMeta: metaByChannel(),
    publisherProviderSnapshot: lastProviderSnapshot,
    publisherLeafKeyB64: lastProviderSnapshot === null ? null : lastProviderLeafKeyB64,
    coPublisherGroupIds: coPublisherGroupIds(),
    publisherCommits: publisherCommits(),
    channelAccess: accessByChannel(),
  };
}

/** Write the current state now (serialised behind any in-flight write).
 *  The snapshot is taken synchronously at call time. */
function flushNow(): Promise<void> {
  const s = sink;
  if (s === null || poisoned) return Promise.resolve();
  const payload = encodeChannelLog(currentSnapshot(s));
  const run = flushChain.then(() => saveChannelLog(s.accountIdB64, s.deviceKey, payload));
  flushChain = run.catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("channel log persist failed", err);
  });
  return flushChain;
}

/** Coalesce mutations into one write per `FLUSH_DELAY_MS`. No-op until
 *  the hydration arms the sink — mutations before that are picked up by
 *  the hydration's own merge + write. */
function schedulePersist(): void {
  if (sink === null || flushTimer !== null) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    void flushNow();
  }, FLUSH_DELAY_MS);
}

/** The channel log could not be written durably: no session armed the
 *  store (signed out, no device key), or the account identity it snapshots
 *  is gone. */
export class ChannelLogUnavailableError extends Error {
  override name = "ChannelLogUnavailableError";
  constructor(reason: string) {
    super(`channel log not persisted: ${reason}`);
  }
}

/**
 * Write the current state now, FAIL-CLOSED: resolves only once the row,
 * carrying a provider snapshot taken now, is on disk, and rejects otherwise
 * (no sink, no live account identity, the account's stores sealed, an
 * IndexedDB failure). Unlike {@link flushNow} it never falls back to the last
 * good provider snapshot: a caller that publishes after it needs THIS
 * provider state recorded. Serialised behind any in-flight write; a pending
 * debounced write is folded in.
 */
function flushDurable(): Promise<void> {
  const s = sink;
  if (s === null) return Promise.reject(new ChannelLogUnavailableError("no session"));
  if (poisoned) {
    return Promise.reject(new ChannelLogUnavailableError("a PublisherGroup commit was not recorded; reload"));
  }
  const provider = s.snapshotProvider();
  if (provider === null) {
    return Promise.reject(new ChannelLogUnavailableError("no account identity"));
  }
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flushTimer = null;
  }
  const payload = encodeChannelLog(currentSnapshot(s, provider));
  const run = flushChain.then(() =>
    saveChannelLog(s.accountIdB64, s.deviceKey, payload, { durable: true }),
  );
  flushChain = run.catch((err: unknown) => {
    // eslint-disable-next-line no-console
    console.warn("channel log persist failed", err);
  });
  return run;
}

/** Flush a pending debounced write immediately, if any. */
function flushPending(): Promise<void> {
  if (flushTimer === null) return Promise.resolve();
  clearTimeout(flushTimer);
  flushTimer = null;
  return flushNow();
}

// ---------------------------------------------------------
// Hydration
// ---------------------------------------------------------

let hydrationWaiters: Array<() => void> = [];

/** Resolves once the current session's hydration has settled (or at
 *  once when it already has). */
export function awaitChannelsHydration(): Promise<void> {
  if (hydrated()) return Promise.resolve();
  return new Promise((resolve) => {
    hydrationWaiters.push(resolve);
  });
}

function settleHydration(): void {
  setHydrated(true);
  const waiters = hydrationWaiters;
  hydrationWaiters = [];
  for (const w of waiters) w();
}

export interface HydrateChannelsArgs {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  /** The session-long ACCOUNT identity the PublisherGroups live in.
   *  Its provider is restored from the blob and re-snapshotted on every
   *  flush; it must be the same instance the channel routes use. */
  readonly identity: WasmIdentity;
  /** Reads the account identity's provider at flush time; `null` when
   *  the session has ended. Defaults to snapshotting `identity`. */
  readonly snapshotProvider?: () => Uint8Array | null;
  /** `false` once the session this hydration belongs to has ended (a
   *  logout / account switch during the load). A stale hydration must
   *  not arm write-through for the old account, or its late flush would
   *  put the new session's (empty) state under the old account's row.
   *  Defaults to always current. */
  readonly isCurrent?: () => boolean;
}

function isStale(args: HydrateChannelsArgs): boolean {
  return args.isCurrent !== undefined && !args.isCurrent();
}

function releaseWaiters(): void {
  const waiters = hydrationWaiters;
  hydrationWaiters = [];
  for (const w of waiters) w();
}

/**
 * Load the per-account channel log and publish counters, restore the PublisherGroup provider
 * into `identity`, fill the in-memory state and arm write-through.
 *
 * Merges rather than replaces: a mutation that landed before this
 * settled (in-memory) wins over the stored row on the same id, and the
 * union is written back. For every persisted signing seed the publisher
 * keys are re-opened via `PublisherGroup.load` — never `openOrCreate`:
 * if the restored provider does not hold the group, creating one would
 * silently fork the channel onto a seed no subscriber has, so the seed
 * is dropped and the channel shows as not publishable instead.
 *
 * Never throws; a failure logs and settles the hydration so routes do
 * not hang on `awaitChannelsHydration()`.
 */
export async function hydrateChannelsState(args: HydrateChannelsArgs): Promise<void> {
  const leafKeyB64 = base64Encode(args.identity.signaturePublicKey);
  let stored: ChannelLogSnapshot = emptyChannelLogSnapshot();
  try {
    const payload = await loadChannelLog(args.accountIdB64, args.deviceKey);
    const decoded = payload === null ? null : decodeChannelLog(payload);
    if (decoded !== null) stored = decoded;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("channel log load failed; starting empty", err);
  }

  // A hydration whose session ended while the row was loading touches no
  // in-memory state: a logout or account switch has already cleared it,
  // and filling it now would hand the old account's channels, publisher
  // keys and publish counters to the next session. It arms nothing and
  // writes nothing; it only releases anyone still waiting on it.
  if (isStale(args)) {
    releaseWaiters();
    return;
  }
  counterOwner = args.accountIdB64;

  // 1. The provider snapshot, gated on the leaf key (mirrors
  //    `chat/rehydrate.ts`): a snapshot from another account identity
  //    holds groups this key did not sign for, and `restoreStorage`
  //    replaces the whole provider.
  let providerRestored = false;
  setOrphanedByKeyReset(loadOrphanedByReset(args.accountIdB64));
  if (stored.publisherProviderSnapshot !== null) {
    if (stored.publisherLeafKeyB64 !== leafKeyB64) {
      // eslint-disable-next-line no-console
      console.warn("channel log: provider snapshot belongs to another identity; dropped");
      // ADR-0073 phase 2 (UX 16.6) — the one way this device learns which
      // channels it owned under the OLD keys: the publisher state is here,
      // under an identity this account no longer has. Recorded durably
      // (`channels/orphaned-by-reset.ts`): the next flush replaces this
      // snapshot. Nothing is deleted; the person removes each from the
      // channel's own screen.
      setOrphanedByKeyReset(
        recordOrphanedByReset(args.accountIdB64, stored.publisherSigningSeeds.keys()),
      );
    } else {
      setOrphanedByKeyReset(loadOrphanedByReset(args.accountIdB64));
      // Keep the stored bytes as the best-known snapshot whether or not
      // the restore succeeds: a later flush that cannot read the provider
      // must re-write THESE, never `null` over them.
      lastProviderSnapshot = stored.publisherProviderSnapshot;
      lastProviderLeafKeyB64 = leafKeyB64;
      try {
        args.identity.restoreStorage(stored.publisherProviderSnapshot);
        providerRestored = true;
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("channel log: provider snapshot restore failed; publisher keys not re-opened", err);
      }
    }
  }

  // 2. Merge the plain sections (in-memory wins on a clash).
  const mem = channels();
  const memIds = new Set(mem.map((c) => c.id));
  const mergedChannels = [...mem, ...stored.channels.filter((c) => !memIds.has(c.id))].sort(
    (a, b) => b.addedAtMs - a.addedAtMs,
  );
  setChannels(mergedChannels);
  setSubscriberKeysByChannel((map) => new Map([...stored.subscriberKeys, ...map]));
  setSubscribedSeedsByChannel((map) => {
    const next = new Map<string, ReadonlyMap<number, Uint8Array>>(stored.subscribedSeeds);
    for (const [id, inner] of map) {
      next.set(id, new Map([...(next.get(id) ?? []), ...inner]));
    }
    return next;
  });
  // ADR-0051 — two copies of our own bookkeeping, not a receive: the one
  // with the greater stamp stays (a plain compare; the receive path uses the
  // core's `channelMetaIsNewer`, which is the same strict rule).
  setMetaByChannel((map) => {
    const next = new Map<string, ChannelSharedMeta>(stored.channelMeta);
    for (const [id, m] of map) {
      const held = next.get(id);
      if (held === undefined || m.updatedAtMs > held.updatedAtMs) next.set(id, m);
    }
    return next;
  });
  setCoPublisherGroupIds((map) => new Map([...stored.coPublisherGroupIds, ...map]));
  setAccessByChannel((map) => new Map([...(stored.channelAccess ?? new Map()), ...map]));
  // G-50 — the stored entries belong to the restored provider; one already
  // in memory (recorded before this settled) wins on its id.
  setPublisherCommits((mem) => {
    const ids = new Set(mem.map((e) => e.id));
    return [...mem, ...stored.publisherCommits.filter((e) => !ids.has(e.id))];
  });

  // The account's publish counters. A value that landed in memory before
  // this settled is at least the floor it was seeded from; the larger of
  // the two wins, so neither can rewind.
  const storedCounters = loadAllCounters(args.accountIdB64);
  setCounterByChannel((map) => {
    const next = new Map<string, bigint>(storedCounters);
    for (const [id, value] of map) {
      const held = next.get(id);
      if (held === undefined || value > held) next.set(id, value);
    }
    return next;
  });

  // 3. Publisher keys: re-derive the content seed from the restored group.
  if (providerRestored && stored.publisherSigningSeeds.size > 0) {
    try {
      const wasm = await loadWasm();
      for (const [id, signingKeySeed] of stored.publisherSigningSeeds) {
        // The wasm load suspended: the session may have ended since.
        if (isStale(args)) break;
        if (keysByChannel().has(id)) continue;
        try {
          // ADR-0078 section 6: a co-published channel's group is opened
          // under the id its link conveyed (the owner's derivation).
          const joinedId = coPublisherGroupIds().get(id);
          const group =
            joinedId === undefined
              ? wasm.PublisherGroup.load(args.identity, channelIdFromHex(id))
              : wasm.PublisherGroup.loadJoined(args.identity, hexBytes(joinedId), channelIdFromHex(id));
          if (group === undefined) {
            // eslint-disable-next-line no-console
            console.warn(`channel ${id}: publisher group missing from the restored provider`);
            continue;
          }
          let seed: Uint8Array;
          let seedEpoch: number;
          try {
            seed = group.channelSeed(args.identity);
            seedEpoch = Number(group.epoch());
          } finally {
            group.free();
          }
          channelsActions.setKeys(id, { seed, signingKeySeed, seedEpoch });
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn(`channel ${id}: could not re-open publisher keys`, err);
        }
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("channel log: wasm unavailable; publisher keys not re-opened", err);
    }
  }

  // 4. The plaintext localStorage row the subscriber keys used to live in.
  try {
    localStorage.removeItem(LEGACY_SUBSCRIBER_KEYS_STORAGE_KEY);
  } catch {
    // no localStorage (tests) or blocked storage — nothing to wipe
  }

  // 5. Arm write-through and settle. Write back once so the merged view
  //    (and a fresh provider snapshot) is on disk. A hydration whose
  //    session ended during step 3 arms nothing and writes nothing; it
  //    only releases anyone still waiting on it.
  if (isStale(args)) {
    releaseWaiters();
    return;
  }
  sink = {
    accountIdB64: args.accountIdB64,
    deviceKey: args.deviceKey,
    snapshotProvider: args.snapshotProvider ?? ((): Uint8Array => args.identity.snapshotStorage()),
    leafKeyB64,
  };
  settleHydration();
  await flushNow();
}

/**
 * Mark the hydration settled without loading anything — the session has
 * no device key (or no account identity), so there is nothing to read or
 * write. Routes awaiting the hydration proceed with in-memory state.
 */
export function markChannelsHydrationUnavailable(): void {
  sink = null;
  settleHydration();
}

/**
 * Drop the in-memory channel state for the session that just ended
 * (logout, account switch). Flushes a pending write first so the last
 * mutation lands. Drops the in-memory publish counters too, so the next
 * account never starts from this one's floor. Leaves the encrypted row
 * AND the persisted publish counters on disk: both are per account, the
 * row is unreadable without the device key, and the counter must stay
 * for as long as the signing seeds in that row do (it is what keeps a
 * later post under the same sender key from rewinding).
 */
export function clearChannelsSession(): Promise<void> {
  const pending = flushPending();
  dropChannelsMemory();
  return pending;
}

/** Empty the in-memory state and disarm write-through, writing nothing. */
function dropChannelsMemory(): void {
  sink = null;
  lastProviderSnapshot = null;
  lastProviderLeafKeyB64 = null;
  setChannels([]);
  setPostsByChannel(new Map());
  setEpochByChannel(new Map());
  setKeysByChannel(new Map());
  setCounterByChannel(new Map());
  counterOwner = null;
  setSubscriberKeysByChannel(new Map());
  setSubscribedSeedsByChannel(new Map());
  setMetaByChannel(new Map());
  setCoPublisherGroupIds(new Map());
  setPublisherCommits([]);
  setAccessByChannel(new Map());
  poisoned = false;
  setOrphanedByKeyReset(new Set<string>());
  setHydrated(false);
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

// Forget device. The hydration MERGES what is in memory into the stored row
// and writes the union back, so a channel, key or seed put in memory by work
// that was in flight across the wipe (after the wipe step cleared the state)
// would be written into the wiped account's new row at its next sign-in on
// this page. Nothing is flushed: the account's stores are sealed. The state
// is the session's; it is dropped when it belongs to one of `ids` or to
// nobody (the session already ended), never when another account owns it.
registerAccountMemory("channels-state", (ids) => {
  const owner = sink?.accountIdB64 ?? counterOwner;
  if (owner !== null && !ids.includes(owner)) return;
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flushTimer = null;
  }
  dropChannelsMemory();
});

/**
 * The Forget device / Delete account half: drop the in-memory state, the
 * account's encrypted row (which holds its publisher signing seeds) and
 * then the account's publish counters. The counters go only after the row
 * is deleted: the sender key is in the post KDF and is random and
 * unrecoverable, so once its seed is gone the next post signs with a new
 * key and a counter restarting from 0 cannot collide, while a counter
 * removed beside a seed that survived (a failed delete) could rewind
 * under it. If the row delete throws, the counters stay. Another
 * account's counters are untouched.
 */
export async function wipeChannelsForAccount(accountIdB64: string): Promise<void> {
  await clearChannelsSession();
  await deleteChannelLog(accountIdB64);
  // The decrypted posts the feed ranks (ADR-0063 section 3) go with it.
  await deleteChannelPostLog(accountIdB64);
  clearPublishCountersForAccount(accountIdB64);
}

// ---------------------------------------------------------
// Actions
// ---------------------------------------------------------

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
    schedulePersist();
  },
  /** Remove a channel from the local list. The server-
   *  side registry is unaffected — this is purely the
   *  user un-following on their own device. */
  removeChannel(channelId: string): void {
    // ADR-0073 phase 2 (UX 16.6) — `Remove from this device` on an orphaned
    // channel: the record goes with it.
    if (orphanedByKeyReset().has(channelId)) {
      const owner = counterOwner ?? sink?.accountIdB64 ?? null;
      setOrphanedByKeyReset(
        owner === null
          ? new Set([...orphanedByKeyReset()].filter((id) => id !== channelId))
          : forgetOrphanedByReset(owner, channelId),
      );
    }
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
    setMetaByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    setAccessByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    // Clear only the in-memory counter — the account's PERSISTED floor
    // is deliberately kept. Un-follow does not destroy the PublisherGroup
    // (it persists in the account identity's provider), so re-joining the
    // same channel re-derives the SAME seed; keeping the floor means the
    // counter can never rewind under a seed and sender key that come back
    // (a counter that is too high is always safe). `setKeys` re-seeds the
    // in-memory value from the persisted floor.
    setCounterByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    schedulePersist();
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
    schedulePersist();
  },
  /** ADR-0051 — install the newest shared metadata record for a channel.
   *  The caller has already decided it is newer (`channelMetaIsNewer` on
   *  the receive path; the publisher's own fresh stamp on the send path).
   *  Persisted. */
  setSharedMeta(channelId: string, meta: ChannelSharedMeta): void {
    setMetaByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, meta);
      return next;
    });
    schedulePersist();
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
  /** Install publisher keys for a channel. Seeds the
   *  publish counter LOAD-ELSE-0: a genuinely new channel
   *  (no in-memory and no persisted counter) starts at 0;
   *  an existing channel keeps its floor. It must NOT clobber
   *  a persisted counter to 0 — the channel content seed is
   *  STABLE across a reload / re-open (it re-derives from the
   *  persistent PublisherGroup MLS exporter) and so is the sender
   *  key (its signing seed is persisted with the account's
   *  channel log), so rewinding the
   *  counter to 0 would re-use an AEAD `(key, nonce)` under
   *  the same seed + epoch (catastrophic keystream reuse).
   *  Mirrors the publisher-seed reuse-else-generate pattern.
   *  `nextCounter` owns the write-through; `setKeys` only
   *  seeds the in-memory value. Persists the signing seed
   *  and a fresh provider snapshot. */
  setKeys(channelId: string, keys: ChannelPublisherKeys): void {
    setKeysByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, keys);
      return next;
    });
    setCounterByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, seedCounter(map.get(channelId), channelId, counterOwner ?? undefined));
      return next;
    });
    schedulePersist();
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
   *
   *  Write-through: the ADVANCED floor (`value + 1`) is persisted
   *  BEFORE this returns, so a crash / reload right after a post can
   *  never hand out `value` again. Burning a counter value is safe;
   *  re-issuing one re-uses the AEAD nonce (catastrophic), so the
   *  store is advanced first and the caller composes with the returned
   *  (pre-increment) `value`. The floor is the session account's;
   *  with no account to persist under this throws and issues
   *  nothing (`issueAndAdvance`). */
  nextCounter(channelId: string): bigint {
    const issued = issueAndAdvance(
      untrack(counterByChannel).get(channelId),
      channelId,
      counterOwner ?? undefined,
    );
    setCounterByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, issued.advanced);
      return next;
    });
    return issued.value;
  },
  /** Install / replace the subscriber-side Ed25519 keypair
   *  the device generated when joining `channelId` as a
   *  reader. Persisted so a reload does not strand the user
   *  without the secret needed to open future envelopes. */
  setSubscriberKeys(channelId: string, keys: ChannelSubscriberKeys): void {
    setSubscriberKeysByChannel((map) => {
      const next = new Map(map);
      next.set(channelId, keys);
      return next;
    });
    schedulePersist();
  },
  /** ADR-0087 section 15 (UX 15.2) — this account stopped being a publisher
   *  of `channelId` (removed, or stepped down) but stays subscribed: drop
   *  the publisher keys and the co-published group id, keep the channel, its
   *  posts, the subscriber key and every recovered seed, so the channel
   *  falls back to the subscriber view. */
  dropPublisherRole(channelId: string): void {
    setKeysByChannel((map) => {
      if (!map.has(channelId)) return map;
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    setCoPublisherGroupIds((map) => {
      if (!map.has(channelId)) return map;
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    schedulePersist();
  },
  /** Forget the subscriber-side keypair for `channelId` —
   *  used on un-follow. Clears the recovered seeds too,
   *  since they were only useful while the device had the
   *  matching secret half. */
  clearSubscriberKeys(channelId: string): void {
    setSubscriberKeysByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    setSubscribedSeedsByChannel((map) => {
      const next = new Map(map);
      next.delete(channelId);
      return next;
    });
    schedulePersist();
  },
  /** Record the recovered content-key seed for a specific
   *  epoch after the subscriber opened that epoch's seed
   *  envelope (ADR-0031 multi-epoch retention). Idempotent
   *  per `(channelId, epoch)`. Persisted; never sent
   *  off-device. */
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
    schedulePersist();
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
  /** The account identity's MLS provider changed outside this
   *  store's own mutations — a PublisherGroup commit (co-publisher
   *  approve, an applied incoming commit, a rotation) or an
   *  external-join KeyPackage minted for a co-publisher request.
   *  Schedules a flush so the next snapshot carries it. */
  notePublisherGroupChanged(): void {
    schedulePersist();
  },
  /** The FAIL-CLOSED sibling of {@link notePublisherGroupChanged}: write the
   *  account identity's provider into the channel log NOW and reject when
   *  that did not happen. For a caller that publishes only after its MLS
   *  state is recorded (the co-publisher knock): a refused persist means
   *  nothing was published. */
  persistPublisherGroupDurable(): Promise<void> {
    return flushDurable();
  },
  /** ADR-0078 section 6 — remember the PublisherGroup id of a channel this
   *  account co-publishes (from its `/g/` capability). Written with the next
   *  flush; the co-publisher request writes durably before its knock. */
  setCoPublisherGroupId(channelIdHex: string, groupIdHex: string): void {
    setCoPublisherGroupIds((map) => new Map(map).set(channelIdHex, groupIdHex.toLowerCase()));
    schedulePersist();
  },
  /** The PublisherGroup id hex of a co-published channel, or `undefined`
   *  when this account owns it (or does not publish to it). */
  coPublisherGroupIdOf(channelIdHex: string): string | undefined {
    return coPublisherGroupIds().get(channelIdHex);
  },
  /** ADR-0087 phase B — the access record of a channel (empty when none). */
  accessOf(channelId: string): ChannelAccessRecord {
    return accessByChannel().get(channelId) ?? emptyAccessRecord();
  },
  /** ADR-0087 phase B — change a channel's access record; persisted with the
   *  next flush. */
  updateAccess(channelId: string, f: (r: ChannelAccessRecord) => ChannelAccessRecord): void {
    setAccessByChannel((map) => new Map(map).set(channelId, f(map.get(channelId) ?? emptyAccessRecord())));
    schedulePersist();
  },
  /** ADR-0087 phase B — {@link updateAccess}, written durably now (a
   *  secret a later publish depends on: an admission seed, an invite seed). */
  async updateAccessDurable(
    channelId: string,
    f: (r: ChannelAccessRecord) => ChannelAccessRecord,
  ): Promise<void> {
    setAccessByChannel((map) => new Map(map).set(channelId, f(map.get(channelId) ?? emptyAccessRecord())));
    await flushDurable();
  },
  /** G-50 — the owed PublisherGroup commits, oldest first. */
  pendingPublisherCommits(): readonly PublisherCommitEntry[] {
    return publisherCommits();
  },
  /**
   * G-50 — record a PublisherGroup commit together with the provider
   * snapshot that holds its N+1, in ONE durable write, BEFORE any of its
   * steps runs. Rejects when the write did not land; the caller then runs
   * no step, and the store refuses every later write this session so the
   * unrecorded N+1 never becomes durable (a reload rehydrates N).
   */
  async recordPublisherCommit(entry: PublisherCommitEntry): Promise<void> {
    setPublisherCommits((list) => [...list.filter((e) => e.id !== entry.id), entry]);
    // The commit moves the channel to a new epoch, so the channel record is
    // owed under it (ADR-0051 section 4); written in the same flush as the
    // commit's record, and cleared once the re-post landed (`settleOwedRecord`).
    const priorAccess = untrack(accessByChannel).get(entry.channelIdHex);
    setAccessByChannel((map) =>
      new Map(map).set(entry.channelIdHex, { ...(map.get(entry.channelIdHex) ?? emptyAccessRecord()), recordOwed: true }),
    );
    try {
      await flushDurable();
    } catch (err) {
      setPublisherCommits((list) => list.filter((e) => e.id !== entry.id));
      setAccessByChannel((map) => {
        const next = new Map(map);
        if (priorAccess === undefined) next.delete(entry.channelIdHex);
        else next.set(entry.channelIdHex, priorAccess);
        return next;
      });
      poisoned = true;
      // eslint-disable-next-line no-console
      console.error("G-50: a PublisherGroup commit could not be recorded; channel writes are refused until reload");
      throw err;
    }
  },
  /** G-50 — a step of `entry` landed: write the updated record durably. */
  async savePublisherCommit(entry: PublisherCommitEntry): Promise<void> {
    setPublisherCommits((list) => list.map((e) => (e.id === entry.id ? entry : e)));
    await flushDurable();
  },
  /** G-50 — every step of entry `id` landed (or a restart found it torn):
   *  drop the record durably. */
  async finishPublisherCommit(id: string): Promise<void> {
    setPublisherCommits((list) => list.filter((e) => e.id !== id));
    await flushDurable();
  },
};
