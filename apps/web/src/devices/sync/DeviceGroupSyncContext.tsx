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
import { SyncStatePersister, type CapturedSyncState, type SyncStateCapture } from "./sync-state-persister";
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
  DeviceGroupNotJoinedError,
  deviceGroupIdOf,
  deviceGroupLineageSeed,
  ensureDeviceGroup,
  persistDeviceGroupSnapshotWithCursor,
  type EnsuredDeviceGroup,
} from "../lifecycle";
import {
  DeviceGroupSnapshotChangedError,
  deleteDeviceGroupPending,
  dropDeviceGroupStateForRejoin,
  getDeviceGroupJournalCursor,
  getDeviceGroupPending,
  putDeviceGroupPending,
} from "../device-group-snapshot-store";
import { applyDeviceJournalBatch, startingCursor } from "./journal-batch";
import { BindingEpoch, StaleBindingError } from "../../auth/binding-epoch";
import {
  KNOCK_CHECK_INTERVAL_MS,
  REKNOCK_AFTER_MS,
  admissionVerdict,
  decodeAdmissionRecord,
  decodeRejoinState,
  encodeAdmissionRecord,
  encodeRejoinState,
  isBehind,
  newAdmissionRecord,
  noteAdded,
  noteHandled,
  rejoinOnce,
  type AdmissionRecord,
  type RejoinPorts,
} from "../device-group-rejoin";
import {
  commitDeviceGroupChange,
  resendOwedDeviceGroupCommitFor,
} from "../device-group-commit-runtime";
import { DiscoveryClient } from "../../discovery/client";
import {
  buildPairingQrPayload,
  buildRejoinKnockPayload,
  generateRendezvousPseudonym,
  ownPairingHandle,
  parsePairingQrPayload,
} from "../pairing";
import { pollForWelcome } from "../welcome-courier";
import { base64Decode } from "../../auth/account-id";
import {
  isDeviceRemoved,
  pollStepAfterBatch,
  readDeviceGroupActive,
  rejoinAllowed,
} from "./removed-device";
import { decodeJournalEntry, encodeJournalEntry } from "../journal-envelope";
import { fetchDeviceAttestKey } from "../../auth/device-attest-key";
import { mintHlc, observeHlc } from "./hlc";
import type { LineageEntry } from "../../chat/lineage";
import {
  installAddressRegister,
  publishHeldAddress,
} from "../../auth/protect/address-register";
import { installUsernameRegister, publishRegisterUsername } from "../../auth/username-register";
import { receiveServiceEnvelope } from "../../reach/service-envelope-intake";
import {
  bindDeviceCards,
  noteDeviceAction,
  noteDeviceForeground,
  pruneOwnDeviceCards,
  scheduleOwnCardResend,
  unbindDeviceCards,
} from "../device-card-runtime";
import { receiveDeviceCardEnvelope, senderKeyAtReceipt } from "../device-card-intake";
import { linkClaimRecord, linkRequestKeyHex } from "../../contacts/link-claims";
import { base64Encode } from "../../auth/account-id";
import { journalReadability, type JournalReadability } from "../../contacts/journal-readability";
import { sealKeyRotationAfterRemoval } from "../key-rotation";
import { installKeyRotationPort, type JournalCatchUp } from "./key-rotation-port";
import {
  flushServiceEnvelopes,
  registerServiceEnvelopeSender,
  retryDirtyServiceEnvelopes,
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
  type EndedConversation,
  type SavedEntry,
} from "./synced-projection";
import { drainOutbox, type OutboxBatch } from "./folder-outbox";
import { publishOwnDeviceRoster } from "../own-device-roster";
import {
  detectGrowth,
  onDeviceGroupChangedHere,
  snapshotDelayMs,
  type LeafMember,
} from "./growth";
import { onRestoredSyncedState, registerSyncedStateSource } from "./synced-state-source";
import {
  readLinkRecords,
  type LinkAnswerVerdict,
  type LinkRecordModel,
  type NewLinkRecord,
} from "./link-records";
import {
  bytesToHex,
  stampEdits,
  type ConversationRef,
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
/** The DeviceGroup's membership at one moment. Keys are the devices'
 *  signature public keys, hex. */
export interface DeviceRoster {
  readonly keys: readonly string[];
  /** This device's own key. */
  readonly ownKey: string;
  /** Keys that entered through a change made ON THIS DEVICE (a pairing or
   *  a confirmation run here): the devices this device let in. */
  readonly addedHere: readonly string[];
}

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
  /** ADR-0065 section 4 — reactive: the account's saved messages, most
   *  recently saved first, from every device. */
  readonly saved: Accessor<readonly SavedEntry[]>;
  /** Reactive: every blob id a saved message names (lower-case hex). */
  readonly savedBlobIdsHex: Accessor<readonly string[]>;
  /** ADR-0087 section 9 — reactive: the conversations whose register says
   *  this account is out of them, from every device. */
  readonly endedConversations: Accessor<readonly EndedConversation[]>;
  /** Reactive: the conversation lineage registers (ADR-0073 R.5): which
   *  group continues which conversation. Read through `chat/lineage.ts`. */
  readonly lineage: Accessor<readonly LineageEntry[]>;
  /** Reactive: another device removed this one from the account, and this
   *  device has applied that removal (its DeviceGroup is inactive). The poll
   *  loop has stopped for good; the app shell shows "This device was
   *  removed" (docs/ux/second-device.md section 3.1). Reset on sign-out and
   *  on an account switch. */
  readonly deviceRemoved: Accessor<boolean>;
  /** Reactive: the account's devices as this device's DeviceGroup holds
   *  them, or `null` while the handle is not loaded. Feeds the "A device
   *  was added to your account" notice (`devices/added-notice.ts`). */
  readonly deviceRoster: Accessor<DeviceRoster | null>;

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

  /**
   * ADR-0087 section 9 — this account's membership of a conversation ended,
   * or it joined one again, at MLS `epoch` (`0n` for a channel). Fold the
   * register, keep it on disk and send it to the account's other devices.
   * `false` when there is nothing to tell them (the core's compose returned
   * nothing), which never stops the wipe. Rejects while the synced state is
   * not loaded, or when the row could not be written; a send failure is not
   * an error, the register waits in the outbox.
   */
  writeConversationEnd(
    ref: ConversationRef,
    state: "ended" | "joined",
    epoch: bigint,
  ): Promise<boolean>;

  /**
   * ADR-0073 R.5 — record that `successorGroupId` continues the conversation
   * `rootConversationId`. Write-once: `false` when this device already holds
   * a register for the successor (nothing is written or sent). On `true` the
   * register is folded here at once and its envelope is sent to the account's
   * other devices over the DeviceGroup's `ServiceEnvelope`, now if this
   * device is online, at the next start if not. Never throws for a failed
   * send; `false` also when the runtime has not started.
   */
  setLineage(successorGroupId: Uint8Array, rootConversationId: Uint8Array): Promise<boolean>;

  /** ADR-0079 section 4 — reactive: the account's own invite links, every
   *  device's, revoked ones included. */
  readonly linkRecords: Accessor<readonly LinkRecordModel[]>;
  /** Record a link just minted here, fold it at once and send it to the
   *  account's other devices. `false` when the runtime has not started.
   *  Never throws for a failed send (it is sent again at the next start). */
  setLinkRecord(record: NewLinkRecord): Promise<boolean>;
  /** Mark a link revoked (final), fold and send. `false` when nothing
   *  changed. */
  revokeLinkRecord(linkId: Uint8Array): Promise<boolean>;
  /** Mark a link copied (2026-10-26): it is no longer editable on any of
   *  the account's devices. Fold, persist and send. */
  markLinkShared(linkId: Uint8Array): Promise<boolean>;
  /** Count one request accepted through a link, remembering the requester's
   *  account key, fold and send. */
  recordLinkUse(linkId: Uint8Array, requesterKey: Uint8Array): Promise<boolean>;
  /** The one rule for a request's link answer (ADR-0079 section 3 as
   *  amended 2026-10-19), asked of the fold: the link's expiry is judged at
   *  `pressedAtMs` (the time the DS stamped on the request row), its
   *  revocation and uses now. `null` while the fold is not loaded: the
   *  request stays undecided (no timer) and is decided once {@link ready}
   *  turns `true`. */
  linkAnswerVerdictAt(
    linkId: Uint8Array,
    linkSecret: Uint8Array,
    requesterKey: Uint8Array,
    pressedAtMs: number,
  ): LinkAnswerVerdict | null;
  /** ADR-0079 (reworked 2026-10-17) - seal and post this device's claim of
   *  a link request into the account's DeviceGroup. `beforePublish` gets
   *  the sealed bytes (hex) and must record them durably (throwing aborts
   *  the publish). The publish answer files nothing: the receive loop
   *  judges the row at its position (it counts only when sealed at the
   *  journal's epoch there). Throws when the runtime is not ready or the
   *  publish fails; a failed publish may still have landed, which the loop
   *  then recognises by the recorded bytes. */
  postLinkClaim(
    linkId: Uint8Array,
    requesterKey: Uint8Array,
    beforePublish: (wireHex: string) => void,
  ): Promise<number>;
  /** ADR-0079 (order of 2026-10-17) - publish the link-founded note (the
   *  request, the one-to-one founded for it, its Welcome) into the
   *  account's DeviceGroup, BEFORE any Welcome is written. Resolves once the
   *  publish answered; throws when it failed. */
  postLinkFounded(
    linkId: Uint8Array,
    requesterKey: Uint8Array,
    groupId: Uint8Array,
    welcome: Uint8Array,
    beforePublish: (wireHex: string) => void,
  ): Promise<number>;
  /** ADR-0079: this device's DeviceGroup leaf key and the current roster's
   *  leaf keys, the inputs of the claim decision. `null` while not ready. */
  linkClaimRoster(): { readonly ownKey: Uint8Array; readonly roster: readonly Uint8Array[] } | null;
  /** Wait until the DeviceGroup journal has been read to its head by a pass
   *  that started after this call (so every row up to a claim just posted is
   *  applied). `"read"` when it was. */
  catchUpJournal(): Promise<JournalCatchUp>;
  /** Reactive: bumped whenever a claim from another device of the account
   *  is recorded. */
  readonly linkClaimsVersion: Accessor<number>;
}

/** How long a catch-up waits for the journal's head before it reports no
 *  answer. Chosen, not measured. */
const CATCH_UP_TIMEOUT_MS = 40_000;
/** How long the envelope waits for this handle to be reloaded after the
 *  removal. Chosen, not measured. */
const HANDLE_RELOAD_WAIT_MS = 10_000;

/** The leaf signature keys of a DeviceGroup, or `null` when it cannot be read. */
function deviceKeysOf(g: WasmDeviceGroup): Uint8Array[] | null {
  try {
    return (g.devices() as { signaturePublicKey: Uint8Array }[]).map((m) => m.signaturePublicKey);
  } catch {
    return null;
  }
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
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
  const [lineage, setLineageEntries] = createSignal<readonly LineageEntry[]>([]);
  const [saved, setSaved] = createSignal<readonly SavedEntry[]>([]);
  const [savedBlobIdsHex, setSavedBlobIdsHex] = createSignal<readonly string[]>([]);
  const [endedConversations, setEndedConversations] = createSignal<readonly EndedConversation[]>([]);
  const [linkRecords, setLinkRecords] = createSignal<readonly LinkRecordModel[]>([]);
  const [linkClaimsVersion, setLinkClaimsVersion] = createSignal(0);
  const [outbox, setOutbox] = createSignal<readonly OutboxBatch[]>([]);
  const [ready, setReady] = createSignal(false);
  const [deviceRemoved, setDeviceRemoved] = createSignal(false);
  const [deviceRoster, setDeviceRoster] = createSignal<DeviceRoster | null>(null);
  /** Publish the roster of the held handle. `localChange`: the handle was
   *  just reloaded after a change made on this device, so what is new in it
   *  was let in here. A read that fails publishes nothing. */
  function publishRoster(localChange: boolean): void {
    const g = group;
    if (g === null || deviceRemoved()) return;
    const members = membersOf(g);
    if (members === null || members.length === 0) return;
    let ownLeaf: number;
    try {
      ownLeaf = g.ownLeafIndex();
    } catch {
      return;
    }
    const own = members.find((m) => m.leaf === ownLeaf);
    if (own === undefined) return;
    const before = untrack(deviceRoster);
    const keys = members.map((m) => m.key);
    // ADR-0074 section 2.3 — the roster every own-device admission is
    // checked against (`own-device-roster.ts`).
    if (activeAccountIdB64 !== null) {
      publishOwnDeviceRoster({ accountIdB64: activeAccountIdB64, keysHex: keys });
    }
    // ADR-0080: a leaf that left takes its card with it.
    pruneOwnDeviceCards(keys);
    const addedHere =
      localChange && before !== null ? keys.filter((k) => !before.keys.includes(k)) : [];
    if (
      before !== null &&
      addedHere.length === 0 &&
      before.ownKey === own.key &&
      before.keys.length === keys.length &&
      keys.every((k) => before.keys.includes(k))
    ) {
      return;
    }
    setDeviceRoster({ keys, ownKey: own.key, addedHere });
  }
  /** Bumped on every change worth writing to disk. */
  const [stateVersion, setStateVersion] = createSignal(0);

  // Long-lived handles for the active session. `null` between
  // sessions. The poll loop reads them every iteration; the
  // send paths reads them per call.
  let identity: WasmIdentity | null = null;
  let group: WasmDeviceGroup | null = null;
  /** The one fold of the account's synced state (ADR-0058). */
  let synced: WasmSyncedState | null = null;
  /** The loaded wasm module, for the synchronous receive path. */
  let wasmSync: Awaited<ReturnType<typeof loadWasm>> | null = null;
  let uninstallAddressRegister: (() => void) | null = null;
  let uninstallUsernameRegister: (() => void) | null = null;
  void loadWasm().then((w) => {
    wasmSync = w;
  });
  let pollAbort: AbortController | null = null;
  let pollDone: Promise<void> | null = null;
  let activeAccountIdB64: string | null = null;
  /** Bumped by every `teardown`. Work started under one binding (a bootstrap,
   *  a handle reload, a rejoin, an outbox drain) captures it and, after
   *  each await, stops when it has moved: the account it was working for has
   *  signed out, and anything it assigned would belong to nobody or - after
   *  another sign-in - to somebody else. A counter rather than the account id
   *  alone, because signing out and back in to the SAME account is also a new
   *  binding with new handles. */
  const binding = new BindingEpoch();
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
  /** The generation of the stored DeviceGroup snapshot the long-lived handle
   *  was read from or last wrote (`device-group-snapshot-store.ts`). Every
   *  write of this handle is conditional on it: a commit of this device made
   *  through another handle replaces the row, and this handle - an epoch
   *  behind - must not write over it (ADR-0081 decision 3). */
  let handleGeneration: number | null = null;
  /** The handle's snapshot writes, one at a time. The receive loop and a
   *  publish both write through this handle; run side by side, both named
   *  the same generation and the second was refused as if another handle
   *  had written (seen at signup: the username and profile sends failed). */
  let handleWrites: Promise<unknown> = Promise.resolve();
  function writeHandleSnapshot(
    id: WasmIdentity,
    accountIdB64: string,
    deviceKey: CryptoKey,
    cursor: number | undefined,
    stillThisHandle: () => boolean,
  ): Promise<number> {
    // The generation is read and recorded inside the queue, so the next write
    // always names the one this write produced.
    const run = handleWrites.then(async () => {
      const written = await persistDeviceGroupSnapshotWithCursor(
        id,
        accountIdB64,
        deviceKey,
        cursor,
        handleGeneration ?? undefined,
      );
      if (stillThisHandle()) handleGeneration = written;
      return written;
    });
    handleWrites = run.catch(() => undefined);
    return run;
  }
  /** ADR-0081 decision 4 - the rejoin in progress, if any. */
  let rejoinAbort: AbortController | null = null;
  /** The sibling's admission record (`device-group-rejoin.ts`), loaded with
   *  the handle. */
  let admissions: AdmissionRecord | null = null;

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
  // Keyed on the DeviceGroup id derived from the account seed (ADR-0078
  // section 6, `deviceGroupIdOf`), set before the group is ensured, so the
  // founding claim finds the root too.
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
    setLineageEntries(p.lineage);
    setSaved(p.saved);
    setSavedBlobIdsHex(p.savedBlobIdsHex);
    setEndedConversations(p.endedConversations);
    if (wasmSync !== null) setLinkRecords(readLinkRecords(synced, wasmSync.LinkRecordKind.Contact));
    // ADR-0075 (amended 2026-10-13): the recovery address the account's
    // devices keep, for the screens above this provider.
    if (activeAccountIdB64 !== null) {
      publishHeldAddress(activeAccountIdB64, synced.recoveryEmail() ?? null);
      // ADR-0076: the account's username as its devices agree on it.
      publishRegisterUsername(activeAccountIdB64, synced.ownUsername() ?? null);
    }
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

  // ADR-0080 section 3: the person's actions on this device (a tap, a key)
  // are what refreshes this device's card; never a timer. Hidden pages are
  // ignored inside `noteDeviceAction`.
  if (typeof document !== "undefined") {
    const onAction = (): void => noteDeviceAction();
    // A growth re-send that fell due while the page was hidden goes at the
    // next foreground (it is not an action: no fresh card is decided here).
    const onVisible = (): void => {
      if (document.visibilityState === "visible") noteDeviceForeground();
    };
    document.addEventListener("pointerdown", onAction, { capture: true, passive: true });
    document.addEventListener("keydown", onAction, { capture: true });
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onAction, { capture: true });
      document.removeEventListener("keydown", onAction, { capture: true });
      document.removeEventListener("visibilitychange", onVisible);
    });
  }

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
          // ADR-0080: every device that was there re-sends its own card.
          scheduleOwnCardResend();
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
    // ADR-0080: the device cards of this account (stored, own, the switch).
    bindDeviceCards(accountIdB64);
    // Everything below runs across awaits a sign-out can land in. `current`
    // is asked after each one; a stale bootstrap assigns nothing, starts
    // nothing and frees what it built.
    const current = binding.capture();
    const deviceKey = await getDeviceKey(accountIdB64);
    if (!current()) return;
    if (deviceKey === null) {
      // No device key — recovery / forget-device race. Bail;
      // the next session transition retries.
      activeAccountIdB64 = null;
      return;
    }
    let ownId: Uint8Array | null = null;
    try {
      ownId = await deviceGroupIdOf(accountIdB64, deviceKey, auth.identitySecrets()?.accountIdentitySeed);
    } catch {
      ownId = null;
    }
    if (!current()) return;
    ownDeviceGroupId = ownId;
    accountIdBytesActive = accountIdBytes;

    let ensured;
    try {
      const seed = await ensureDeviceIdentitySeed(accountIdB64, deviceKey);
      try {
        ensured = await ensureDeviceGroup(
          accountIdBytes,
          accountIdB64,
          seed,
          deviceKey,
          auth.identitySecrets()?.accountIdentitySeed,
          { mayFound: () => directoryListsNoOtherDevice(accountIdBytes, seed) },
        );
      } finally {
        seed.fill(0);
      }
    } catch (err) {
      if (err instanceof DeviceGroupNotJoinedError && err.reason === "rejoin") {
        // ADR-0081 decision 4: the account has other devices; one of them
        // admits this device again.
        if (!current()) return;
        startRejoin(accountIdB64, accountIdBytes, deviceKey, current);
        return;
      }
      // eslint-disable-next-line no-console
      console.warn("DeviceGroupSyncProvider bootstrap failed", err);
      // The session this bootstrap was for is gone: its failure says nothing
      // about the binding that is current now, and must not unbind it.
      if (!current()) return;
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

    if (!current()) {
      // Signed out while the group was being read or founded. The handles
      // belong to no session; holding them would leak them, and assigning
      // them would hand the next account this one's identity.
      freeHandles(ensured.group, ensured.identity);
      return;
    }
    identity = ensured.identity;
    group = ensured.group;
    handleGeneration = ensured.generation;

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
    // From here on a stale return frees nothing: `teardown` is what made it
    // stale, and it already freed the handles assigned above.
    const wasm = await loadWasm();
    if (!current()) return;
    // A write the previous session flushed on Log out may still be in flight;
    // read after it, so a sign-in right after a sign-out finds the last edit.
    await persister.idle();
    if (!current()) return;
    boundSyncGeneration = syncStateGeneration(accountIdB64);
    let restored: Awaited<ReturnType<typeof loadSyncState>> = null;
    try {
      restored = await loadSyncState(accountIdB64, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device-sync state restore failed; starting empty", err);
    }
    // The row just read is this account's drafts, flags and folders; shown
    // under another session they would be another person's.
    if (!current()) return;
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
    publishRoster(false);

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

    await loadAdmissions(accountIdB64, deviceKey);
    if (!current()) return;
    startPollLoop(accountIdB64, deviceKey);
    void drainFolderOutbox();
    void flushUnsentLineage();
    void flushUnsentRecoveryEmail();
    // ADR-0075: the synced state is loaded, so the recovery address can be
    // written (a write that waited for this is applied now).
    uninstallAddressRegister?.();
    uninstallAddressRegister = installAddressRegister({ accountIdB64, set: setRecoveryEmailImpl });
    void flushUnsentOwnUsername();
    void flushUnsentLinkRecords();
    uninstallUsernameRegister?.();
    uninstallUsernameRegister = installUsernameRegister({ accountIdB64, set: setOwnUsernameImpl });
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
    publishOwnDeviceRoster(null);
    // ADR-0074 section 2.3: an inactive DeviceGroup is proof of removal by a
    // device of the account. The device signs out and wipes everything by
    // itself; the removed screen (UX 18.2) is the auth layer's from here.
    if (activeAccountIdB64 !== null) auth.reportDeviceRemoved(activeAccountIdB64);
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
    const current = binding.capture();
    reloading = true;
    try {
      pollAbort?.abort();
      pollAbort = null;
      if (pollDone !== null) await pollDone;
      if (!current()) return;
      pollDone = null;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null || !current()) return;
      const seed = await ensureDeviceIdentitySeed(account, deviceKey);
      if (!current()) {
        seed.fill(0);
        return;
      }
      const ensured = await ensureDeviceGroup(accountBytes, account, seed, deviceKey);
      seed.fill(0);
      if (!current()) {
        freeHandles(ensured.group, ensured.identity);
        return;
      }
      try {
        group?.free();
        identity?.free();
      } catch {
        // best-effort
      }
      group = ensured.group;
      identity = ensured.identity;
      handleGeneration = ensured.generation;
      ownPublishedIds.clear();
      if (isDeviceRemoved(readDeviceGroupActive(group))) {
        markRemoved();
        return;
      }
      // What is new in the reloaded handle was added on this device.
      publishRoster(true);
      startPollLoop(account, deviceKey);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup handle reload failed", err);
    } finally {
      // Only this binding's flag: a later session may be reloading too.
      if (current()) reloading = false;
    }
    if (!current()) return;
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
    binding.advance();
    reloading = false;
    rejoinAbort?.abort();
    rejoinAbort = null;
    handleGeneration = null;
    admissions = null;
    unbindDeviceCards();
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
    uninstallAddressRegister?.();
    uninstallAddressRegister = null;
    uninstallUsernameRegister?.();
    uninstallUsernameRegister = null;
    activeAccountIdB64 = null;
    boundSyncGeneration = null;
    ownDeviceGroupId = null;
    accountIdBytesActive = null;
    ownPublishedIds.clear();
    setReady(false);
    setDeviceRemoved(false);
    setDeviceRoster(null);
    publishOwnDeviceRoster(null);
    setDrafts({});
    setFlags({});
    setReadCursors({});
    setOrder(null);
    setFolders([]);
    setLineageEntries([]);
    setSaved([]);
    setSavedBlobIdsHex([]);
    setEndedConversations([]);
    setLinkRecords([]);
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
    persister.schedule(account, syncStateCapture(account));
  });

  /** Reads the newest fold and outbox for `account` when the write runs, or
   *  `null` once the account is no longer bound. It may run inside
   *  `teardown`, which an effect calls, so it reads untracked. */
  function syncStateCapture(account: string): SyncStateCapture {
    return (): CapturedSyncState | null =>
      untrack(() => {
        const generation = boundSyncGeneration;
        if (synced === null || activeAccountIdB64 !== account || deviceRemoved()) return null;
        if (generation === null || syncStateGeneration(account) !== generation) return null;
        return { row: { synced: synced.toBytes(), pending: outbox() }, generation };
      });
  }

  /**
   * Write the stored row now, past the debounce. `false` when it could not
   * be written. An invite link's record must be on disk before the link is
   * handed out: this device never reads its own record back from the
   * journal, so a page closed within the debounce forgot the link, and every
   * request through it then waited for a verdict that never came (seen on
   * a local stack 2026-10-22: a reload right after making a link left the
   * requester on "will get your messages when their app is next online").
   */
  async function persistSyncedNow(): Promise<boolean> {
    const account = activeAccountIdB64;
    if (account === null || synced === null || deviceRemoved()) return false;
    try {
      await persister.writeNow(account, syncStateCapture(account));
      return true;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("device-sync state not written", err instanceof Error ? err.name : "");
      return false;
    }
  }

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
    // The parts are THIS account's fold. Each send awaits the network; the
    // handle is re-checked by identity before the next part is sealed, so a
    // part is never sealed into the DeviceGroup of whoever signed in since.
    const g = group;
    const id = identity;
    const parts = synced.snapshotParts();
    try {
      for (const part of parts) {
        if (group !== g || identity !== id) return;
        const bytes = g.createDeviceGroupSnapshotMessage(id, part, mintHlc());
        await publishGroupMessage(bytes);
      }
      // ADR-0073 R.13a - the lineage is not in `snapshotParts()`: a catch-up
      // is the snapshot parts PLUS every lineage envelope, each with its
      // original stamp.
      await sendLineageEnvelopes(synced.lineageEnvelopes());
      // ADR-0075 - the recovery-email register is not in `snapshotParts()`
      // either: its envelope rides beside them, with its original stamp.
      const recoveryEmail = synced.recoveryEmailEnvelope();
      if (recoveryEmail !== undefined) await sendLineageEnvelopes([recoveryEmail]);
      // ADR-0076 - and the own-username register, the same way.
      const ownUsername = synced.ownUsernameEnvelope();
      if (ownUsername !== undefined) await sendLineageEnvelopes([ownUsername]);
      // ADR-0079 - and every link record, each with its original stamp.
      await sendLineageEnvelopes(synced.linkRecordEnvelopes());
    } catch (err) {
      // The next growth, or the new device's own writes, carry the state
      // forward; a failed re-send is not retried in a loop.
      // eslint-disable-next-line no-console
      console.warn("synced-state snapshot send failed", err);
    } finally {
      for (const part of parts) part.free();
    }
  }

  // ----- ADR-0073 R.5: the lineage register ------------------------

  /** `localStorage` marker: a lineage register written here has not been
   *  confirmed sent. Set before the publish, cleared after it, so a device
   *  that was offline (or closed mid-send) sends every register again at its
   *  next start. Re-sending is idempotent: the fold keeps the first stamp. */
  const lineageUnsentKey = (account: string): string => `lakko.lineage.unsent.v1.${account}`;

  async function sendLineageEnvelopes(envelopes: readonly Uint8Array[]): Promise<void> {
    for (const envelope of envelopes) {
      if (group === null || identity === null || reloading || deviceRemoved()) {
        throw new Error("DeviceGroupSyncProvider is not ready");
      }
      await publishGroupMessage(group.createServiceEnvelopeMessage(identity, envelope, mintHlc()));
    }
  }

  // ----- ADR-0075: the recovery-email register --------------------

  /** `localStorage` marker: the register written here has not been confirmed
   *  sent (it holds no address, only the fact). The same rule as the lineage
   *  marker; re-sending is idempotent (last writer wins by stamp). */
  const recoveryEmailUnsentKey = (account: string): string => `lakko.recovery-email.unsent.v1.${account}`;

  async function sendRecoveryEmailRegister(account: string): Promise<void> {
    if (synced === null) return;
    try {
      const envelope = synced.recoveryEmailEnvelope();
      if (envelope !== undefined) await sendLineageEnvelopes([envelope]);
      localStorage.removeItem(recoveryEmailUnsentKey(account));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "recovery-email register send failed; retried at the next start",
        err instanceof Error ? err.name : "unknown",
      );
    }
  }

  /** Send the register again when an earlier send was not confirmed. */
  async function flushUnsentRecoveryEmail(): Promise<void> {
    const account = activeAccountIdB64;
    if (account === null || synced === null) return;
    try {
      if (localStorage.getItem(recoveryEmailUnsentKey(account)) === null) return;
    } catch {
      return;
    }
    await sendRecoveryEmailRegister(account);
  }

  /** Write the register (`null`: no recovery email), fold it here at once
   *  and send it to the account's other devices. Never throws; the address
   *  is never logged. */
  async function setRecoveryEmailImpl(address: string | null): Promise<void> {
    if (synced === null) return;
    let written: boolean;
    try {
      written = synced.setRecoveryEmail(mintHlc(), address);
    } catch {
      // An address the one rule refuses: nothing is kept.
      return;
    }
    if (!written) return;
    refresh();
    const account = activeAccountIdB64;
    if (account === null) return;
    try {
      localStorage.setItem(recoveryEmailUnsentKey(account), "1");
    } catch {
      /* storage unavailable: the send below is the only attempt */
    }
    await sendRecoveryEmailRegister(account);
  }

  // ----- ADR-0076: the own-username register ----------------------

  const ownUsernameUnsentKey = (account: string): string => `lakko.own-username.unsent.v1.${account}`;

  async function sendOwnUsernameRegister(account: string): Promise<void> {
    if (synced === null) return;
    try {
      const envelope = synced.ownUsernameEnvelope();
      if (envelope !== undefined) await sendLineageEnvelopes([envelope]);
      localStorage.removeItem(ownUsernameUnsentKey(account));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "own-username register send failed; retried at the next start",
        err instanceof Error ? err.name : "unknown",
      );
    }
  }

  async function flushUnsentOwnUsername(): Promise<void> {
    const account = activeAccountIdB64;
    if (account === null || synced === null) return;
    try {
      if (localStorage.getItem(ownUsernameUnsentKey(account)) === null) return;
    } catch {
      return;
    }
    await sendOwnUsernameRegister(account);
  }

  /** Write the register, fold it here at once and send it to the account's
   *  other devices. Never throws. */
  async function setOwnUsernameImpl(username: string): Promise<void> {
    if (synced === null) return;
    let written: boolean;
    try {
      written = synced.setOwnUsername(mintHlc(), username);
    } catch {
      // A name the username rule refuses: nothing is kept.
      return;
    }
    if (!written) return;
    refresh();
    const account = activeAccountIdB64;
    if (account === null) return;
    try {
      localStorage.setItem(ownUsernameUnsentKey(account), "1");
    } catch {
      /* storage unavailable: the send below is the only attempt */
    }
    await sendOwnUsernameRegister(account);
  }

  // ----- ADR-0079: the link-record register -----------------------

  /** `localStorage` marker: a link record written here has not been
   *  confirmed sent (it holds no link, only the fact). Re-sending every
   *  record is idempotent: the fold keeps the winning stamp. */
  const linkRecordsUnsentKey = (account: string): string => `lakko.link-records.unsent.v1.${account}`;

  async function sendLinkRecordEnvelopes(account: string, envelopes: readonly Uint8Array[]): Promise<void> {
    try {
      await sendLineageEnvelopes(envelopes);
      localStorage.removeItem(linkRecordsUnsentKey(account));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "link-record register send failed; retried at the next start",
        err instanceof Error ? err.name : "unknown",
      );
    }
  }

  async function flushUnsentLinkRecords(): Promise<void> {
    const account = activeAccountIdB64;
    if (account === null || synced === null) return;
    try {
      if (localStorage.getItem(linkRecordsUnsentKey(account)) === null) return;
    } catch {
      return;
    }
    await sendLinkRecordEnvelopes(account, synced.linkRecordEnvelopes());
  }

  /** Fold a write here at once, mark it unsent and send the one record. */
  async function afterLinkRecordWrite(linkId: Uint8Array): Promise<void> {
    refresh();
    const account = activeAccountIdB64;
    if (account === null || synced === null) return;
    try {
      localStorage.setItem(linkRecordsUnsentKey(account), "1");
    } catch {
      /* storage unavailable: the send below is the only attempt */
    }
    const envelope = synced.linkRecordEnvelopeFor(linkId);
    if (envelope !== undefined) await sendLinkRecordEnvelopes(account, [envelope]);
  }

  async function setLinkRecordImpl(record: NewLinkRecord): Promise<boolean> {
    if (synced === null || wasmSync === null) return false;
    let written: boolean;
    try {
      const r = new wasmSync.LinkRecord(
        record.linkId,
        record.linkSecret,
        record.kind === "contact" ? wasmSync.LinkRecordKind.Contact : wasmSync.LinkRecordKind.Call,
        record.ledgerId,
        record.revocationSeed,
        BigInt(Math.max(0, Math.floor(record.expiryMs))),
        record.maxUses >>> 0,
        BigInt(Math.max(0, Math.floor(record.mintedAtMs))),
        record.label,
      );
      try {
        written = synced.setLinkRecord(mintHlc(), r);
      } finally {
        r.free();
      }
    } catch {
      // A record out of bounds: nothing is kept.
      return false;
    }
    if (!written) return false;
    // On disk before the caller hands the link out; a record that could not
    // be written mints no link (the caller revokes it).
    refresh();
    if (!(await persistSyncedNow())) return false;
    await afterLinkRecordWrite(record.linkId);
    return true;
  }

  async function revokeLinkRecordImpl(linkId: Uint8Array): Promise<boolean> {
    if (synced === null) return false;
    let written: boolean;
    try {
      written = synced.revokeLinkRecord(mintHlc(), linkId);
    } catch {
      return false;
    }
    if (written) {
      refresh();
      await persistSyncedNow();
      await afterLinkRecordWrite(linkId);
    }
    return written;
  }

  async function markLinkSharedImpl(linkId: Uint8Array): Promise<boolean> {
    if (synced === null) return false;
    let written: boolean;
    try {
      written = synced.markLinkShared(mintHlc(), linkId);
    } catch {
      return false;
    }
    if (written) {
      refresh();
      await persistSyncedNow();
      // The send goes on in the background, so Copy does not wait on the
      // network (as iOS and Android); the unsent mark is set before the
      // first await, so a failed send is retried like any other.
      void afterLinkRecordWrite(linkId);
    }
    return written;
  }

  async function recordLinkUseImpl(linkId: Uint8Array, requesterKey: Uint8Array): Promise<boolean> {
    if (synced === null) return false;
    let written: boolean;
    try {
      written = synced.recordLinkUse(mintHlc(), linkId, requesterKey);
    } catch {
      return false;
    }
    if (written) {
      refresh();
      await persistSyncedNow();
      await afterLinkRecordWrite(linkId);
    }
    return written;
  }

  function linkAnswerVerdictAtImpl(
    linkId: Uint8Array,
    linkSecret: Uint8Array,
    requesterKey: Uint8Array,
    pressedAtMs: number,
  ): LinkAnswerVerdict | null {
    if (synced === null || !ready()) return null;
    // A stamp that cannot be read counts as written after any expiry
    // (fail-closed: the link does not count).
    const pressedAt =
      Number.isFinite(pressedAtMs) && pressedAtMs >= 0 ? BigInt(Math.floor(pressedAtMs)) : 18446744073709551615n;
    try {
      const v = synced.linkAnswerVerdict(linkId, linkSecret, requesterKey, pressedAt);
      return v === "accept" || v === "answered" || v === "unknown" ? v : "dead";
    } catch {
      return "dead";
    }
  }


  /** Send every register again when an earlier send was not confirmed. */
  async function flushUnsentLineage(): Promise<void> {
    const account = activeAccountIdB64;
    if (account === null || synced === null) return;
    try {
      if (localStorage.getItem(lineageUnsentKey(account)) === null) return;
      await sendLineageEnvelopes(synced.lineageEnvelopes());
      localStorage.removeItem(lineageUnsentKey(account));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("lineage register send failed; retried at the next start", err);
    }
  }

  async function setLineageImpl(
    successorGroupId: Uint8Array,
    rootConversationId: Uint8Array,
  ): Promise<boolean> {
    if (synced === null) return false;
    let written: boolean;
    try {
      written = synced.setLineage(mintHlc(), successorGroupId, rootConversationId);
    } catch (err) {
      // An id out of bounds, or a successor naming itself.
      // eslint-disable-next-line no-console
      console.warn("lineage register refused", err);
      return false;
    }
    if (!written) return false;
    refresh();
    const account = activeAccountIdB64;
    try {
      if (account !== null) localStorage.setItem(lineageUnsentKey(account), "1");
    } catch {
      /* storage unavailable: the send below is the only attempt */
    }
    try {
      const envelope = synced.lineageEnvelopeFor(successorGroupId);
      if (envelope !== undefined) await sendLineageEnvelopes([envelope]);
      if (account !== null) localStorage.removeItem(lineageUnsentKey(account));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("lineage register send failed; retried at the next start", err);
    }
    return true;
  }

  // ----- Inbound poll loop ---------------------------------------

  // ----- ADR-0074 section 3.3: the key-rotation port -----------------

  /** Poll passes started so far; a catch-up is settled only by a pass that
   *  started after it was asked for. */
  let passCounter = 0;
  const catchUps = new Set<{ readonly after: number; readonly settle: (r: JournalCatchUp) => void }>();
  function settleCatchUps(result: JournalCatchUp, passStartedAt?: number): void {
    for (const waiter of [...catchUps]) {
      if (result === "read" && passStartedAt !== undefined && passStartedAt <= waiter.after) continue;
      catchUps.delete(waiter);
      waiter.settle(result);
    }
  }
  function catchUp(): Promise<JournalCatchUp> {
    if (group === null || pollAbort === null || deviceRemoved()) return Promise.resolve("offline");
    return new Promise((resolve) => {
      const waiter = {
        after: passCounter,
        settle: (r: JournalCatchUp): void => {
          clearTimeout(timer);
          resolve(r);
        },
      };
      const timer = setTimeout(() => {
        catchUps.delete(waiter);
        resolve("offline");
      }, CATCH_UP_TIMEOUT_MS);
      catchUps.add(waiter);
    });
  }

  async function postKeyRotation(input: {
    readonly removedDeviceKey: Uint8Array;
    readonly preRemovalEpoch: bigint | null;
    readonly envelope: Uint8Array;
  }): Promise<{ readonly epoch: bigint }> {
    // The removal ran on another handle of this device; this one is reloaded
    // from disk by `announceDeviceGroupChangedHere`. Give that a moment: the
    // seam below refuses a handle that still lists the removed device.
    const deadline = Date.now() + HANDLE_RELOAD_WAIT_MS;
    while (
      Date.now() < deadline &&
      (reloading ||
        group === null ||
        (deviceKeysOf(group) ?? []).some((k) => bytesEqual(k, input.removedDeviceKey)))
    ) {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return sealKeyRotationAfterRemoval(input, {
      rosterKeys: () => {
        if (group === null || reloading) throw new Error("DeviceGroupSyncProvider is not ready");
        const keys = deviceKeysOf(group);
        if (keys === null) throw new Error("the DeviceGroup roster could not be read");
        return keys;
      },
      epoch: () => {
        if (group === null || identity === null) throw new Error("DeviceGroupSyncProvider is not ready");
        const epoch = deviceGroupEpoch(group, identity);
        if (epoch === null) throw new Error("the DeviceGroup epoch could not be read");
        return epoch;
      },
      seal: (envelope) => {
        if (group === null || identity === null || reloading || deviceRemoved()) {
          throw new Error("DeviceGroupSyncProvider is not ready");
        }
        // The DeviceGroup's own `ServiceEnvelope` carrier: this envelope is
        // never sealed into any other group.
        return group.createServiceEnvelopeMessage(identity, envelope, mintHlc());
      },
      publish: async (wire) => {
        await publishGroupMessage(wire);
      },
    });
  }

  onCleanup(
    installKeyRotationPort({
      epoch: () => (group === null || identity === null ? null : deviceGroupEpoch(group, identity)),
      post: postKeyRotation,
      catchUp,
    }),
  );

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
    while (!signal.aborted) {
      // ADR-0057 obligation 2 - every device of the account holds the
      // DeviceGroup's root: claim the journal on open, once per session (an
      // unclaimed journal never answers the 401 the lazy claim waits for).
      // Asked on every pass, which costs nothing once settled, so a claim
      // attempted before the session's signer factory was installed, or one
      // that met a network failure, is retried (`ds/owner-claim.ts`).
      void ds.ensureOwnerClaim(groupId);
      const passStartedAt = ++passCounter;
      let raw;
      try {
        raw = await ds.fetchGroup(groupId, {
          after: cursor,
          // A catch-up is waiting for the head: do not hold the read open.
          waitSecs: catchUps.size > 0 ? 0 : 25,
        });
      } catch (err) {
        if (signal.aborted) return;
        if (err instanceof DsHttpError && err.status === 404) {
          // Fresh journal slot — keep polling.
          continue;
        }
        if (err instanceof DsNetworkError) {
          // Transient — back off briefly + retry.
          settleCatchUps("offline");
          await sleepCancellable(2000, signal);
          continue;
        }
        if (err instanceof DsHttpError && (err.status === 401 || err.status === 403)) {
          // The journal does not answer this device any more.
          settleCatchUps("unreadable");
        } else {
          settleCatchUps("offline");
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
      // A service envelope whose publish failed (the own device card, the
      // setting, the own name) is sent again on every pass until it goes.
      void retryDirtyServiceEnvelopes();

      // ADR-0081 decision 2, step 6 - a DeviceGroup commit of this device
      // that did not finish (the DS could not be reached, a write failed, a
      // crash) is finished before anything else; until it is, this device
      // never reads itself as behind (the DS may hold its own next epoch).
      let commitPending = false;
      try {
        const owed = await resendOwedDeviceGroupCommitFor({
          accountIdBytes: accountIdBytesActive ?? new Uint8Array(0),
          accountIdB64,
          deviceKey,
        });
        if (owed !== null) {
          // eslint-disable-next-line no-console
          console.info(`DeviceGroup commit outbox: ${owed.kind}`);
        }
      } catch (err) {
        commitPending = true;
        // eslint-disable-next-line no-console
        console.warn("DeviceGroup commit outbox: not finished yet", err instanceof Error ? err.name : "");
      }
      // A landed entry made its epoch current and reloaded this handle.
      if (signal.aborted) return;
      const createdAtOf = new Map<number, number>(
        raw.messages.map((m) => [m.id, Date.parse(m.createdAt)] as [number, number]),
      );

      // G-13 (ADR-0015): the 32-byte verifying key peers feed to
      // `processIncomingVerifyingRemovals`. Cached (24 h). Without it the
      // batch is HELD, not applied under a zero key: a zero key refuses a
      // removal the policy requires to be attested, and a refused row is
      // skipped for good once the cursor moves past it. A device removed that
      // way would stay an epoch behind, drain the journal, read as behind and
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
      // Stopped while the key was being fetched: `g` and `id` may be freed,
      // and the fold may already be another session's.
      if (signal.aborted) return;

      // ADR-0058 section 5.1 — who was here before this batch.
      const membersBefore = membersOf(g);

      // ADR-0079 (order of 2026-10-17) — the journal readability rule for
      // this batch: own claims and notes are judged at their rows, commits
      // move the journal's epoch. Kept next to the claim record.
      const wasmJ = await loadWasm();
      if (signal.aborted) return;
      let readability: JournalReadability | null = null;
      try {
        readability = journalReadability({
          record: linkClaimRecord(accountIdB64),
          groupIdHex: bytesToHex(g.groupId()),
          epochOf: (m) => wasmJ.epochOfMessage(m),
          epochAfter: (m) => wasmJ.epochAfterMessage(m),
          groupEpoch: () => g.epoch(),
          newEpochs: (base) => new wasmJ.JournalEpochs(base),
          fromParts: (base, ids, afters) => wasmJ.JournalEpochs.fromParts(base, ids, afters),
          hex: bytesToHex,
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("journal readability not loaded", err instanceof Error ? err.name : "");
      }
      const judgeOwnRow = (rowId: number, mls: Uint8Array): boolean => {
        try {
          if (readability?.judgeOwn(rowId, mls) == null) return false;
          setLinkClaimsVersion((v) => v + 1);
          return true;
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("own link row not judged", err instanceof Error ? err.name : "");
          return false;
        }
      };
      const observeRow = (rowId: number, mls: Uint8Array): void => {
        try {
          readability?.observe(rowId, mls);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("journal epoch not observed", err instanceof Error ? err.name : "");
        }
      };

      const batch = await applyDeviceJournalBatch(raw.messages, cursor, {
        isOwnEcho: (rowId) => ownPublishedIds.has(rowId),
        apply: async (message) => {
          // Unwrap the DeviceGroup journal envelope (the commit / message plus
          // any removal attestation that travelled with it).
          const entry = decodeJournalEntry(message.payload);
          // ADR-0079 (order of 2026-10-17): this device's own claim or
          // link-founded note, recognised by the sealed bytes it recorded
          // before the publish (its own application message cannot be
          // decrypted here), and judged by the readability rule at this
          // row's position: it counts only when sealed at the journal's
          // epoch here. Recorded before the batch is persisted.
          if (judgeOwnRow(message.id, entry.message)) return;
          try {
            const attestation = entry.attestation ?? new Uint8Array(0);
            const keysBefore = membersOf(g);
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
            // ADR-0081 decision 4 - the sibling's record of when each key
            // was added, read at the row (the DS clock), so a knock older
            // than the addition is known to be answered.
            if (processed.kind !== "Application" && keysBefore !== null && admissions !== null) {
              const keysAfter = membersOf(g) ?? [];
              const at = createdAtOf.get(message.id);
              for (const m of keysAfter) {
                if (at !== undefined && Number.isFinite(at) && !keysBefore.some((b) => b.key === m.key)) {
                  admissions = noteAdded(admissions, m.key, at);
                }
              }
            }
            if (processed.kind === "Application") {
              const data = processed.applicationData;
              if (data === undefined) return;
              try {
                const wasm = await loadWasm();
                // The fold `applyParsed` writes is the provider's current one.
                // A loop stopped during the await above must not fold its row
                // into it: after a sign-in it is another account's.
                if (signal.aborted) return;
                const parsed = wasm.parseDeviceGroupPayload(data);
                try {
                  // ADR-0074 section 3.3: a `key-rotation` envelope means
                  // something here and nowhere else. Awaited, so a journal
                  // read "to the head" has stored it before it reports.
                  if (
                    parsed.kind === "serviceEnvelope" &&
                    (await auth.keyRotation.intake(parsed.serviceEnvelope))
                  ) {
                    return;
                  }
                  // ADR-0080: a device card is kept under the sender's leaf
                  // key, resolved from the MLS-authenticated sender leaf HERE,
                  // against the roster this row was sealed under (C-68).
                  if (
                    parsed.kind === "serviceEnvelope" &&
                    receiveDeviceCardEnvelope(wasm, parsed.serviceEnvelope, processed.senderLeaf, membersOf(g))
                  ) {
                    return;
                  }
                  // ADR-0079 (order of 2026-10-17): another device's
                  // link-founded note. Kept (the earliest per request) only
                  // from a sender leaf the roster resolves at receipt (C-68),
                  // before the batch is persisted.
                  if (parsed.kind === "serviceEnvelope" && wasm.isLinkFoundedEnvelope(parsed.serviceEnvelope)) {
                    let note;
                    try {
                      note = wasm.decodeLinkFoundedEnvelope(parsed.serviceEnvelope);
                    } catch {
                      return;
                    }
                    try {
                      if (senderKeyAtReceipt(processed.senderLeaf, membersOf(g)) === null) return;
                      let changed = false;
                      try {
                        changed = linkClaimRecord(accountIdB64).recordNote(
                          linkRequestKeyHex(note.linkId, note.requesterAccountKey),
                          {
                            id: message.id,
                            groupIdB64: base64Encode(note.groupId),
                            welcomeB64: base64Encode(note.welcome),
                          },
                          Date.now(),
                        );
                      } catch (err) {
                        // eslint-disable-next-line no-console
                        console.warn("link-founded note not recorded", err instanceof Error ? err.name : "");
                      }
                      if (changed) setLinkClaimsVersion((v) => v + 1);
                    } finally {
                      note.free();
                    }
                    return;
                  }
                  // ADR-0079 (reworked 2026-10-17): another device's claim of
                  // a link request. Honoured only from a sender leaf the
                  // roster resolves at receipt (C-68); its DS id is recorded
                  // with that leaf key before the batch is persisted (one
                  // synchronous write, so a decision in progress reads it
                  // whole).
                  if (parsed.kind === "serviceEnvelope" && wasm.isLinkClaimEnvelope(parsed.serviceEnvelope)) {
                    let claim;
                    try {
                      claim = wasm.decodeLinkClaimEnvelope(parsed.serviceEnvelope);
                    } catch {
                      return;
                    }
                    try {
                      const author = senderKeyAtReceipt(processed.senderLeaf, membersOf(g));
                      if (author === null) return;
                      let changed = false;
                      try {
                        changed = linkClaimRecord(accountIdB64).recordSeen(
                          linkRequestKeyHex(claim.linkId, claim.requesterAccountKey),
                          message.id,
                          author,
                          Date.now(),
                        );
                      } catch (err) {
                        // eslint-disable-next-line no-console
                        console.warn("link claim not recorded", err instanceof Error ? err.name : "");
                      }
                      if (changed) setLinkClaimsVersion((v) => v + 1);
                    } finally {
                      claim.free();
                    }
                    return;
                  }
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
          } finally {
            // A commit that took effect moves the journal's epoch.
            observeRow(message.id, entry.message);
          }
        },
        onOwnEcho: (message) => {
          // ADR-0079: a row this device published is judged and observed
          // at its position like any other.
          const entry = decodeJournalEntry(message.payload);
          judgeOwnRow(message.id, entry.message);
          observeRow(message.id, entry.message);
        },
        // The snapshot and the position it has consumed, in one write. A
        // failure holds the cursor (the rows come back next poll) rather than
        // recording a position the durable state has not reached.
        // A handle stopped while the batch was being applied (another handle
        // on this device changed the group) must not write its state: it is
        // an epoch behind what is on disk. Refusing holds the cursor.
        persist: async (to) => {
          if (signal.aborted || group !== g) {
            throw new Error("DeviceGroup handle replaced; not persisting");
          }
          // Conditional on the row this handle read (ADR-0081 decision 3).
          await writeHandleSnapshot(id, accountIdB64, deviceKey, to, () => group === g);
          await saveAdmissions(accountIdB64, deviceKey);
        },
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
          // Another handle on this device wrote a newer snapshot without
          // announcing it: this handle is behind what is on disk and would
          // hold the cursor forever. Reload from disk; the poll restarts
          // from the stored cursor.
          if (err instanceof DeviceGroupSnapshotChangedError && group === g) {
            void reloadHandle();
          }
        },
      });
      try {
        readability?.dispose();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("journal epochs not saved", err instanceof Error ? err.name : "");
      }
      // ADR-0057 obligation 6 — no heal on apply: the device that committed
      // registered the epoch with the account's root before publishing.

      // A row in this batch removed THIS device (docs/ux/second-device.md
      // section 3.1). The batch that carried the removal was persisted above,
      // so the next launch finds the inactive snapshot and shows the same
      // screen; nothing after it is polled or persisted.
      // Stopped while the batch was applied: nothing below (the removed
      // screen, the growth re-send, the rejoin) is this loop's to decide
      // any more, and `g` may be freed.
      if (signal.aborted) return;
      if (pollStepAfterBatch(readDeviceGroupActive(g)) === "stop-removed") {
        markRemoved();
        return;
      }

      // A device joined in this batch: the lowest prior leaf re-sends.
      const membersAfter = membersOf(g);
      if (!signal.aborted && group === g) publishRoster(false);
      if (membersBefore !== null && membersAfter !== null) {
        const growth = detectGrowth(membersBefore, membersAfter);
        if (growth.grew) {
          // ADR-0080: every device that was there re-sends its own card.
          scheduleOwnCardResend();
          try {
            void maybeScheduleSnapshot(g.ownLeafIndex(), growth.priorLeaves);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("growth re-send check failed", err);
          }
        }
      }

      // ADR-0074 section 3.3: a pass that started after a catch-up was asked
      // for and found nothing more behind it has read the journal to its head.
      // An empty answer at the head is a read to the head too: nothing is
      // persisted for it, and waiting for a persist kept the read open-ended
      // (every pass with wait 0, a tight loop) until the waiter timed out.
      if (!raw.hasMore && (batch.persisted || raw.messages.length === 0)) {
        settleCatchUps("read", passStartedAt);
      }
      if (batch.persisted) {
        cursor = batch.nextCursor;
        // Past the cursor on disk now, so these ids can never come back.
        for (const echoId of batch.skippedOwnEchoes) ownPublishedIds.delete(echoId);
      } else if (raw.messages.length > 0) {
        // Held: give storage a moment before the same rows are fetched again.
        await sleepCancellable(2000, signal);
        if (signal.aborted) return;
      }

      // ADR-0081 decision 4 - behind for good? A drained read (no rows,
      // nothing more) while the DS names a newer epoch than this handle's:
      // since an epoch is registered only with its commit row, nothing left
      // in the journal can bring this device forward. It drops its
      // DeviceGroup state and rejoins through a sibling.
      const drained = raw.messages.length === 0 && !raw.hasMore;
      const latestEpoch = raw.latestEpoch === undefined ? undefined : BigInt(raw.latestEpoch);
      const localEpoch = drained && latestEpoch !== undefined ? deviceGroupEpoch(g, id) : null;
      if (
        localEpoch !== null &&
        isBehind(localEpoch, drained, latestEpoch, commitPending) &&
        rejoinAllowed(readDeviceGroupActive(g))
      ) {
        // eslint-disable-next-line no-console
        console.warn(
          `DeviceGroup behind the DS with nothing left to read: local epoch ${localEpoch}, DS epoch ${latestEpoch}; rejoining`,
        );
        void enterRejoin(accountIdB64);
        return;
      }
      // At the head: a sibling reads the knocks (ADR-0081 decision 4).
      if (drained && !commitPending && Date.now() - lastKnockCheckAt >= KNOCK_CHECK_INTERVAL_MS) {
        lastKnockCheckAt = Date.now();
        await checkKnocks(g, id, accountIdB64, deviceKey, signal).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("DeviceGroup knock check failed", err instanceof Error ? err.name : "");
        });
        if (signal.aborted) return;
      }
    }
  }

  /** When the knocks were last read. */
  let lastKnockCheckAt = 0;
  /** DS id of the last knock row read this session. */
  let knockCursor = 0;

  /** The directory check of the founding policy (ADR-0081 decision 4):
   *  `true` when the account's directory lists no own device other than
   *  this one. A failure throws (nothing is founded on an unread directory). */
  async function directoryListsNoOtherDevice(accountIdBytes: Uint8Array, deviceSeed: Uint8Array): Promise<boolean> {
    const wasm = await loadWasm();
    const device = wasm.Identity.fromSeed(deviceSeed);
    let ownKeyHex: string;
    try {
      ownKeyHex = bytesToHex(device.signaturePublicKey);
    } finally {
      device.free();
    }
    const listed = await new DiscoveryClient().fetchKeyPackages(accountIdBytes);
    return (listed ?? []).every((row) => bytesToHex(row.deviceId) === ownKeyHex);
  }

  async function loadAdmissions(account: string, deviceKey: CryptoKey): Promise<void> {
    let record: AdmissionRecord | null = null;
    try {
      const bytes = await getDeviceGroupPending(account, "admissions", deviceKey);
      record = bytes === null ? null : decodeAdmissionRecord(bytes);
    } catch {
      record = null;
    }
    admissions = record ?? newAdmissionRecord(Date.now());
  }

  async function saveAdmissions(account: string, deviceKey: CryptoKey): Promise<void> {
    if (admissions === null || activeAccountIdB64 !== account) return;
    try {
      await putDeviceGroupPending(account, "admissions", encodeAdmissionRecord(admissions), deviceKey);
    } catch (err) {
      // The record is a guard against admitting twice; an unsaved update is
      // rebuilt from the journal by the next batch.
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup admission record not saved", err instanceof Error ? err.name : "");
    }
  }

  /**
   * A sibling's side of ADR-0081 decision 4: read the DeviceGroup's knocks
   * and admit each one {@link admissionVerdict} allows - replace the device's
   * previous leaf, if it is still in the roster, by its KeyPackage in one
   * commit carrying its self-removal proof (ADR-0081 amendment), else add
   * the KeyPackage; one commit through the outbox, the Welcome to the knock's
   * rendezvous.
   */
  async function checkKnocks(
    g: WasmDeviceGroup,
    id: WasmIdentity,
    accountIdB64: string,
    deviceKey: CryptoKey,
    signal: AbortSignal,
  ): Promise<void> {
    const accountBytes = accountIdBytesActive;
    const session = auth.state();
    const username = session.kind === "session" ? session.account.username : "";
    if (accountBytes === null || admissions === null || username.trim().length === 0) return;
    const ds = new DsClient();
    const knocks = await ds.fetchKnocks(g.groupId(), { after: knockCursor });
    if (signal.aborted || knocks.messages.length === 0) return;
    knockCursor = knocks.messages[knocks.messages.length - 1]?.id ?? knockCursor;
    const wasm = await loadWasm();
    const loginHandle = await ownPairingHandle(username);
    const roster = (membersOf(g) ?? []).map((m) => ({ leaf: m.leaf, keyHex: m.key }));
    const ownKeyHex = bytesToHex(id.signaturePublicKey);
    let policy = false;
    try {
      policy = g.requireAttestedRemovals();
    } catch {
      policy = false;
    }
    let directory: Map<string, Uint8Array> | null = null;
    for (const knock of knocks.messages) {
      if (signal.aborted) return;
      let decoded;
      try {
        decoded = await parsePairingQrPayload(knock.payload, loginHandle);
      } catch {
        continue; // not a payload for this account (or not a pairing payload)
      }
      const keyBytes = wasm.keyPackageSignatureKey(decoded.keyPackage);
      const keyHex = bytesToHex(keyBytes);
      if (directory === null) directory = await ownDirectory(accountBytes, accountIdB64, deviceKey);
      const listedKp = directory.get(keyHex);
      const knockAtMs = Date.parse(knock.createdAt);
      // Checked BEFORE a commit is built: a commit the DS takes but every
      // peer refuses would leave this device ahead of them.
      const proof = decoded.selfRemovalProof;
      const proofCovers =
        proof !== null &&
        wasm.deviceGroupSelfRemovalCovers(id, proof, g.groupId(), g.epoch(), keyBytes, decoded.keyPackage);
      const verdict = admissionVerdict({
        knockId: knock.id,
        knockAtMs: Number.isFinite(knockAtMs) ? knockAtMs : 0,
        keyHex,
        ownKeyHex,
        roster,
        handled: new Set(admissions.handled),
        watchingSinceMs: admissions.watchingSinceMs,
        lastAddedAtMs: admissions.lastAddedAtMs[keyHex],
        listed: listedKp !== undefined,
        policyRequiresAttestation: policy,
        selfRemovalProofCovers: proofCovers,
        keyPackageAlreadyIn: g.containsLeafOfKeyPackage(id, decoded.keyPackage),
        nowMs: Date.now(),
      });
      if (verdict !== "admit") continue;
      const account = { accountIdBytes: accountBytes, accountIdB64, deviceKey };
      const added = await commitDeviceGroupChange(
        account,
        (fresh) => {
          // Another sibling answered it meanwhile.
          if (fresh.group.containsLeafOfKeyPackage(fresh.identity, decoded.keyPackage)) return null;
          const before = membersOfEnsured(fresh) ?? [];
          const previous = before.find((m) => m.key === keyHex);
          const priorLeaves = before.map((m) => m.leaf).sort((a, b) => a - b);
          if (previous === undefined) {
            const addition = fresh.group.addDeviceFromWire(fresh.identity, decoded.keyPackage);
            return {
              kind: "rejoin-add",
              row: encodeJournalEntry(addition.commit, null),
              welcome: { bytes: addition.welcome, rendezvous: decoded.rendezvousPseudonym },
              priorLeaves,
            };
          }
          // ADR-0081 amendment — the previous leaf out and the new one in, in
          // ONE commit; the proof rides in the removal-authorisation slot.
          // Re-checked at the epoch this commit leaves: a proof names one
          // epoch, and peers refuse it at any other. Under the policy a
          // removal it does not cover would be refused by every peer.
          const covers =
            proof !== null &&
            wasm.deviceGroupSelfRemovalCovers(
              fresh.identity,
              proof,
              fresh.group.groupId(),
              fresh.group.epoch(),
              keyBytes,
              decoded.keyPackage,
            );
          if (!covers && fresh.group.requireAttestedRemovals()) return null;
          const replaced = fresh.group.replaceDeviceAt(fresh.identity, previous.leaf, decoded.keyPackage);
          return {
            kind: "rejoin-replace",
            row: encodeJournalEntry(replaced.commit, covers ? proof : null),
            welcome: { bytes: replaced.welcome, rendezvous: decoded.rendezvousPseudonym },
            priorLeaves,
          };
        },
        async (_fresh, outcome) => outcome,
      );
      if (admissions !== null && added !== null && added.outcome.kind === "landed") {
        admissions = noteHandled(noteAdded(admissions, keyHex, Date.now()), knock.id);
        await saveAdmissions(accountIdB64, deviceKey);
        // eslint-disable-next-line no-console
        console.info("DeviceGroup: a device of this account was admitted again");
      }
      // The commits reloaded the long-lived handle; the next pass re-reads.
      return;
    }
  }

  function membersOfEnsured(e: EnsuredDeviceGroup): LeafMember[] | null {
    return membersOf(e.group);
  }

  /** The account's directory rows whose device binding verifies under the
   *  account key the conversation leaves are bound to: leaf key hex to its
   *  directory KeyPackage. */
  async function ownDirectory(
    accountIdBytes: Uint8Array,
    accountIdB64: string,
    deviceKey: CryptoKey,
  ): Promise<Map<string, Uint8Array>> {
    const wasm = await loadWasm();
    const out = new Map<string, Uint8Array>();
    const lineageSeed = await deviceGroupLineageSeed(accountIdB64, deviceKey, auth.identitySecrets()?.accountIdentitySeed);
    if (lineageSeed === null) return out;
    const lineage = wasm.Identity.fromSeed(lineageSeed);
    let accountKeyHex: string;
    try {
      accountKeyHex = bytesToHex(lineage.signaturePublicKey);
    } finally {
      lineage.free();
    }
    const listed = await new DiscoveryClient().fetchKeyPackages(accountIdBytes);
    for (const row of listed ?? []) {
      let bound: Uint8Array | undefined;
      try {
        bound = wasm.keyPackageAccountKey(row.keyPackage) ?? undefined;
      } catch {
        bound = undefined;
      }
      const leafHex = bytesToHex(row.deviceId);
      // A Phase 1 unbound leaf resolves to itself: not a binding.
      if (bound === undefined || bytesToHex(bound) !== accountKeyHex || bytesToHex(bound) === leafHex) continue;
      out.set(leafHex, row.keyPackage);
    }
    return out;
  }

  /** Stop following the DeviceGroup, drop its local state, and rejoin. */
  async function enterRejoin(account: string): Promise<void> {
    const accountBytes = accountIdBytesActive;
    if (activeAccountIdB64 !== account || accountBytes === null || group === null) return;
    if (deviceRemoved()) return;
    const current = binding.capture();
    reloading = true;
    try {
      pollAbort?.abort();
      pollAbort = null;
      if (pollDone !== null) await pollDone;
      if (!current()) return;
      pollDone = null;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null || !current()) return;
      try {
        group?.free();
        identity?.free();
      } catch {
        // best-effort
      }
      group = null;
      identity = null;
      handleGeneration = null;
      ownPublishedIds.clear();
      setReady(false);
      publishOwnDeviceRoster(null);
      startRejoin(account, accountBytes, deviceKey, current, true);
    } finally {
      if (current()) reloading = false;
    }
  }

  /**
   * The device's side of ADR-0081 decision 4 (`device-group-rejoin.ts`):
   * drop the local state, make sure the directory KeyPackage is out, knock,
   * wait for the Welcome, join, then load the joined DeviceGroup like a
   * bootstrap would. Runs until joined or the session ends.
   */
  function startRejoin(
    account: string,
    accountBytes: Uint8Array,
    deviceKey: CryptoKey,
    current: () => boolean,
    dropState = true,
  ): void {
    if (rejoinAbort !== null) return;
    const abort = new AbortController();
    rejoinAbort = abort;
    accountIdBytesActive = accountBytes;
    const ports = rejoinPorts(account, accountBytes, deviceKey, abort.signal);
    void (async () => {
      let first = dropState;
      while (!abort.signal.aborted && current()) {
        try {
          const outcome = await rejoinOnce(ports, first);
          first = false;
          if (outcome === "joined") break;
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("DeviceGroup rejoin attempt failed; trying again", err instanceof Error ? err.name : "");
          await sleepCancellable(BOOTSTRAP_RETRY_MS, abort.signal);
        }
      }
      if (rejoinAbort === abort) rejoinAbort = null;
      if (abort.signal.aborted || !current()) return;
      // Joined: load it like any session start.
      activeAccountIdB64 = null;
      void bootstrap(account, accountBytes);
    })();
  }

  function rejoinPorts(
    account: string,
    accountBytes: Uint8Array,
    deviceKey: CryptoKey,
    signal: AbortSignal,
  ): RejoinPorts {
    return {
      dropLocalState: () => dropDeviceGroupStateForRejoin(account),
      publishDirectoryKeyPackage: () => auth.republishKeyPackage(),
      async loadState() {
        const bytes = await getDeviceGroupPending(account, "rejoin", deviceKey);
        return bytes === null ? null : decodeRejoinState(bytes);
      },
      saveState: (state) => putDeviceGroupPending(account, "rejoin", encodeRejoinState(state), deviceKey),
      async knock(nowMs) {
        const wasm = await loadWasm();
        const s = auth.state();
        const username = s.kind === "session" ? s.account.username : "";
        const loginHandle = await ownPairingHandle(username);
        const groupId = await deviceGroupIdOf(account, deviceKey, auth.identitySecrets()?.accountIdentitySeed);
        if (groupId === null) throw new Error("the DeviceGroup id cannot be derived yet");
        const seed = await ensureDeviceIdentitySeed(account, deviceKey);
        const joiner = wasm.Identity.fromSeed(seed);
        seed.fill(0);
        try {
          const keyPackage = joiner.createKeyPackage();
          const rendezvous = generateRendezvousPseudonym();
          // ADR-0081 amendment — the device identity's key IS this device's
          // old DeviceGroup leaf key: it signs that its leaf may be replaced
          // by this KeyPackage in the commit leaving the journal's latest
          // epoch (an empty knock is a probe that names it), which lets a
          // sibling readmit it under the attested-removal policy without a
          // passkey. An unclaimed journal names no epoch: no proof.
          const ds = new DsClient();
          const probe = await ds.knock(groupId, []);
          const payload =
            probe.epoch === null
              ? await buildPairingQrPayload(loginHandle, keyPackage, rendezvous)
              : await buildRejoinKnockPayload(
                  loginHandle,
                  keyPackage,
                  rendezvous,
                  wasm.signDeviceGroupSelfRemoval(joiner, groupId, BigInt(probe.epoch), keyPackage),
                );
          const state = {
            v: 1 as const,
            rendezvousB64: base64Encode(rendezvous),
            joinerSnapshotB64: base64Encode(joiner.snapshotStorage()),
            knockedAtMs: nowMs,
          };
          // Kept BEFORE the knock: the joiner's provider is the only thing
          // that can open the Welcome a sibling sends.
          await putDeviceGroupPending(account, "rejoin", encodeRejoinState(state), deviceKey);
          await ds.knock(groupId, [payload]);
          return state;
        } finally {
          joiner.free();
        }
      },
      async awaitWelcome(state, deadlineMs) {
        const remaining = Math.max(0, Math.min(deadlineMs - Date.now(), REKNOCK_AFTER_MS));
        const outcome = await pollForWelcome(base64Decode(state.rendezvousB64), {
          signal,
          timeoutMs: remaining,
        });
        return outcome.kind === "received" ? outcome.welcome : null;
      },
      async join(state, welcome) {
        const wasm = await loadWasm();
        const seed = await ensureDeviceIdentitySeed(account, deviceKey);
        const joiner = wasm.Identity.fromSeed(seed);
        seed.fill(0);
        try {
          joiner.restoreStorage(base64Decode(state.joinerSnapshotB64));
          const joined = wasm.DeviceGroup.joinFromWelcome(welcome, joiner, accountBytes);
          try {
            await persistDeviceGroupSnapshotWithCursor(joiner, account, deviceKey, undefined);
          } finally {
            joined.free();
          }
        } finally {
          joiner.free();
        }
        // A fresh observer of the journal.
        await putDeviceGroupPending(account, "admissions", encodeAdmissionRecord(newAdmissionRecord(Date.now())), deviceKey);
      },
      clearState: () => deleteDeviceGroupPending(account, "rejoin"),
      now: () => Date.now(),
    };
  }

  function applyParsed(parsed: ParsedDeviceGroupPayload): void {
    if (parsed.kind === "serviceEnvelope") {
      // ADR-0060 section 7 - a Visibility, ReachState or (ADR-0048 section 4a)
      // own Profile envelope from another device of this account, ordered by
      // its own stamp (not the HLC) and not a synced register; anything else
      // inside is dropped by the intake.
      // ADR-0073 R.5 / R.13a - a lineage envelope is routed BEFORE that
      // dispatcher: it is a `lakko-sync` register, folded by the one fold.
      // ADR-0075 - the recovery-email register, the same way.
      if (wasmSync !== null && wasmSync.isRecoveryEmailEnvelope(parsed.serviceEnvelope)) {
        try {
          if (synced?.applyRecoveryEmailEnvelope(parsed.serviceEnvelope) === true) {
            observeHlc(synced.maxHlc());
            refresh();
          }
        } catch {
          // A malformed register is dropped, never folded.
        }
        return;
      }
      // ADR-0079 - a link-record register, the same way.
      if (wasmSync !== null && wasmSync.isLinkRecordEnvelope(parsed.serviceEnvelope)) {
        try {
          if (synced?.applyLinkRecordEnvelope(parsed.serviceEnvelope) === true) {
            observeHlc(synced.maxHlc());
            refresh();
          }
        } catch {
          // A malformed register is dropped, never folded.
        }
        return;
      }
      // ADR-0076 - the own-username register, the same way.
      if (wasmSync !== null && wasmSync.isOwnUsernameEnvelope(parsed.serviceEnvelope)) {
        try {
          if (synced?.applyOwnUsernameEnvelope(parsed.serviceEnvelope) === true) {
            observeHlc(synced.maxHlc());
            refresh();
          }
        } catch {
          // A malformed register is dropped, never folded.
        }
        return;
      }
      if (wasmSync !== null && wasmSync.isLineageEnvelope(parsed.serviceEnvelope)) {
        try {
          if (synced?.applyLineageEnvelope(parsed.serviceEnvelope) === true) {
            observeHlc(synced.maxHlc());
            refresh();
          }
        } catch (err) {
          // A malformed register is dropped, never folded.
          // eslint-disable-next-line no-console
          console.warn("lineage envelope refused", err);
        }
        return;
      }
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

  async function publishGroupMessage(wireBytes: Uint8Array): Promise<number> {
    if (group === null || identity === null || activeAccountIdB64 === null || reloading) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    if (deviceRemoved()) {
      throw new Error("this device was removed from the account");
    }
    // The account and the identity the message was sealed with. Both awaits
    // below can span a sign-out, and what is persisted afterwards is this
    // identity's snapshot under this account's row - never whatever the
    // provider holds by then.
    const account = activeAccountIdB64;
    const id = identity;
    const current = binding.capture();
    const ds = new DsClient();
    // G-13 (ADR-0015): wrap in the journal envelope (application messages
    // carry no attestation).
    const published = await ds.publishGroup(
      group.groupId(),
      encodeJournalEntry(wireBytes, null),
    );

    // The session ended while the publish was out: its handles are freed and
    // its own-echo set was cleared. The row is on the journal; the snapshot
    // that sealed it went with the sign-out.
    if (!current()) throw new StaleBindingError();
    // The DS hands this row straight back to the receive loop; remember its id
    // so the loop skips it rather than failing to decrypt its own message.
    ownPublishedIds.add(published.id);
    const deviceKey = await getDeviceKey(account);
    if (!current()) throw new StaleBindingError();
    // A handle reload in between replaced `identity`; the snapshot on disk is
    // already the newer one.
    if (deviceKey !== null && identity === id) {
      // Fail-closed and conditional on the row this handle read (ADR-0081
      // decision 3).
      await writeHandleSnapshot(id, account, deviceKey, undefined, () => identity === id);
      if (!current()) throw new StaleBindingError();
    }
    return published.id;
  }

  /** ADR-0079 (reworked 2026-10-17): seal and publish this device's claim
   *  of a link request on the DeviceGroup's own `ServiceEnvelope` carrier. */
  async function postLinkClaimImpl(
    linkId: Uint8Array,
    requesterKey: Uint8Array,
    beforePublish: (wireHex: string) => void,
  ): Promise<number> {
    if (group === null || identity === null || reloading || deviceRemoved()) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const wasm = await loadWasm();
    const envelope = wasm.composeLinkClaimEnvelope(linkId, requesterKey);
    if (group === null || identity === null || reloading || deviceRemoved()) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const wire = group.createServiceEnvelopeMessage(identity, envelope, mintHlc());
    beforePublish(bytesToHex(wire));
    return publishGroupMessage(wire);
  }

  async function postLinkFoundedImpl(
    linkId: Uint8Array,
    requesterKey: Uint8Array,
    groupId: Uint8Array,
    welcome: Uint8Array,
    beforePublish: (wireHex: string) => void,
  ): Promise<number> {
    if (group === null || identity === null || reloading || deviceRemoved()) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const wasm = await loadWasm();
    const envelope = wasm.composeLinkFoundedEnvelope(linkId, requesterKey, groupId, welcome);
    if (group === null || identity === null || reloading || deviceRemoved()) {
      throw new Error("DeviceGroupSyncProvider is not ready");
    }
    const wire = group.createServiceEnvelopeMessage(identity, envelope, mintHlc());
    // Recorded before the publish: the receive loop judges the row at its
    // position by these bytes.
    beforePublish(bytesToHex(wire));
    return publishGroupMessage(wire);
  }

  function linkClaimRosterImpl(): { ownKey: Uint8Array; roster: Uint8Array[] } | null {
    const g = group;
    if (g === null || identity === null || reloading || deviceRemoved()) return null;
    try {
      const own = g.ownLeafIndex();
      const devices = g.devices() as { leafIndex: number; signaturePublicKey: Uint8Array }[];
      const ownKey = devices.find((d) => d.leafIndex === own)?.signaturePublicKey;
      if (ownKey === undefined) return null;
      return { ownKey, roster: devices.map((d) => d.signaturePublicKey) };
    } catch {
      return null;
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
    const { g, id, s } = requireReady();
    const hlc = mintHlc();
    const reply = replyToId ?? new Uint8Array(0);
    const bytes = g.createDeviceGroupDraftMessage(id, groupId, text, reply, hlc);
    await publishGroupMessage(bytes);
    // A device never receives its own DeviceGroup messages: fold the write
    // in here, at the same stamp. Into the fold the write was made against:
    // `synced !== s` means the session changed while the publish was out.
    if (synced === s && s.applyDraft(hlc, groupId, text, reply)) refresh();
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
    const { g, id, s } = requireReady();
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
      synced === s &&
      s.applyFlags(hlc, groupId, next.pinned, next.muted, next.archived, next.unreadOverride)
    ) {
      refresh();
    }
  }

  async function setReadCursorImpl(
    groupId: Uint8Array,
    upToMessageId: Uint8Array,
    readAtMs: bigint,
  ): Promise<void> {
    const { g, id, s } = requireReady();
    const hlc = mintHlc();
    const bytes = g.createDeviceGroupReadCursorMessage(id, groupId, upToMessageId, readAtMs, hlc);
    await publishGroupMessage(bytes);
    if (synced === s && s.applyReadCursor(hlc, groupId, upToMessageId, readAtMs)) refresh();
  }

  async function setOrderImpl(
    groupIds: readonly Uint8Array[],
  ): Promise<void> {
    const { g, id, s } = requireReady();
    const hlc = mintHlc();
    // Defensive copy — mutating the caller's arrays later must not affect
    // the wire bytes.
    const ids = groupIds.map((x) => new Uint8Array(x));
    const bytes = g.createDeviceGroupOrderMessage(id, ids, hlc);
    await publishGroupMessage(bytes);
    if (synced === s && s.applyConversationOrder(hlc, ids.map((x) => new Uint8Array(x)))) {
      refresh();
    }
  }

  // ----- Folders (ADR-0058) ----------------------------------------

  async function applyFolderEditsImpl(edits: readonly RegisterEdit[]): Promise<void> {
    if (edits.length === 0) return;
    const { s } = requireReady();
    const wasm = await loadWasm();
    // Signed out during the await: `s` is freed, and the outbox is no longer
    // this account's.
    if (synced !== s) throw new Error("DeviceGroupSyncProvider is not ready");
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

  // ----- The end of a membership (ADR-0087 section 9) ----------------

  async function writeConversationEndImpl(
    ref: ConversationRef,
    state: "ended" | "joined",
    epoch: bigint,
  ): Promise<boolean> {
    const { s } = requireReady();
    const wasm = await loadWasm();
    if (synced !== s) throw new Error("DeviceGroupSyncProvider is not ready");
    const hlc = mintHlc();
    const register =
      state === "ended"
        ? s.composeConversationEnded(hlc, ref.kind, ref.id, epoch)
        : s.composeConversationJoined(hlc, ref.kind, ref.id, epoch);
    if (register === undefined) return false;
    const cs = new wasm.FolderChangeSet();
    try {
      cs.addConversationEnd(register);
      s.applyFolderChanges(cs);
    } finally {
      cs.free();
      register.free();
    }
    setOutbox((q) => [...q, stampEdits([{ t: "conversation-end", ref }], mintHlc)]);
    refresh();
    // On disk before the caller goes on: what comes next deletes what this
    // register is the only remaining record of.
    if (!(await persistSyncedNow())) throw new Error("synced state not written");
    void drainFolderOutbox();
    return true;
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
    // The queue drained is the one of the session this call starts in. After
    // a sign-out its result is nobody's: writing it back would put this
    // account's unsent edits into an emptied (or another account's) outbox,
    // and re-arming the retry would keep a timer alive while signed out.
    const current = binding.capture();
    draining = true;
    try {
      const wasm = await loadWasm();
      if (!current()) return;
      const queue = outbox();
      const result = await drainOutbox(
        queue,
        async (batch) => {
          // Not `StaleBindingError`: a failed batch only stops the drain, and
          // the stale result is dropped below.
          if (!current() || group === null || identity === null) throw new Error("not ready");
          const cs = buildFolderChangeSet(wasm, batch, (ref) =>
            synced?.conversationEnd(ref.kind, ref.id),
          );
          try {
            // A conversation-end edit whose register is no longer held sends
            // nothing, and takes its place in the queue with it.
            if (cs.registerCount === 0) return;
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
      if (!current()) return;
      // Batches added while this ran are kept after what is still waiting.
      setOutbox((now) => [...result.remaining, ...now.slice(queue.length)]);
      if (result.failed) scheduleOutboxRetry();
    } catch (err) {
      if (!current()) return;
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
    saved,
    savedBlobIdsHex,
    endedConversations,
    writeConversationEnd: writeConversationEndImpl,
    lineage,
    setLineage: setLineageImpl,
    linkRecords,
    setLinkRecord: setLinkRecordImpl,
    revokeLinkRecord: revokeLinkRecordImpl,
    markLinkShared: markLinkSharedImpl,
    recordLinkUse: recordLinkUseImpl,
    linkAnswerVerdictAt: linkAnswerVerdictAtImpl,
    postLinkClaim: postLinkClaimImpl,
    linkClaimRoster: linkClaimRosterImpl,
    postLinkFounded: postLinkFoundedImpl,
    catchUpJournal: catchUp,
    linkClaimsVersion,
    deviceRemoved,
    deviceRoster,
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

function freeHandles(g: WasmDeviceGroup, id: WasmIdentity): void {
  try {
    g.free();
    id.free();
  } catch {
    // best-effort
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
