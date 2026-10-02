// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Cross-device sync runtime (Phase 2a3e2b-web).
 *
 * Sits **inside** `<AuthProvider>` (it reads the session +
 * identitySecrets the auth side publishes) and manages the
 * per-account DeviceGroup lifecycle that powers cross-device
 * reactive state — drafts, per-thread flags, conversation
 * order, read cursors.
 *
 * Three concerns rolled into one Solid context:
 *
 *   1. **Lifecycle.** On every transition into a `session`
 *      state the context lazily ensures a DeviceGroup handle
 *      via [`ensureDeviceGroup`], snapshots it after every
 *      mutation, and tears it down on logout / forgetDevice.
 *
 *   2. **Outbound state mutations.** UI calls
 *      `setDraft(groupId, ...)` / `setFlags(...)` /
 *      `setReadCursor(...)` / `setOrder(...)`; each call
 *      composes the matching typed wasm message, persists the
 *      DeviceGroup snapshot, and publishes the wire bytes to
 *      the DeviceGroup DS journal so every other device on
 *      the account converges.
 *
 *   3. **Inbound state mutations.** A background poll loop
 *      pulls the DeviceGroup journal, runs `processIncoming`
 *      on each row, dispatches application payloads through
 *      [`parseDeviceGroupPayload`], and folds them into ONE
 *      `SyncedState` (ADR-0058, the Rust fold shared with iOS
 *      and Android). The reactive maps the app reads are
 *      projections of it (`synced-projection.ts`).
 *
 *   4. **Chat folders and catch-up (ADR-0058).** Folder edits are
 *      folded here at once and sent through an outbox that
 *      survives a reload; when the DeviceGroup gains a device, the
 *      lowest prior leaf re-sends the whole fold so the newcomer
 *      starts with the account's pins, drafts and folders.
 *
 * Same wasm Identity / DeviceGroup handle pair the
 * `apps/web/src/devices` orchestrator uses on the add-device
 * surface — but this context holds onto it for the duration
 * of the session rather than acquiring + releasing per call,
 * because the poll loop needs a long-lived handle.
 */
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

import { useAuth } from "../../auth/AuthContext";
import { ensureDeviceIdentitySeed } from "../../auth/device-identity-store";
import { getDeviceKey } from "../../auth/key-store";
import { loadSyncState, saveSyncState, syncStateGeneration } from "./sync-state-idb";
import { SyncStatePersister, type CapturedSyncState } from "./sync-state-persister";
import { DsClient, DsHttpError, DsNetworkError } from "../../ds/client";
import { dsAccess, takeSigner } from "../../ds/access";
import { loadWasm } from "../../wasm";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
  ParsedDeviceGroupPayload,
  SyncedState as WasmSyncedState,
} from "../../../wasm/lakko_ffi_wasm";

import {
  ensureDeviceGroup,
  persistDeviceGroupSnapshot,
  persistDeviceGroupSnapshotWithCursor,
} from "../lifecycle";
import {
  deleteDeviceGroupSnapshot,
  getDeviceGroupJournalCursor,
} from "../device-group-snapshot-store";
import { applyDeviceJournalBatch, startingCursor } from "./journal-batch";
import { forkVerdict } from "./fork-verdict";
import {
  forkRepairAllowed,
  isDeviceRemoved,
  pollStepAfterBatch,
  readDeviceGroupActive,
} from "./removed-device";
import { decodeJournalEntry, encodeJournalEntry } from "../journal-envelope";
import { fetchDeviceAttestKey } from "../../auth/device-attest-key";
import { mintHlc, observeHlc } from "./hlc";
import { receiveServiceEnvelope } from "../../reach/service-envelope-intake";
import {
  flushServiceEnvelopes,
  registerServiceEnvelopeSender,
} from "../../reach/service-envelope-outbox";
import {
  groupIdHex,
  type DraftState,
  type FlagsState,
  type OrderState,
  type ReadCursorState,
} from "./state";
import {
  buildFolderChangeSet,
  mergeRestoredFold,
  openPersistedFold,
  projectSyncedState,
} from "./synced-projection";
import { drainOutbox, type OutboxBatch } from "./folder-outbox";
import {
  detectGrowth,
  onDeviceGroupChangedHere,
  snapshotDelayMs,
  type LeafMember,
} from "./growth";
import { onRestoredSyncedState, registerSyncedStateSource } from "./synced-state-source";
import {
  bytesToHex,
  stampEdits,
  type FolderModel,
  type RegisterEdit,
} from "../../chat/folders/folder-model";

/** How long a change waits before the stored row is rewritten. A draft moves
 *  on a keystroke, and the row is one encrypt plus one IndexedDB write. */
const SYNC_PERSIST_DEBOUNCE_MS = 500;

/** How long a folder edit that could not be sent waits before the next try,
 *  when no poll success prompts one sooner. */
const OUTBOX_RETRY_MS = 15_000;

// ---------- Reactive maps + public API shape ---------------------

interface DraftMap {
  readonly [groupIdHex: string]: DraftState;
}
interface FlagsMap {
  readonly [groupIdHex: string]: FlagsState;
}
interface ReadCursorMap {
  readonly [groupIdHex: string]: ReadCursorState;
}

/** Public surface of the cross-device sync context. */
export interface DeviceGroupSyncApi {
  /** Reactive: `true` once the per-session DeviceGroup handle
   *  is loaded and the poll loop has started. */
  readonly ready: Accessor<boolean>;
  /** Reactive snapshot of the per-group draft map. */
  readonly drafts: Accessor<DraftMap>;
  /** Reactive snapshot of the per-group flags map. */
  readonly flags: Accessor<FlagsMap>;
  /** Reactive snapshot of the per-group read cursor map. */
  readonly readCursors: Accessor<ReadCursorMap>;
  /** Reactive snapshot of the whole-account explicit
   *  conversation ordering. `null` when none is held. */
  readonly order: Accessor<OrderState | null>;
  /** Reactive: the person's chat folders, in bar order (ADR-0058). Only
   *  folders whose name has arrived; a deleted folder never comes back. */
  readonly folders: Accessor<readonly FolderModel[]>;
  /** Reactive: a folder edit made here has not reached the other devices
   *  yet (UX spec section 6). */
  readonly pendingFolderSync: Accessor<boolean>;
  /** Reactive: another device removed this one from the account, and this
   *  device has applied that removal (its DeviceGroup is inactive). The poll
   *  loop has stopped for good; the app shell shows "This device was
   *  removed" (docs/ux/second-device.md section 3.1). Reset on sign-out and
   *  on an account switch. */
  readonly deviceRemoved: Accessor<boolean>;

  /** Convenience helper — return the draft for `groupId` or
   *  `undefined` if none. `groupId` is the raw bytes. */
  getDraft(groupId: Uint8Array): DraftState | undefined;
  /** Convenience helper — return the flags for `groupId` or
   *  `undefined`. */
  getFlags(groupId: Uint8Array): FlagsState | undefined;
  /** Convenience helper — return the read cursor or
   *  `undefined`. */
  getReadCursor(groupId: Uint8Array): ReadCursorState | undefined;

  /** Compose + publish a `Draft` payload. Empty `text` clears
   *  the receivers' draft for the same group. */
  setDraft(
    groupId: Uint8Array,
    text: string,
    replyToId?: Uint8Array,
  ): Promise<void>;

  /** Compose + publish a `Flags` payload. Last-write-wins for
   *  the WHOLE struct — the caller supplies the full snapshot. */
  setFlags(
    groupId: Uint8Array,
    flags: {
      readonly pinned: boolean;
      readonly muted: boolean;
      readonly archived: boolean;
      readonly unreadOverride: boolean;
    },
  ): Promise<void>;

  /** Compose + publish a `ReadCursor` payload. */
  setReadCursor(
    groupId: Uint8Array,
    upToMessageId: Uint8Array,
    readAtMs: bigint,
  ): Promise<void>;

  /** Compose + publish an `Order` payload — explicit display
   *  ordering for the conversation list. */
  setOrder(groupIds: readonly Uint8Array[]): Promise<void>;

  /**
   * One folder edit (ADR-0058): the registers it changes and nothing else.
   * Stamped here, folded here at once, then sent - now if this device is
   * online, later if not. Throws only when the edit itself is invalid (a
   * bound broken) or the runtime has not started; a send failure is not an
   * error, it waits in the outbox.
   */
  applyFolderEdits(edits: readonly RegisterEdit[]): Promise<void>;
}

const DeviceGroupSyncContext = createContext<DeviceGroupSyncApi>();

// ---------- Provider implementation ------------------------------

export function DeviceGroupSyncProvider(props: {
  readonly children: JSX.Element;
}): JSX.Element {
  const auth = useAuth();

  // Reactive projections of the fold.
  const [drafts, setDrafts] = createSignal<DraftMap>({});
  const [flags, setFlags] = createSignal<FlagsMap>({});
  const [readCursors, setReadCursors] = createSignal<ReadCursorMap>({});
  const [order, setOrder] = createSignal<OrderState | null>(null);
  const [folders, setFolders] = createSignal<readonly FolderModel[]>([]);
  const [outbox, setOutbox] = createSignal<readonly OutboxBatch[]>([]);
  const [ready, setReady] = createSignal(false);
  const [deviceRemoved, setDeviceRemoved] = createSignal(false);
  /** Bumped on every change worth writing to disk. */
  const [stateVersion, setStateVersion] = createSignal(0);

  // Long-lived handles for the active session. `null` between
  // sessions. The poll loop reads them every iteration; the
  // send paths reads them per call.
  let identity: WasmIdentity | null = null;
  let group: WasmDeviceGroup | null = null;
  /** The one fold of the account's synced state (ADR-0058). */
  let synced: WasmSyncedState | null = null;
  let pollAbort: AbortController | null = null;
  let pollDone: Promise<void> | null = null;
  let activeAccountIdB64: string | null = null;
  /** While the handle is being reloaded after a change made through another
   *  handle on this device, nothing is sent from the stale one. */
  let reloading = false;
  let snapshotTimer: ReturnType<typeof setTimeout> | null = null;
  /** Writes the stored row, debounced; flushed by `teardown` so Log out keeps
   *  the last edit (see `sync-state-persister.ts`). */
  const persister = new SyncStatePersister({
    debounceMs: SYNC_PERSIST_DEBOUNCE_MS,
    save: async (account, captured) => {
      const deviceKey = await getDeviceKey(account);
      await saveSyncState(account, deviceKey, captured.row, captured.generation);
    },
    onError: (err) => {
      // Best-effort: the in-memory state stays good and the next change
      // rewrites the row.
      // eslint-disable-next-line no-console
      console.warn("device-sync state persist failed", err);
    },
  });
  /** The row's delete generation when this runtime bound the account. A wipe
   *  since then (Forget device, Delete account) bumps it, and nothing captured
   *  under the old binding is written. */
  let boundSyncGeneration: number | null = null;
  let outboxTimer: ReturnType<typeof setTimeout> | null = null;
  let draining = false;
  /** ADR-0060 section 7 - unregisters this provider as the carrier of the
   *  Visibility / ReachState envelopes. */
  let unregisterServiceSender: (() => void) | null = null;
  /** DS ids of the journal rows THIS provider published this page lifetime.
   *  The DS does not de-duplicate, so each comes straight back on the next
   *  poll; the loop skips it by id instead of handing OpenMLS a message it
   *  cannot decrypt (`CannotDecryptOwnMessage`). Exact by construction: no
   *  other device's row can carry an id this device got from its own publish.
   *  See `sync/journal-batch.ts`. */
  const ownPublishedIds = new Set<number>();
  /** Accounts whose forked DeviceGroup this provider has already re-founded.
   *  The repair runs at most once per account for the provider's lifetime
   *  (kept across logout), so a device that forks again waits for the next
   *  page load rather than resetting the account's chain in a loop. */
  const forkRepaired = new Set<string>();

  // ADR-0053 contract point 1 — every request to the DeviceGroup journal is
  // proven under the access key of the epoch this device's DeviceGroup is at.
  // The DS client asks at the moment of each request; a refused request makes
  // it claim the journal with the account's root (ADR-0057 obligation 2).
  const unregisterDsSource = dsAccess.registerGroupSource((groupId) => {
    if (group === null || identity === null) return null;
    const own = group.groupId();
    if (own.length !== groupId.length) return null;
    for (let i = 0; i < own.length; i++) if (own[i] !== groupId[i]) return null;
    return group.dsAccessSigner(identity);
  });
  onCleanup(unregisterDsSource);

  // ADR-0057 — the account owns its DeviceGroup: every device holds the root.
  // Keyed on the id derived from the account (`deviceGroupIdFor`), set before
  // the group is ensured, so the founding claim finds the root too.
  let ownDeviceGroupId: Uint8Array | null = null;
  const unregisterOwner = dsAccess.registerOwnerSource((groupId) => {
    const own = ownDeviceGroupId ?? group?.groupId() ?? null;
    if (own === null || own.length !== groupId.length) return null;
    for (let i = 0; i < own.length; i++) if (own[i] !== groupId[i]) return null;
    return true;
  });
  onCleanup(unregisterOwner);
  // ADR-0058 section 5.2 — the backup carries the fold.
  const unregisterSource = registerSyncedStateSource(() =>
    synced === null ? null : synced.toBytes(),
  );
  onCleanup(unregisterSource);

  // A backup restore that finished after this runtime started: merge the
  // restored fold into the running one (ADR-0058 section 5.2). Always a
  // merge - the fold keeps the newer register from each side.
  onCleanup(
    onRestoredSyncedState((account, bytes) => {
      if (account !== activeAccountIdB64 || synced === null || bytes.length === 0) return;
      void loadWasm().then((wasm) => {
        if (synced === null || account !== activeAccountIdB64) return;
        // A backup fold this build cannot read holds nothing to merge.
        const backupFold = openPersistedFold(wasm, bytes, "restored");
        if (backupFold === null) return;
        try {
          if (mergeRestoredFold(synced, backupFold)) {
            observeHlc(synced.maxHlc());
            refresh();
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("restored synced state not merged", err);
        } finally {
          backupFold.free();
        }
      });
    }),
  );


  /** Re-read the fold into the reactive maps, and schedule the disk write. */
  function refresh(): void {
    if (synced === null) return;
    const p = projectSyncedState(synced);
    setDrafts(p.drafts);
    setFlags(p.flags);
    setReadCursors(p.readCursors);
    setOrder(p.order);
    setFolders(p.folders);
    setStateVersion((v) => v + 1);
  }

  // ----- Lifecycle: session ↔ anon transitions --------------------

  /** How long a failed bootstrap waits before trying again. */
  const BOOTSTRAP_RETRY_MS = 30_000;

  createEffect(() => {
    const s = auth.state();
    if (s.kind !== "session") {
      // Tear down on logout / forgetDevice. Cancel the poll
      // loop, free the wasm handles, clear all reactive state.
      teardown();
      return;
    }
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      // Session restored from cache without an export key
      // (cold-start); the effect re-runs when secrets land.
      return;
    }
    // Idempotent — if we already bootstrapped for the same
    // account, skip the heavy work.
    if (activeAccountIdB64 === s.account.accountId) return;
    void bootstrap(s.account.accountId, secrets.accountId);
  });

  onCleanup(() => {
    teardown();
  });

  // A pairing on this device commits through a handle of its own; the
  // long-lived one here is then an epoch behind. Stop it before it can see
  // that commit, reload from disk, and - for an add - run the growth re-send.
  onCleanup(
    onDeviceGroupChangedHere((change) => {
      void reloadHandle().then(async () => {
        if (change.growth !== undefined) {
          // The add's commit is published right after the announce; a floor
          // keeps the re-send behind it on the journal.
          await maybeScheduleSnapshot(change.growth.ownLeaf, change.growth.priorLeaves, 5_000);
        }
      });
    }),
  );

  let accountIdBytesActive: Uint8Array | null = null;

  async function bootstrap(
    accountIdB64: string,
    accountIdBytes: Uint8Array,
  ): Promise<void> {
    // Already bootstrapping concurrently — drop the duplicate.
    if (activeAccountIdB64 === accountIdB64 && identity !== null) return;
    activeAccountIdB64 = accountIdB64;
    try {
      ownDeviceGroupId = (await loadWasm()).deviceGroupIdFor(accountIdBytes);
    } catch {
      ownDeviceGroupId = null;
    }
    accountIdBytesActive = accountIdBytes;

    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) {
      // No device key — recovery / forget-device race. Bail;
      // the next session transition retries.
      activeAccountIdB64 = null;
      return;
    }

    let ensured;
    try {
      const seed = await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      ensured = await ensureDeviceGroup(
        accountIdBytes,
        accountIdB64,
        seed,
        deviceKey,
        auth.identitySecrets()?.accountIdentitySeed,
      );
      seed.fill(0);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroupSyncProvider bootstrap failed", err);
      activeAccountIdB64 = null;
      // A founding whose journal claim did not land (ADR-0057, the second
      // security pass: it fails closed, nothing persisted) is retried while
      // the session lasts. Chosen, not measured.
      setTimeout(() => {
        const now = auth.state();
        const again = auth.identitySecrets();
        if (now.kind !== "session" || again === null) return;
        if (now.account.accountId !== accountIdB64 || activeAccountIdB64 !== null) return;
        void bootstrap(accountIdB64, again.accountId);
      }, BOOTSTRAP_RETRY_MS);
      return;
    }

    identity = ensured.identity;
    group = ensured.group;

    // Removed while the app was closed: the persisted snapshot already
    // carries the commit that removed this device. Nothing is polled, folded
    // or persisted for it; the shell shows the removed screen.
    if (isDeviceRemoved(readDeviceGroupActive(group))) {
      markRemoved();
      return;
    }

    // Seed the fold from this device's own disk BEFORE the poll starts - this
    // client resumes the journal from a persisted cursor, so the rows behind
    // it are never read again. The stored state is a head start, not an
    // authority: the poll folds newer rows on top by stamp.
    const wasm = await loadWasm();
    // A write the previous session flushed on Log out may still be in flight;
    // read after it, so a sign-in right after a sign-out finds the last edit.
    await persister.idle();
    boundSyncGeneration = syncStateGeneration(accountIdB64);
    let restored: Awaited<ReturnType<typeof loadSyncState>> = null;
    try {
      restored = await loadSyncState(accountIdB64, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device-sync state restore failed; starting empty", err);
    }
    // A row this build cannot read (an older encoding version) is "none
    // held": start empty, the other devices' growth re-send and the journal
    // refill it, and the next write replaces the row.
    synced =
      restored !== null && restored.synced.length > 0
        ? openPersistedFold(wasm, restored.synced, "stored")
        : null;
    synced ??= new wasm.SyncedState();
    try {
      observeHlc(synced.maxHlc());
    } catch {
      // best-effort
    }
    setOutbox(restored?.pending ?? []);
    refresh();
    setReady(true);

    // ADR-0060 section 7 - the "who can find me" setting and the reach key
    // version counters ride this DeviceGroup as `ServiceEnvelope` bodies.
    unregisterServiceSender?.();
    unregisterServiceSender = registerServiceEnvelopeSender(async (envelope) => {
      // Checked before sealing: a handle being replaced must not advance its
      // sender ratchet for a message that is never published.
      if (group === null || identity === null || reloading || deviceRemoved()) {
        throw new Error("DeviceGroupSyncProvider is not ready");
      }
      await publishGroupMessage(group.createServiceEnvelopeMessage(identity, envelope, mintHlc()));
    });

    startPollLoop(accountIdB64, deviceKey);
    void drainFolderOutbox();
  }

  function startPollLoop(accountIdB64: string, deviceKey: CryptoKey): void {
    if (group === null || identity === null || deviceRemoved()) return;
    pollAbort = new AbortController();
    pollDone = runPollLoop(group, identity, accountIdB64, deviceKey, pollAbort.signal);
  }

  /**
   * This device was removed from the account (docs/ux/second-device.md
   * section 3.1): stop the poll loop for good this session and every timer
   * that would send or persist, and publish `deviceRemoved`. The handles stay
   * held until `teardown` frees them on sign-out.
   */
  function markRemoved(): void {
    if (!deviceRemoved()) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup: this device was removed from the account; sync stopped");
    }
    setDeviceRemoved(true);
    if (pollAbort !== null) {
      pollAbort.abort();
      pollAbort = null;
    }
    if (snapshotTimer !== null) clearTimeout(snapshotTimer);
    snapshotTimer = null;
    if (outboxTimer !== null) clearTimeout(outboxTimer);
    outboxTimer = null;
    persister.cancel();
  }

  /**
   * Replace the long-lived handle with a fresh one read from disk. Used when
   * another handle on this device (the pairing flow) committed: the one held
   * here is an epoch behind and anything it sent or persisted would be wrong.
   */
  async function reloadHandle(): Promise<void> {
    const account = activeAccountIdB64;
    const accountBytes = accountIdBytesActive;
    if (account === null || accountBytes === null || group === null) return;
    if (deviceRemoved()) return;
    reloading = true;
    try {
      pollAbort?.abort();
      pollAbort = null;
      if (pollDone !== null) await pollDone;
      pollDone = null;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null || activeAccountIdB64 !== account) return;
      const seed = await ensureDeviceIdentitySeed(account, deviceKey);
      const ensured = await ensureDeviceGroup(accountBytes, account, seed, deviceKey);
      seed.fill(0);
      try {
        group?.free();
        identity?.free();
      } catch {
        // best-effort
      }
      group = ensured.group;
      identity = ensured.identity;
      ownPublishedIds.clear();
      if (isDeviceRemoved(readDeviceGroupActive(group))) {
        markRemoved();
        return;
      }
      startPollLoop(account, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup handle reload failed", err);
    } finally {
      reloading = false;
    }
    void drainFolderOutbox();
    // ADR-0060 section 7 - an envelope marked while the handle was being
    // replaced (a pairing hands its setting over this way) goes now.
    void flushServiceEnvelopes();
  }

  function teardown(): void {
    // Log out keeps the stored row (and the folder edits it holds that have
    // not been sent): the restored DeviceGroup has already consumed the
    // journal rows that built it, so a row dropped here is gone for good. A
    // change still waiting for its debounce is written now, captured before
    // the fold is freed below; nothing is captured once the account is
    // unbound. Forget device and Delete account remove the row through the
    // account wipe (`auth/wipe-account.ts`).
    void persister.flush();
    unregisterServiceSender?.();
    unregisterServiceSender = null;
    if (pollAbort !== null) {
      pollAbort.abort();
      pollAbort = null;
    }
    pollDone = null;
    if (snapshotTimer !== null) clearTimeout(snapshotTimer);
    snapshotTimer = null;
    if (outboxTimer !== null) clearTimeout(outboxTimer);
    outboxTimer = null;
    if (group !== null) {
      try {
        group.free();
      } catch {
        // best-effort
      }
      group = null;
    }
    if (identity !== null) {
      try {
        identity.free();
      } catch {
        // best-effort
      }
      identity = null;
    }
    if (synced !== null) {
      try {
        synced.free();
      } catch {
        // best-effort
      }
      synced = null;
    }
    activeAccountIdB64 = null;
    boundSyncGeneration = null;
    ownDeviceGroupId = null;
    accountIdBytesActive = null;
    ownPublishedIds.clear();
    setReady(false);
    setDeviceRemoved(false);
    setDrafts({});
    setFlags({});
    setReadCursors({});
    setOrder(null);
    setFolders([]);
    setOutbox([]);
  }

  // Write the row whenever the fold or the outbox moves - a local set or a
  // sibling's row folded by the poll. Debounced because a draft changes on a
  // keystroke.
  createEffect(() => {
    stateVersion();
    outbox();
    const account = activeAccountIdB64;
    if (account === null || synced === null || deviceRemoved()) return;
    // The capture reads the newest fold and outbox when the write runs. It
    // may run inside `teardown`, which an effect calls, so it reads untracked.
    const capture = (): CapturedSyncState | null =>
      untrack(() => {
        const generation = boundSyncGeneration;
        if (synced === null || activeAccountIdB64 !== account || deviceRemoved()) return null;
        if (generation === null || syncStateGeneration(account) !== generation) return null;
        return { row: { synced: synced.toBytes(), pending: outbox() }, generation };
      });
    persister.schedule(account, capture);
  });

  // ----- Growth re-send (ADR-0058 section 5.1) -------------------

  function membersOf(g: WasmDeviceGroup): LeafMember[] | null {
    try {
      return g.devices().map((m: { leafIndex: number; signaturePublicKey: Uint8Array }) => ({
        leaf: m.leafIndex,
        key: bytesToHex(m.signaturePublicKey),
      }));
    } catch {
      return null;
    }
  }

  async function maybeScheduleSnapshot(
    ownLeaf: number,
    priorLeaves: number[],
    minDelayMs = 0,
  ): Promise<void> {
    const wasm = await loadWasm();
    if (!wasm.shouldSendSnapshot(ownLeaf, Uint32Array.from(priorLeaves))) return;
    if (snapshotTimer !== null) return; // one re-send already waiting covers this
    snapshotTimer = setTimeout(() => {
      snapshotTimer = null;
      void sendSnapshot();
    }, Math.max(minDelayMs, snapshotDelayMs()));
  }

  /** Every part of the fold, each its own message with a fresh envelope
   *  stamp; the registers inside keep their ORIGINAL stamps. */
  async function sendSnapshot(): Promise<void> {
    if (synced === null || group === null || identity === null || deviceRemoved()) return;
    const parts = synced.snapshotParts();
    try {
      for (const part of parts) {
        if (group === null || identity === null) return;
        const bytes = group.createDeviceGroupSnapshotMessage(identity, part, mintHlc());
        await publishGroupMessage(bytes);
      }
    } catch (err) {
      // The next growth, or the new device's own writes, carry the state
      // forward; a failed re-send is not retried in a loop.
      // eslint-disable-next-line no-console
      console.warn("synced-state snapshot send failed", err);
    } finally {
      for (const part of parts) part.free();
    }
  }

  // ----- Inbound poll loop ---------------------------------------

  async function runPollLoop(
    g: WasmDeviceGroup,
    id: WasmIdentity,
    accountIdB64: string,
    deviceKey: CryptoKey,
    signal: AbortSignal,
  ): Promise<void> {
    const ds = new DsClient();
    const groupId = g.groupId();
    // Resume where the last persisted batch left off, instead of re-reading
    // the whole journal on every page load. The cursor is stored in the same
    // row and write as the snapshot the provider was just restored from, so it
    // never names a row that snapshot has not consumed.
    let cursor = 0;
    try {
      cursor = startingCursor(await getDeviceGroupJournalCursor(accountIdB64));
    } catch (err) {
      // Unreadable: start from the beginning. Re-reading is the safe direction.
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup journal cursor read failed; starting from 0", err);
    }
    /** When this loop started suspecting a fork (`fork-verdict.ts`). */
    let suspectSince: number | undefined;
    let forkReported = false;

    while (!signal.aborted) {
      // ADR-0057 obligation 2 - every device of the account holds the
      // DeviceGroup's root: claim the journal on open, once per session (an
      // unclaimed journal never answers the 401 the lazy claim waits for).
      // Asked on every pass, which costs nothing once settled, so a claim
      // attempted before the session's signer factory was installed, or one
      // that met a network failure, is retried (`ds/owner-claim.ts`).
      void ds.ensureOwnerClaim(groupId);
      let raw;
      try {
        raw = await ds.fetchGroup(groupId, {
          after: cursor,
          waitSecs: 25,
        });
      } catch (err) {
        if (signal.aborted) return;
        if (err instanceof DsHttpError && err.status === 404) {
          // Fresh journal slot — keep polling.
          continue;
        }
        if (err instanceof DsNetworkError) {
          // Transient — back off briefly + retry.
          await sleepCancellable(2000, signal);
          continue;
        }
        // Unexpected error — log + back off; we don't want a
        // single bad iteration to permanently kill the loop.
        // eslint-disable-next-line no-console
        console.warn("DeviceGroup poll loop fetch failed", err);
        await sleepCancellable(5000, signal);
        continue;
      }
      // Stopped while the long poll was out (the handle is being replaced):
      // apply nothing and persist nothing with this one.
      if (signal.aborted) return;

      // The journal answered, so this device is online: send what waited.
      if (outbox().length > 0) void drainFolderOutbox();

      // G-13 (ADR-0015): the 32-byte verifying key peers feed to
      // `processIncomingVerifyingRemovals`. Cached (24 h). Without it the
      // batch is HELD, not applied under a zero key: a zero key refuses a
      // removal the policy requires to be attested, and a refused row is
      // skipped for good once the cursor moves past it. A device removed that
      // way would stay an epoch behind, drain the journal, read as forked and
      // re-found the DeviceGroup over every healthy device (ADR-0057,
      // 2026-09-30). Holding keeps the removal for the next poll.
      let attestVk: Uint8Array;
      try {
        attestVk = await fetchDeviceAttestKey();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("device-attest key fetch failed; holding the DeviceGroup batch", err);
        await sleepCancellable(5000, signal);
        continue;
      }

      // ADR-0058 section 5.1 — who was here before this batch.
      const membersBefore = membersOf(g);

      const batch = await applyDeviceJournalBatch(raw.messages, cursor, {
        isOwnEcho: (rowId) => ownPublishedIds.has(rowId),
        apply: async (message) => {
          // Unwrap the DeviceGroup journal envelope (the commit / message plus
          // any removal attestation that travelled with it).
          const entry = decodeJournalEntry(message.payload);
          const attestation = entry.attestation ?? new Uint8Array(0);
          // Refuse-to-apply an unattested removal under the policy. A throw —
          // bad frame / wrong epoch / a removal refused for want of a valid
          // attestation — is logged by `onApplyError` and the batch goes on.
          const processed = g.processIncomingVerifyingRemovals(
            id,
            entry.message,
            attestation,
            attestVk,
            BigInt(Math.floor(Date.now() / 1000)),
          );
          if (processed.kind === "Application") {
            const data = processed.applicationData;
            if (data === undefined) return;
            try {
              const wasm = await loadWasm();
              const parsed = wasm.parseDeviceGroupPayload(data);
              try {
                applyParsed(parsed);
              } finally {
                parsed.free();
              }
            } catch (err) {
              // A payload refused by the decoder (ADR-0058 section 4) or of a
              // kind this build does not know is skipped; the next write of
              // those registers from any device supersedes it.
              // eslint-disable-next-line no-console
              console.warn("parseDeviceGroupPayload failed", err);
            }
          }
          // Commit / Proposal / ExternalJoinProposal — no application-state
          // effect; the ratchet already advanced inside the process call.
        },
        // The snapshot and the position it has consumed, in one write. A
        // failure holds the cursor (the rows come back next poll) rather than
        // recording a position the durable state has not reached.
        // A handle stopped while the batch was being applied (another handle
        // on this device changed the group) must not write its state: it is
        // an epoch behind what is on disk. Refusing holds the cursor.
        persist: (to) =>
          signal.aborted
            ? Promise.reject(new Error("DeviceGroup handle replaced; not persisting"))
            : persistDeviceGroupSnapshotWithCursor(id, accountIdB64, deviceKey, to),
        onApplyError: (message, err) => {
          // eslint-disable-next-line no-console
          console.warn("DeviceGroup journal row not applied", message.id, err);
        },
        onPersistError: (err) => {
          // eslint-disable-next-line no-console
          console.warn(
            "DeviceGroup snapshot + cursor persist failed; holding the cursor",
            err,
          );
        },
      });
      // ADR-0057 obligation 6 — no heal on apply: the device that committed
      // registered the epoch with the account's root before publishing.

      // A row in this batch removed THIS device (docs/ux/second-device.md
      // section 3.1). The batch that carried the removal was persisted above,
      // so the next launch finds the inactive snapshot and shows the same
      // screen; nothing after it is polled or persisted.
      if (!signal.aborted && pollStepAfterBatch(readDeviceGroupActive(g)) === "stop-removed") {
        markRemoved();
        return;
      }

      // A device joined in this batch: the lowest prior leaf re-sends.
      const membersAfter = membersOf(g);
      if (membersBefore !== null && membersAfter !== null) {
        const growth = detectGrowth(membersBefore, membersAfter);
        if (growth.grew) {
          try {
            void maybeScheduleSnapshot(g.ownLeafIndex(), growth.priorLeaves);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("growth re-send check failed", err);
          }
        }
      }

      if (batch.persisted) {
        cursor = batch.nextCursor;
        // Past the cursor on disk now, so these ids can never come back.
        for (const echoId of batch.skippedOwnEchoes) ownPublishedIds.delete(echoId);
      } else if (raw.messages.length > 0) {
        // Held: give storage a moment before the same rows are fetched again.
        await sleepCancellable(2000, signal);
      }

      // Forked off the account's chain? A drained read (no rows, nothing
      // more) while the DS names a newer epoch than this handle's means
      // nothing left in the journal can bring it forward (`fork-verdict.ts`).
      // The epoch is only derived when it can matter: a read with rows, or
      // one the DS named no epoch for, is in sync whatever the epoch is.
      const drained = raw.messages.length === 0 && !raw.hasMore;
      const latestEpoch = raw.latestEpoch === undefined ? undefined : BigInt(raw.latestEpoch);
      const localEpoch = drained && latestEpoch !== undefined ? deviceGroupEpoch(g, id) : null;
      const verdict =
        localEpoch === null
          ? ({ kind: "in-sync" } as const)
          : forkVerdict(localEpoch, drained, latestEpoch, suspectSince, Date.now());
      suspectSince = verdict.kind === "suspect" ? verdict.since : undefined;
      // The repair re-founds the account's DeviceGroup; it never runs on a
      // removed device. An inactive group cannot reach this point anyway (it
      // stopped above, and its epoch comes from a DS signer an inactive group
      // cannot derive, so `localEpoch` is null and the verdict in-sync); the
      // guard keeps it so if either of those changes.
      if (verdict.kind === "forked" && forkRepairAllowed(readDeviceGroupActive(g))) {
        if (forkRepaired.has(accountIdB64)) {
          if (!forkReported) {
            // eslint-disable-next-line no-console
            console.warn(
              `DeviceGroup still behind the DS after a repair this session: local epoch ${localEpoch}, DS epoch ${latestEpoch}; not repairing again`,
            );
            forkReported = true;
          }
        } else if (auth.identitySecrets()?.accountIdentitySeed === undefined) {
          // The re-founding claims (or resets) the journal with the account's
          // root, which needs the phrase seed. Without it nothing is deleted;
          // the loop keeps polling and repairs once the seed is available.
          if (!forkReported) {
            // eslint-disable-next-line no-console
            console.warn(
              `DeviceGroup forked: local epoch ${localEpoch}, DS epoch ${latestEpoch}; account root not available, not repairing yet`,
            );
            forkReported = true;
          }
        } else {
          // eslint-disable-next-line no-console
          console.warn(
            `DeviceGroup forked: local epoch ${localEpoch}, DS epoch ${latestEpoch}; re-founding it`,
          );
          // The repair waits for this loop to finish, so it is started, not
          // awaited, and the loop stops here.
          void repairForkedDeviceGroup(accountIdB64, cursor);
          return;
        }
      }
    }
  }

  /**
   * Re-found a DeviceGroup that has forked off the account's chain
   * (`fork-verdict.ts`): stop the poll loop, delete ONLY this account's
   * DeviceGroup snapshot (its journal cursor lives in the same row), found a
   * fresh DeviceGroup through `ensureDeviceGroup` - which claims the journal
   * with the account's root and, on the 409 the old chain answers, RESETS it
   * (ADR-0057 obligation 7) - and resume polling at the journal head as last
   * seen: the old group's rows cannot be read by the new one, re-reading them
   * only produces refused rows.
   *
   * What it keeps: the DeviceGroup snapshot store (`lakko-device-group-
   * snapshots`) is its own IndexedDB database, separate from the chat MLS
   * store, and the device identity seed, contacts and the synced-state row are
   * not touched. The device identity is the same seed and the same signature
   * key, so its conversation leaves in chats are unaffected.
   *
   * CONSEQUENCE: the reset removes every epoch key of the old chain, so every
   * OTHER device of the account loses the DeviceGroup - its reads answer 401 -
   * and must be added again from this one. A 401 is not a drained read, so
   * those devices never start a repair of their own.
   *
   * The founding is fail-closed (`ensureDeviceGroup`): a claim or reset that
   * does not land persists nothing, and the founding is tried again after
   * `BOOTSTRAP_RETRY_MS` while the session lasts; the next session founds too,
   * since the snapshot is gone.
   */
  async function repairForkedDeviceGroup(account: string, headCursor: number): Promise<void> {
    const accountBytes = accountIdBytesActive;
    if (activeAccountIdB64 !== account || accountBytes === null || group === null) return;
    if (forkRepaired.has(account)) return;
    if (deviceRemoved() || !forkRepairAllowed(readDeviceGroupActive(group))) return;
    forkRepaired.add(account);
    reloading = true;
    try {
      pollAbort?.abort();
      pollAbort = null;
      if (pollDone !== null) await pollDone;
      pollDone = null;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null || activeAccountIdB64 !== account) return;
      try {
        await deleteDeviceGroupSnapshot(account);
      } catch (err) {
        // The stale snapshot is still there, and founding over it would load
        // it again: keep following with the handle held, as before.
        // eslint-disable-next-line no-console
        console.warn("DeviceGroup snapshot delete failed; not re-founding", err);
        startPollLoop(account, deviceKey);
        return;
      }
      try {
        group?.free();
        identity?.free();
      } catch {
        // best-effort
      }
      group = null;
      identity = null;
      ownPublishedIds.clear();
      await installRefoundedDeviceGroup(account, accountBytes, deviceKey, headCursor);
    } finally {
      reloading = false;
    }
  }

  /** The founding half of {@link repairForkedDeviceGroup}, retried on its own
   *  while the session lasts when the claim or reset does not land. */
  async function installRefoundedDeviceGroup(
    account: string,
    accountBytes: Uint8Array,
    deviceKey: CryptoKey,
    headCursor: number,
  ): Promise<void> {
    if (activeAccountIdB64 !== account || group !== null || deviceRemoved()) return;
    let ensured;
    try {
      const seed = await ensureDeviceIdentitySeed(account, deviceKey);
      try {
        ensured = await ensureDeviceGroup(
          accountBytes,
          account,
          seed,
          deviceKey,
          auth.identitySecrets()?.accountIdentitySeed,
        );
      } finally {
        seed.fill(0);
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup re-founding failed; trying again later", err);
      setReady(false);
      setTimeout(() => {
        void installRefoundedDeviceGroup(account, accountBytes, deviceKey, headCursor);
      }, BOOTSTRAP_RETRY_MS);
      return;
    }
    if (activeAccountIdB64 !== account || group !== null) {
      ensured.group.free();
      ensured.identity.free();
      return;
    }
    if (!ensured.bootstrapped) {
      // eslint-disable-next-line no-console
      // A write from the stale handle (a publish that was already out) landed
      // after the delete. It is the forked state again; the next page load
      // detects it and repairs then.
      console.warn("DeviceGroup snapshot rewritten during the re-founding; not re-founded this session");
    }
    try {
      await persistDeviceGroupSnapshotWithCursor(ensured.identity, account, deviceKey, headCursor);
    } catch (err) {
      // The founding is on disk without a cursor: the loop starts from 0 and
      // the old group's rows are refused, which costs only the re-read.
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup re-founding cursor persist failed", err);
    }
    group = ensured.group;
    identity = ensured.identity;
    setReady(true);
    startPollLoop(account, deviceKey);
    void drainFolderOutbox();
  }

  function applyParsed(parsed: ParsedDeviceGroupPayload): void {
    if (parsed.kind === "serviceEnvelope") {
      // ADR-0060 section 7 - a Visibility, ReachState or (ADR-0048 section 4a)
      // own Profile envelope from another device of this account, ordered by
      // its own stamp (not the HLC) and not a synced register; anything else
      // inside is dropped by the intake.
      void receiveServiceEnvelope(parsed.serviceEnvelope);
      return;
    }
    if (synced === null) return;
    // Every kind - the four maps, folder changes, a catch-up snapshot - folds
    // through the one Rust fold, with its bytewise tiebreak on equal stamps.
    const changed = synced.applyPayload(parsed);
    try {
      observeHlc(synced.maxHlc());
    } catch {
      // best-effort
    }
    if (changed) refresh();
  }

  // ----- Outbound send paths -------------------------------------

  async function publishGroupMessage(wireBytes: Uint8Array): Promise<void> {
    if (group === null || identity === null || activeAccountIdB64 === null || reloading) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    if (deviceRemoved()) {
      throw new Error("this device was removed from the account");
    }
    const ds = new DsClient();
    // G-13 (ADR-0015): wrap in the journal envelope (application messages
    // carry no attestation).
    const published = await ds.publishGroup(
      group.groupId(),
      encodeJournalEntry(wireBytes, null),
    );
    // The DS hands this row straight back to the receive loop; remember its id
    // so the loop skips it rather than failing to decrypt its own message.
    ownPublishedIds.add(published.id);
    const deviceKey = await getDeviceKey(activeAccountIdB64);
    if (deviceKey !== null && identity !== null) {
      await persistDeviceGroupSnapshot(
        identity,
        activeAccountIdB64,
        deviceKey,
      );
    }
  }

  function requireReady(): { g: WasmDeviceGroup; id: WasmIdentity; s: WasmSyncedState } {
    if (group === null || identity === null || synced === null) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    return { g: group, id: identity, s: synced };
  }

  async function setDraftImpl(
    groupId: Uint8Array,
    text: string,
    replyToId?: Uint8Array,
  ): Promise<void> {
    const { g, id } = requireReady();
    const hlc = mintHlc();
    const reply = replyToId ?? new Uint8Array(0);
    const bytes = g.createDeviceGroupDraftMessage(id, groupId, text, reply, hlc);
    await publishGroupMessage(bytes);
    // A device never receives its own DeviceGroup messages: fold the write
    // in here, at the same stamp.
    if (synced?.applyDraft(hlc, groupId, text, reply) === true) refresh();
  }

  async function setFlagsImpl(
    groupId: Uint8Array,
    next: {
      readonly pinned: boolean;
      readonly muted: boolean;
      readonly archived: boolean;
      readonly unreadOverride: boolean;
    },
  ): Promise<void> {
    const { g, id } = requireReady();
    const hlc = mintHlc();
    const bytes = g.createDeviceGroupFlagsMessage(
      id,
      groupId,
      next.pinned,
      next.muted,
      next.archived,
      next.unreadOverride,
      hlc,
    );
    await publishGroupMessage(bytes);
    if (
      synced?.applyFlags(hlc, groupId, next.pinned, next.muted, next.archived, next.unreadOverride) ===
      true
    ) {
      refresh();
    }
  }

  async function setReadCursorImpl(
    groupId: Uint8Array,
    upToMessageId: Uint8Array,
    readAtMs: bigint,
  ): Promise<void> {
    const { g, id } = requireReady();
    const hlc = mintHlc();
    const bytes = g.createDeviceGroupReadCursorMessage(id, groupId, upToMessageId, readAtMs, hlc);
    await publishGroupMessage(bytes);
    if (synced?.applyReadCursor(hlc, groupId, upToMessageId, readAtMs) === true) refresh();
  }

  async function setOrderImpl(
    groupIds: readonly Uint8Array[],
  ): Promise<void> {
    const { g, id } = requireReady();
    const hlc = mintHlc();
    // Defensive copy — mutating the caller's arrays later must not affect
    // the wire bytes.
    const ids = groupIds.map((x) => new Uint8Array(x));
    const bytes = g.createDeviceGroupOrderMessage(id, ids, hlc);
    await publishGroupMessage(bytes);
    if (synced?.applyConversationOrder(hlc, ids.map((x) => new Uint8Array(x))) === true) {
      refresh();
    }
  }

  // ----- Folders (ADR-0058) ----------------------------------------

  async function applyFolderEditsImpl(edits: readonly RegisterEdit[]): Promise<void> {
    if (edits.length === 0) return;
    const { s } = requireReady();
    const wasm = await loadWasm();
    const stamped = stampEdits(edits, mintHlc);
    // Build (and so validate) before anything is applied: a broken bound
    // throws here with nothing changed.
    const cs = buildFolderChangeSet(wasm, stamped);
    try {
      s.applyFolderChanges(cs);
    } finally {
      cs.free();
    }
    setOutbox((q) => [...q, stamped]);
    refresh();
    void drainFolderOutbox();
  }

  function scheduleOutboxRetry(): void {
    if (outboxTimer !== null || deviceRemoved()) return;
    outboxTimer = setTimeout(() => {
      outboxTimer = null;
      void drainFolderOutbox();
    }, OUTBOX_RETRY_MS);
  }

  async function drainFolderOutbox(): Promise<void> {
    if (draining || outbox().length === 0 || deviceRemoved()) return;
    if (group === null || identity === null || reloading) {
      scheduleOutboxRetry();
      return;
    }
    draining = true;
    try {
      const wasm = await loadWasm();
      const queue = outbox();
      const result = await drainOutbox(
        queue,
        async (batch) => {
          if (group === null || identity === null) throw new Error("not ready");
          const cs = buildFolderChangeSet(wasm, batch);
          try {
            const bytes = group.createDeviceGroupFolderChangesMessage(identity, cs, mintHlc());
            await publishGroupMessage(bytes);
          } finally {
            cs.free();
          }
        },
        // A batch the builder refuses can never be sent; it already applied
        // here. Anything else (the network) is retried.
        (err) => err instanceof Error && /bound|folder id|not hex|invalid/i.test(err.message),
      );
      // Batches added while this ran are kept after what is still waiting.
      setOutbox((current) => [...result.remaining, ...current.slice(queue.length)]);
      if (result.failed) scheduleOutboxRetry();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("folder outbox drain failed", err);
      scheduleOutboxRetry();
    } finally {
      draining = false;
    }
  }

  // ----- Hook API ------------------------------------------------

  const api: DeviceGroupSyncApi = {
    ready,
    drafts,
    flags,
    readCursors,
    order,
    folders,
    pendingFolderSync: () => outbox().length > 0,
    deviceRemoved,
    getDraft(groupId) {
      return drafts()[groupIdHex(groupId)];
    },
    getFlags(groupId) {
      return flags()[groupIdHex(groupId)];
    },
    getReadCursor(groupId) {
      return readCursors()[groupIdHex(groupId)];
    },
    setDraft: setDraftImpl,
    setFlags: setFlagsImpl,
    setReadCursor: setReadCursorImpl,
    setOrder: setOrderImpl,
    applyFolderEdits: applyFolderEditsImpl,
  };

  return (
    <DeviceGroupSyncContext.Provider value={api}>
      {props.children}
    </DeviceGroupSyncContext.Provider>
  );
}

/** Hook — every component that reads or writes cross-device
 *  sync state calls this. Throws when invoked outside
 *  `<DeviceGroupSyncProvider>` (programmer error). */
export function useDeviceGroupSync(): DeviceGroupSyncApi {
  const ctx = useContext(DeviceGroupSyncContext);
  if (ctx === undefined) {
    throw new Error(
      "useDeviceGroupSync() called outside <DeviceGroupSyncProvider>",
    );
  }
  return ctx;
}

// ----- Helpers -----------------------------------------------------

/** The DeviceGroup's current epoch, or `null` when it cannot be read. The
 *  wasm DeviceGroup exposes no `epoch()` accessor; the DS access signer is
 *  derived at the group's current epoch and carries it. */
function deviceGroupEpoch(g: WasmDeviceGroup, id: WasmIdentity): bigint | null {
  const signer = takeSigner(() => g.dsAccessSigner(id));
  if (signer === null) return null;
  try {
    return signer.epoch;
  } finally {
    signer.free();
  }
}

function sleepCancellable(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}
