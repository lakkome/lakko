// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the end of a membership, for the session.
 *
 * The rules are `membership-end.ts`, the order is `membership-end-runtime.ts`,
 * the list of stores is `membership-wipe.ts`. This provider binds them to the
 * app and is the ONE seam:
 *
 *   - **After a commit.** Every commit, from any path (the open chat's poll,
 *     the background receive, a sweep), is applied under the group's MLS lock,
 *     so the lock draining is the one place all of them pass through
 *     (`onGroupMlsIdle`). A new path that applies a commit is covered without
 *     being told.
 *   - **At launch.** Every stored group is asked once the synced state is up,
 *     which catches a commit applied and persisted just before a crash.
 *   - **On a register.** `endedConversations()` is read after every fold and at
 *     launch. It is idempotent: a wiped group has nothing left to act on. A
 *     group held active above the register's epoch is one this device came
 *     back to: nothing stops, and `joined` is sent. This check comes before
 *     anything stops, because the core's delete only refuses at the last step.
 *
 * Nothing here runs from a refused Delivery Service read, and none of it runs
 * in the service worker (C-59).
 *
 * The context defaults to an inert value rather than throwing: a view tested
 * or mounted outside this provider then behaves as if no membership ended.
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

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { useCallLog } from "../calls/CallLogContext";
import { useTranslation } from "../translation/context";
import { forgetUploadedCard } from "../channels/member-cards";
import { leaveChannelOnServer } from "../channels/unsubscribe";
import { leaveChannelSubscriptionHere } from "../channels/channel-subscription-register";
import { installChannelDeletedHandler } from "../channels/api";
import { loadKnownGroupIds, saveKnownGroupIds } from "../devices/state-bundle";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex } from "../devices/sync/state";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { withoutConversation } from "../feed/feed-marks";
import { updateFeedMarks } from "../feed/feed-marks-store";
import { onGroupMlsIdle, withGroupMlsLock } from "../mls/group-lock";
import { useProfiles } from "../profile/ProfileContext";
import { memberNameOf } from "../profile/member-label";
import { retireOldPushGid } from "../push/group-routing";
import { dropPendingPublisherRequest } from "../channels/pending-publisher-requests";
import { getGroupRoutingSecretForAccount } from "../push/group-routing-store";
import { channelsActions, channelsState } from "../state/channels-state";
import { useChats, type ActiveGroup } from "./ChatContext";
import { conversationKindOf } from "./conversation-kind";
import { deleteConversation as deleteConversationStores } from "./delete-chat";
import { bytesToHex, hexToBytes, ruleIdOfRef, type ConversationRef, type RegisterEdit } from "./folders/folder-model";
import { useGroupClosed } from "./GroupClosedContext";
import { groupTitle } from "./group-title";
import { useGroupNames } from "./GroupNameContext";
import {
  draftToRestore,
  endKey,
  endTriggered,
  heardRegisterVerdict,
  MembershipEndStore,
  stillActiveGroupIdOf,
  type EndNotice,
  type EndRecord,
  type HeldWelcome,
} from "./membership-end";
import { IdbMembershipEndBackend } from "./membership-end-idb";
import { MembershipEndRuntime, type EndPort, type NewEnd } from "./membership-end-runtime";
import { loadLeaveRequestedAt, readIsActive } from "./membership-ended";
import { groupWipeSteps, runWipeSteps, type GroupWipeContexts } from "./membership-wipe";
import { allowGroupInMessageLog, blockGroupInMessageLog } from "./message-log-store";
import { ownPersonKeyB64, personKeyOf, type RosterLeaf } from "./person-key";
import { usePins } from "./PinContext";
import { useReadWatermarks } from "./ReadWatermarkContext";
import { forgetRequestsSent } from "./requests-sent";
import { chunkSaveEdits, unsaveEdit } from "./saved-messages";
import { useLineage } from "./use-lineage";

/** How often owed records are taken up again while any exist (chosen): a
 *  failed step is a moment without the network or a store refusing. */
const RESUME_INTERVAL_MS = 30_000;

export interface MembershipEndApi {
  /** Whether this group's end is under way here. Reactive. */
  isEnding(groupIdB64: string): boolean;
  /** The same for a channel, by its hex id. Reactive. */
  isChannelEnding(channelIdHex: string): boolean;
  /** The notices of ends noticed this session and not yet shown, oldest
   *  first. In memory only (UX section 9): nothing is shown at the next
   *  start. Reactive. */
  readonly notices: Accessor<readonly EndNotice[]>;
  /** The notice of the conversation a chat route names. Reactive. */
  noticeForRoute(idB64: string): EndNotice | undefined;
  /** Whether the end of the conversation a chat route names was asked for
   *  here: no notice, and the view goes back to the list. Reactive. */
  leftHere(idB64: string): boolean;
  /** Close a notice; its name goes with it. */
  dismissNotice(key: string): void;
  /** The dialog over the open conversation is showing this notice: the
   *  short notice at the bottom leaves it out. */
  claimNotice(key: string): void;
  /** Whether a dialog claimed this notice. */
  isNoticeClaimed(key: string): boolean;
  /** Bumped when a leave is requested here, so a row can say so. */
  readonly leaveVersion: Accessor<number>;
  noteLeaveRequested(): void;
  /** An open chat view reports what its composer holds. */
  registerOpenDraft(groupIdB64: string, read: () => string): () => void;
  /** The call host ends a call in a group that ended. */
  registerCallStop(stop: (groupIdB64: string) => void): () => void;
  /** The background receive reads a journal to its head: `true` when it got
   *  there. */
  registerJournalReader(read: (groupIdB64: string) => Promise<boolean>): () => void;
  /** Called after a group left this device, for what is held in memory. */
  onGroupWiped(listener: (groupIdB64: string) => void): () => void;
  /** Bumped when an end finished: a held Welcome may open now. */
  readonly settledVersion: Accessor<number>;

  /** Unsubscribe: the same end, for a channel. Resolves when it ran.
   *  `removed`: a publisher removed this device's key from a private
   *  channel (UX 9.5), so the person is told once; an unsubscribe is not. */
  endChannel(channelIdHex: string, title: string, removed?: boolean): Promise<void>;
  /** A Welcome put this group in the registry (first join or not). */
  noteJoined(entry: ActiveGroup): Promise<void>;
  /** This device subscribed to a channel. */
  noteChannelJoined(channelIdHex: string): Promise<void>;
  /** 2026-11-13: the account's channel-subscription register says another
   *  device of the account left the channel. The channel goes here as an
   *  unsubscribe would, without telling the channels service (the device
   *  that left did). A channel this device publishes in is left alone. */
  channelLeftElsewhere(channelIdHex: string): void;
  /** Run whatever ends are owed, as far as they go, those a commit just made
   *  owed included. */
  settle(): Promise<void>;
  /** Open a Welcome. It never lands inside a wipe: the wipe drops the registry
   *  entry last, and a join landing before that would be dropped with it. */
  joinAlone<T>(join: () => Promise<T>): Promise<T>;
  /** A join put a group back over one this device was out of, before any end
   *  began. Nothing returns, so what the old one left goes now. */
  startOver(prior: ActiveGroup | undefined): Promise<void>;
  /** Read a group's journal to its head. `false` when that did not happen. */
  readJournal(groupIdB64: string): Promise<boolean>;
  readonly heldWelcomes: Accessor<readonly HeldWelcome[]>;
  holdWelcome(welcome: HeldWelcome): Promise<void>;
  dropHeldWelcome(messageId: number): Promise<void>;
  /** The group a refused Welcome names, standard base64, or `null`. */
  stillActiveGroupOf(err: unknown): string | null;
  /** `Delete this chat`: a device that holds an ended conversation with its
   *  account still in it, or an ended one-to-one. */
  deleteConversation(input: {
    readonly segments: readonly { readonly groupIdB64: string; readonly groupIdHex: string }[];
    readonly headGroupIdB64: string;
    readonly conversationHex: string;
  }): Promise<void>;
}

const INERT: MembershipEndApi = {
  isEnding: () => false,
  isChannelEnding: () => false,
  notices: () => [],
  noticeForRoute: () => undefined,
  leftHere: () => false,
  dismissNotice: () => undefined,
  claimNotice: () => undefined,
  isNoticeClaimed: () => false,
  leaveVersion: () => 0,
  noteLeaveRequested: () => undefined,
  registerOpenDraft: () => () => undefined,
  registerCallStop: () => () => undefined,
  registerJournalReader: () => () => undefined,
  onGroupWiped: () => () => undefined,
  settledVersion: () => 0,
  endChannel: () => Promise.resolve(),
  noteJoined: () => Promise.resolve(),
  noteChannelJoined: () => Promise.resolve(),
  channelLeftElsewhere: () => undefined,
  settle: () => Promise.resolve(),
  joinAlone: (join) => join(),
  startOver: () => Promise.resolve(),
  readJournal: () => Promise.resolve(false),
  heldWelcomes: () => [],
  holdWelcome: () => Promise.resolve(),
  dropHeldWelcome: () => Promise.resolve(),
  stillActiveGroupOf: () => null,
  deleteConversation: () => Promise.resolve(),
};

const MembershipEndContext = createContext<MembershipEndApi>(INERT);

function hexOfB64(idB64: string): string {
  return groupIdHex(base64Decode(idB64));
}

function readEpoch(group: ActiveGroup["group"]): bigint | undefined {
  try {
    return group.epoch;
  } catch {
    return undefined;
  }
}

/** The people in a roster, or `null` on a torn read. */
function rosterPeople(group: ActiveGroup["group"]): string[] | null {
  try {
    const leaves = group.members() as unknown as RosterLeaf[];
    return [...new Set(leaves.map((m) => base64Encode(personKeyOf(m))))];
  } catch {
    return null;
  }
}

function logFailure(what: string, err: unknown): void {
  // eslint-disable-next-line no-console
  console.warn(`[membership-end] ${what}`, err instanceof Error ? err.message : "");
}

export function MembershipEndProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const sync = useDeviceGroupSync();
  const lineage = useLineage();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const groupClosed = useGroupClosed();
  const pins = usePins();
  const sharedChannels = useSharedChannels();
  const readWatermarks = useReadWatermarks();
  const callLog = useCallLog();
  const translation = useTranslation();

  const store = new MembershipEndStore(new IdbMembershipEndBackend(getDeviceKey));
  const [version, setVersion] = createSignal(0);
  onCleanup(store.subscribe(() => setVersion((v) => v + 1)));
  /** The account whose row is loaded. */
  const [openFor, setOpenFor] = createSignal<string | null>(null);
  /** Groups hidden between the trigger and the record reaching disk. */
  const [starting, setStarting] = createSignal<ReadonlySet<string>>(new Set());
  const [leaveVersion, setLeaveVersion] = createSignal(0);
  const [settledVersion, setSettledVersion] = createSignal(0);

  const openDrafts = new Map<string, () => string>();
  let callStop: ((groupIdB64: string) => void) | null = null;
  let journalReader: ((groupIdB64: string) => Promise<boolean>) | null = null;
  const wipedListeners = new Set<(groupIdB64: string) => void>();
  const startingKeys = new Set<string>();
  /** Ends a commit set off and that have not reached disk. */
  const startsInFlight = new Set<Promise<void>>();
  /** A wipe and a Welcome's join run one at a time. */
  let wipeOrJoin: Promise<unknown> = Promise.resolve();
  function oneAtATime<T>(work: () => Promise<T>): Promise<T> {
    const run = wipeOrJoin.then(work, work);
    wipeOrJoin = run.catch(() => undefined);
    return run;
  }
  /** Channels subscribed here whose `joined` register is not written yet. A
   *  channel has no epoch to tell a stale `ended` by, so until it is, the
   *  register must not take the new subscription away. */
  const channelJoinsOwed = new Set<string>();
  /** Groups whose rows the message log refuses in this page. */
  const blockedHere = new Set<string>();
  const answeringKeys = new Set<string>();

  const accountNow = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  /** Every key this account's leaves can be bound to. */
  function ownPersonKeys(): string[] {
    const out = new Set<string>();
    const device = ownPersonKeyB64(chats.identity());
    if (device !== null) out.add(device);
    const account = chats.accountIdentity();
    if (account !== null) out.add(base64Encode(account.signaturePublicKey));
    for (const id of chats.ownPredecessorIdentities()) out.add(base64Encode(id.signaturePublicKey));
    return [...out];
  }

  const contexts: GroupWipeContexts = {
    forgetPins: (g) => pins.forgetConversation(g),
    forgetGroupName: (g) => groupNames.forgetGroup(g),
    forgetGroupClosed: (g) => groupClosed.forget(g),
    forgetReadWatermarks: (g) => readWatermarks.forgetGroup(g),
    forgetSharedChannels: (g) => sharedChannels.forgetGroup(g),
    forgetCallLog: (g) => callLog.forgetGroup(g),
    forgetTranslations: (g) => translation?.forgetConversation(g) ?? Promise.resolve(),
  };

  /** Delete one group's MLS state and leave it durable. `"replaced"` when the
   *  group stored under this id is one this device is in again. */
  async function deleteMlsGroup(
    account: string,
    entry: ActiveGroup,
    record: EndRecord | null,
    isHead: boolean,
  ): Promise<"deleted" | "replaced"> {
    const identity = chats.identity();
    if (identity === null) throw new Error("no canonical identity");
    try {
      if (record === null || (record.origin === "applied" && isHead)) {
        entry.group.deleteLocally(identity);
      } else {
        // On the word of the register. An older segment of a conversation
        // that moved may still be active: it goes with the conversation, at
        // its own epoch.
        const ended = isHead && record !== null ? BigInt(record.endedEpoch) : readEpoch(entry.group);
        if (ended === undefined) throw new Error("the group's epoch cannot be read");
        entry.group.deleteAfterEndedElsewhere(identity, ended);
      }
    } catch (err) {
      if (stillActiveGroupIdOf(err) !== null) return "replaced";
      throw err;
    }
    await chats.persistSnapshotDurable(account);
    return "deleted";
  }

  /** Whether the group now under this id is one this device came back to. */
  function cameBack(entry: ActiveGroup, record: EndRecord): boolean {
    if (readIsActive(entry.group) !== true) return false;
    if (record.origin === "applied") return true;
    const epoch = readEpoch(entry.group);
    return epoch !== undefined && epoch > BigInt(record.endedEpoch);
  }

  const port: EndPort = {
    async publishEnd(record) {
      await sync.writeConversationEnd(
        { kind: record.refKind, id: hexToBytes(record.idHex) ?? new Uint8Array(0) },
        "ended",
        BigInt(record.endedEpoch),
      );
      // 2026-11-13: the account's subscription register says the account
      // left; the other devices drop the channel, and this device does not
      // add it back from a live register after the wipe. Retried (the step
      // rejects) while the register is not loaded.
      // A channel orphaned by a key reset is never written to the register.
      if (record.refKind === 3 && !channelsState.orphanedByKeyReset().has(record.idHex)) {
        const account = accountNow();
        if (account === null) throw new Error("no session");
        await leaveChannelSubscriptionHere(account, record.idHex);
      }
    },
    async stopReceiving(record) {
      const account = accountNow();
      if (account === null) throw new Error("no session");
      if (record.refKind === 3) {
        // 2026-11-13: the account's devices share one subscriber key per
        // channel, so a device that heard of the end from another device of
        // the account tells the service nothing: that device already left
        // with the same key.
        if (record.origin === "heard") return;
        const keys = channelsState.subscriberKeysByChannel().get(record.idHex);
        if (keys !== undefined) {
          const outcome = await leaveChannelOnServer(record.idHex, keys);
          forgetUploadedCard(record.idHex, keys);
          if (outcome.kind !== "left") {
            // The row it leaves behind goes with the 90-day subscriber prune.
            logFailure("leaving a channel on the server failed; dropped locally", null);
          }
        }
        return;
      }
      // The poll and the beacon stop on `isEnding`, the rotation on the
      // group's own state. What is left is the call and the push row.
      for (const groupIdB64 of record.segmentsB64) {
        callStop?.(groupIdB64);
        const secret = await getGroupRoutingSecretForAccount(account, groupIdB64);
        // None means nothing was ever subscribed for this group.
        if (secret !== null) await retireOldPushGid(account, secret);
      }
      // ADR-0078 section 9 - leaving a group rotates this account's `group`
      // and `member` reach keys. The batch that ended the group is not
      // settled any more, so the rotation it used to set off is asked for
      // here. Best effort and not awaited: the wipe does not wait for it.
      const head = record.segmentsB64[record.segmentsB64.length - 1];
      if (record.origin === "applied" && head !== undefined) void profiles.groupShrank(head);
    },
    wipe: (record) => oneAtATime(async () => {
      const account = accountNow();
      if (account === null) throw new Error("no session");
      if (record.refKind === 3) {
        channelsActions.clearSubscriberKeys(record.idHex);
        channelsActions.removeChannel(record.idHex);
        await updateFeedMarks(account, (m) => withoutConversation(m, record.idHex));
        return "wiped";
      }
      const headB64 = record.segmentsB64[record.segmentsB64.length - 1];
      const head = headB64 === undefined ? undefined : chats.get(headB64);
      if (head !== undefined && cameBack(head, record)) return "replaced";
      // The registry and the identity below are this account's only while
      // it stays signed in.
      const stillHere = (): void => {
        if (accountNow() !== account) throw new Error("the session changed");
      };
      const conversationHex = lineage.conversationHexOf(hexToBytes(record.idHex) ?? new Uint8Array(0));
      for (const groupIdB64 of record.segmentsB64) {
        const groupHex = hexOfB64(groupIdB64);
        const isHead = groupIdB64 === headB64;
        // Whatever was applying to this group has finished once this runs.
        // The list itself runs outside the lock: its stores take the routing
        // row lock, and a rotation holds that one while it waits for this.
        await withGroupMlsLock(groupIdB64, () => Promise.resolve());
        const failed = await runWipeSteps(
          groupWipeSteps({ accountIdB64: account, groupIdB64, groupIdHex: groupHex, conversationHex }, contexts),
        );
        if (failed.length > 0) throw new Error(`wipe steps failed: ${failed.join(",")}`);
        const replaced = await withGroupMlsLock(groupIdB64, async () => {
          stillHere();
          const entry = chats.get(groupIdB64);
          if (entry === undefined) return false;
          if ((await deleteMlsGroup(account, entry, record, isHead)) === "replaced") return true;
          // LAST: until here a failure leaves the record owed and the group
          // reachable by the retry.
          chats.remove(groupIdB64);
          return false;
        });
        if (replaced) return "replaced";
        // The row names the groups held: this write takes the group out.
        await chats.persistSnapshotDurable(account);
        const known = loadKnownGroupIds(account);
        if (known.includes(groupIdB64)) {
          saveKnownGroupIds(known.filter((id) => id !== groupIdB64), account);
        }
        for (const listener of wipedListeners) listener(groupIdB64);
      }
      return "wiped";
    }),
    async clearSynced(record) {
      const id = hexToBytes(record.idHex);
      if (id === null) return;
      if (record.refKind === 3) {
        await applyEdits(savedAndFolderEdits({ kind: 3, id }));
        return;
      }
      const conversationId = lineage.conversationIdOf(id);
      await applyEdits(savedAndFolderEdits({ kind: 2, id: conversationId }));
      const flags = sync.getFlags(conversationId);
      if (flags !== undefined && (flags.pinned || flags.muted || flags.archived || flags.unreadOverride)) {
        await sync.setFlags(conversationId, {
          pinned: false,
          muted: false,
          archived: false,
          unreadOverride: false,
        });
      }
      const draft = sync.getDraft(conversationId);
      if (draft !== undefined && draft.text.length > 0) await sync.setDraft(conversationId, "");
      // The cursor has no tombstone: a position that decodes to nothing is
      // how it reads as none held.
      if (sync.getReadCursor(conversationId) !== undefined) {
        await sync.setReadCursor(conversationId, new Uint8Array(0), 0n);
      }
      const order = sync.order();
      const hex = groupIdHex(conversationId);
      if (order !== null && order.groupIds.some((g) => groupIdHex(g) === hex)) {
        await sync.setOrder(order.groupIds.filter((g) => groupIdHex(g) !== hex));
      }
    },
  };

  /** The registers naming a conversation: its saved messages and every
   *  folder that lists it by hand or pins it. */
  function savedAndFolderEdits(ref: ConversationRef): RegisterEdit[] {
    const rule = ruleIdOfRef(ref);
    const edits: RegisterEdit[] = [];
    for (const s of sync.saved()) {
      if (ruleIdOfRef(s.ref) === rule) edits.push(unsaveEdit(s.ref, s.messageId));
    }
    for (const folder of sync.folders()) {
      for (const member of [...folder.included, ...folder.excluded]) {
        if (ruleIdOfRef(member) === rule) {
          edits.push({ t: "member", folderIdHex: folder.idHex, ref: member, state: 0 });
        }
      }
      if (folder.pinned.some((p) => ruleIdOfRef(p) === rule)) {
        edits.push({
          t: "pins",
          folderIdHex: folder.idHex,
          pinned: folder.pinned.filter((p) => ruleIdOfRef(p) !== rule),
        });
      }
    }
    return edits;
  }

  async function applyEdits(edits: readonly RegisterEdit[]): Promise<void> {
    for (const batch of chunkSaveEdits(edits)) await sync.applyFolderEdits(batch);
  }

  // UX section 9 (2026-11-06): the notices live in memory only.
  const [notices, setNotices] = createSignal<readonly EndNotice[]>([]);
  const claimed = new Set<string>();
  const dropNotice = (key: string): void => {
    setNotices((list) => (list.some((n) => n.key === key) ? list.filter((n) => n.key !== key) : list));
  };
  const runtime = new MembershipEndRuntime(store, port, undefined, (notice) => {
    setNotices((list) => [...list.filter((n) => n.key !== notice.key), notice]);
  });

  function publishSettled(): void {
    setSettledVersion((v) => v + 1);
  }

  function blockRows(account: string, groupIdB64: string): void {
    blockedHere.add(groupIdB64);
    blockGroupInMessageLog(account, hexOfB64(groupIdB64));
  }

  function allowRows(account: string, groupIdB64: string): void {
    blockedHere.delete(groupIdB64);
    allowGroupInMessageLog(account, hexOfB64(groupIdB64));
  }

  /** Start an end: hidden and blocked at once, before the first await. */
  async function start(end: NewEnd): Promise<void> {
    const account = accountNow();
    if (account === null) return;
    const key = endKey(end.refKind, end.idHex);
    if (startingKeys.has(key) || store.record(key) !== undefined) return;
    startingKeys.add(key);
    const recordedAtMs = Date.now();
    // Blocked first: hiding it closes an open view, which writes on its way
    // out.
    for (const g of end.segmentsB64) blockRows(account, g);
    setStarting((s) => new Set([...s, ...end.segmentsB64]));
    try {
      await runtime.begin(end);
      // UX 6.2 — a request sent before this end is history: left behind, it
      // made the invite page show "Request sent." for a request that was
      // never posted (seen 2026-11-23). One sent after the end stays.
      if (end.refKind === 2) for (const g of end.segmentsB64) forgetRequestsSent(g, recordedAtMs);
    } catch (err) {
      // The record is not on disk, so nothing happened. The trigger is asked
      // again at the next commit and at the next launch.
      for (const g of end.segmentsB64) allowRows(account, g);
      logFailure("the end could not be recorded", err);
    } finally {
      startingKeys.delete(key);
      setStarting((s) => new Set([...s].filter((g) => !end.segmentsB64.includes(g))));
      publishSettled();
    }
  }

  function titleOf(entry: ActiveGroup): string {
    const shared = groupNames.nameOf(entry.groupIdB64);
    if (shared !== null) return shared;
    const own = new Set(ownPersonKeys());
    const names = (rosterPeople(entry.group) ?? [])
      .filter((k) => !own.has(k))
      .map((k) => memberNameOf(profiles.resolve(k), "a membership end title"));
    return groupTitle(entry.label, names);
  }

  function draftOf(groupIdB64: string): string | null {
    try {
      const text = openDrafts.get(groupIdB64)?.() ?? "";
      return text.trim().length > 0 ? text : null;
    } catch {
      return null;
    }
  }

  function endOfGroup(entry: ActiveGroup, origin: NewEnd["origin"], endedEpoch: bigint): NewEnd {
    const account = accountNow();
    const held = lineage.resolve(entry.groupIdB64)?.held ?? [entry];
    const segmentsB64 = [
      ...held.map((g) => g.groupIdB64).filter((g) => g !== entry.groupIdB64),
      entry.groupIdB64,
    ];
    return {
      refKind: 2,
      idHex: groupIdHex(entry.group.groupId),
      segmentsB64,
      routeIdsB64: [...segmentsB64, base64Encode(lineage.conversationIdOf(entry.group.groupId))],
      title: titleOf(entry),
      // Heard or applied: a device that asked to leave shows no notice either
      // way. The register does not say, so another device of the account does.
      left: account !== null && loadLeaveRequestedAt(account, entry.groupIdB64) !== undefined,
      origin,
      endedEpoch,
      draft: draftOf(entry.groupIdB64),
    };
  }

  /** The seam: did this group's last commit end this account's membership? */
  function checkGroup(groupIdB64: string): void {
    if (openFor() === null || openFor() !== accountNow()) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    // Only the conversation's head speaks for it: an older group of a
    // conversation that moved can end without ending the conversation.
    const view = lineage.resolve(groupIdB64);
    if (view !== undefined && view.head.groupIdB64 !== groupIdB64) return;
    const triggered = endTriggered({
      kind: conversationKindOf(entry.group),
      isActive: readIsActive(entry.group),
      rosterPersonKeysB64: rosterPeople(entry.group),
      ownPersonKeysB64: ownPersonKeys(),
    });
    if (!triggered) return;
    const epoch = readEpoch(entry.group);
    if (epoch === undefined) return;
    const started = start(endOfGroup(entry, "applied", epoch)).finally(() => {
      startsInFlight.delete(started);
    });
    startsInFlight.add(started);
  }
  onCleanup(onGroupMlsIdle(checkGroup));

  async function writeChannelJoined(channelIdHex: string): Promise<void> {
    const id = hexToBytes(channelIdHex);
    if (id === null) return;
    channelJoinsOwed.add(channelIdHex);
    try {
      await sync.writeConversationEnd({ kind: 3, id }, "joined", 0n);
      channelJoinsOwed.delete(channelIdHex);
    } catch (err) {
      // Asked again when the register is next read.
      logFailure("the joined register was not written", err);
    }
  }

  /** A register says a conversation ended. */
  function reactToRegister(ref: ConversationRef, epoch: bigint): void {
    if (ref.kind === 3) {
      const hex = bytesToHex(ref.id);
      if (channelJoinsOwed.has(hex)) {
        void writeChannelJoined(hex);
        return;
      }
      // An owner's channel has no unsubscribe, and a channel it runs is not
      // one a sibling's register can take away.
      if (channelsState.keysByChannel().has(hex)) return;
      const channel = channelsState.channels().find((c) => c.id === hex);
      if (channel === undefined) return;
      const shared = channelsState.metaByChannel().get(hex)?.name ?? channel.localName;
      void start({
        refKind: 3,
        idHex: hex,
        segmentsB64: [],
        routeIdsB64: [],
        title: shared,
        left: false,
        origin: "heard",
        endedEpoch: 0n,
        draft: null,
      });
      return;
    }
    if (ref.kind !== 2) return;
    const entry = chats.get(base64Encode(ref.id));
    const verdict = heardRegisterVerdict({
      held: entry !== undefined,
      isActive: entry === undefined ? undefined : readIsActive(entry.group),
      epoch: entry === undefined ? undefined : readEpoch(entry.group),
      registerEpoch: epoch,
    });
    if (verdict === "nothing" || entry === undefined) return;
    if (verdict === "end") {
      void start(endOfGroup(entry, "heard", epoch));
      return;
    }
    // This device came back after that end. Nothing stops; the other devices
    // are told, and the register stops naming the conversation.
    const key = endKey(2, bytesToHex(ref.id));
    if (answeringKeys.has(key)) return;
    answeringKeys.add(key);
    const own = readEpoch(entry.group);
    void (own === undefined ? Promise.resolve(false) : sync.writeConversationEnd(ref, "joined", own))
      .catch((err: unknown) => {
        logFailure("the joined register was not written; retried at the next fold", err);
      })
      .finally(() => answeringKeys.delete(key));
  }

  // The account's row, and everything owed from a page that closed.
  createEffect(() => {
    const account = accountNow();
    untrack(() => {
      if (account === store.boundAccount()) return;
      setOpenFor(null);
      // Another account (or none): this one's notices are not shown.
      setNotices([]);
      claimed.clear();
      if (account === null) {
        store.close();
        return;
      }
      void store.open(account).then(() => {
        if (store.boundAccount() !== account) return;
        for (const r of store.records()) {
          for (const g of r.segmentsB64) blockRows(account, g);
        }
        setOpenFor(account);
      });
    });
  });

  // Owed records go when the synced state is up, and again on a timer: a step
  // waits for nothing else.
  createEffect(() => {
    if (openFor() === null || !sync.ready()) return;
    void untrack(() => runtime.resume()).then(publishSettled);
  });
  const resumeTimer = setInterval(() => {
    if (store.records().length > 0 && openFor() !== null) void runtime.resume().then(publishSettled);
  }, RESUME_INTERVAL_MS);
  onCleanup(() => clearInterval(resumeTimer));

  // The launch sweep over stored groups: a commit applied and persisted just
  // before a crash ended the group with nobody left to act on it.
  createEffect(() => {
    if (openFor() === null || !sync.ready()) return;
    const groups = chats.groups();
    chats.identity();
    chats.accountIdentity();
    chats.ownPredecessorIdentities();
    untrack(() => {
      for (const g of groups) {
        checkGroup(g.groupIdB64);
        // Any join, not only the mailbox's: a group this page refused rows
        // for is back, active, with no end under way.
        if (blockedHere.has(g.groupIdB64) && !isEnding(g.groupIdB64) && readIsActive(g.group) === true) {
          const account = accountNow();
          if (account !== null) allowRows(account, g.groupIdB64);
        }
      }
    });
  });

  // The reaction to the register: after every fold, and at launch.
  createEffect(() => {
    if (openFor() === null || !sync.ready()) return;
    const ended = sync.endedConversations();
    chats.groups();
    channelsState.channels();
    untrack(() => {
      for (const e of ended) reactToRegister(e.ref, e.epoch);
    });
  });

  function isEnding(groupIdB64: string): boolean {
    version();
    if (starting().has(groupIdB64)) return true;
    return store.records().some((r) => r.segmentsB64.includes(groupIdB64));
  }

  const api: MembershipEndApi = {
    isEnding,
    isChannelEnding(channelIdHex) {
      version();
      return store.record(endKey(3, channelIdHex)) !== undefined;
    },
    notices,
    noticeForRoute(idB64) {
      return notices().find((n) => n.routeIdsB64.includes(idB64));
    },
    leftHere(idB64) {
      version();
      return store.records().some((r) => r.left && r.routeIdsB64.includes(idB64));
    },
    dismissNotice(key) {
      dropNotice(key);
    },
    claimNotice(key) {
      claimed.add(key);
    },
    isNoticeClaimed(key) {
      return claimed.has(key);
    },
    leaveVersion,
    noteLeaveRequested: () => setLeaveVersion((v) => v + 1),
    registerOpenDraft(groupIdB64, read) {
      openDrafts.set(groupIdB64, read);
      return () => {
        if (openDrafts.get(groupIdB64) === read) openDrafts.delete(groupIdB64);
      };
    },
    registerCallStop(stop) {
      callStop = stop;
      return () => {
        if (callStop === stop) callStop = null;
      };
    },
    registerJournalReader(read) {
      journalReader = read;
      return () => {
        if (journalReader === read) journalReader = null;
      };
    },
    onGroupWiped(listener) {
      wipedListeners.add(listener);
      return () => {
        wipedListeners.delete(listener);
      };
    },
    settledVersion,
    async endChannel(channelIdHex, title, removed) {
      await start({
        refKind: 3,
        idHex: channelIdHex,
        segmentsB64: [],
        routeIdsB64: [],
        title,
        left: removed !== true,
        origin: "applied",
        endedEpoch: 0n,
        draft: null,
      });
    },
    async noteJoined(entry) {
      const account = accountNow();
      if (account === null) return;
      const hex = groupIdHex(entry.group.groupId);
      const key = endKey(2, hex);
      // The sentence would be false now: this device is in the group. A
      // dialog showing it over the open conversation goes with it (the route
      // shows the view again), and the text it held goes back into the
      // composer (UX `group-and-channel-privacy.md` 9.2).
      const keptDraft = draftToRestore(notices().find((n) => n.key === key));
      dropNotice(key);
      if (store.record(key) !== undefined) {
        await runtime.run(key);
        // Only a tombstone step can be left: this device is in the group, so
        // the record has nothing more to say about it.
        if (store.record(key) !== undefined) {
          await store.finish(key).catch((err: unknown) => logFailure("record not closed", err));
        }
      }
      allowRows(account, entry.groupIdB64);
      if (keptDraft !== null) {
        // After the end's steps, which clear the conversation's draft.
        void sync
          .saveDraft(entry.group.groupId, keptDraft)
          .catch((err: unknown) => logFailure("the kept draft was not put back", err));
      }
      const epoch = readEpoch(entry.group);
      if (epoch === undefined) return;
      await sync
        .writeConversationEnd({ kind: 2, id: entry.group.groupId }, "joined", epoch)
        // The reaction to the register sends it once the fold is up.
        .catch((err: unknown) => logFailure("the joined register was not written yet", err));
    },
    noteChannelJoined: writeChannelJoined,
    channelLeftElsewhere(channelIdHex) {
      const id = hexToBytes(channelIdHex);
      if (id !== null) reactToRegister({ kind: 3, id }, 0n);
    },
    async settle() {
      // The commit that ended a group may have been applied a moment ago.
      for (const g of chats.groups()) {
        if (readIsActive(g.group) === false) checkGroup(g.groupIdB64);
      }
      await Promise.all(startsInFlight);
      await runtime.resume();
    },
    joinAlone: oneAtATime,
    async startOver(prior) {
      if (prior === undefined || readIsActive(prior.group) !== false) return;
      const account = accountNow();
      if (account === null) return;
      const groupHex = hexOfB64(prior.groupIdB64);
      const conversationHex = lineage.conversationHexOf(hexToBytes(groupHex) ?? new Uint8Array(0));
      const failed = await runWipeSteps(
        groupWipeSteps({ accountIdB64: account, groupIdB64: prior.groupIdB64, groupIdHex: groupHex, conversationHex }, contexts),
      );
      if (failed.length > 0) logFailure("the old group's stores were not all wiped", new Error(failed.join(",")));
    },
    readJournal(groupIdB64) {
      return journalReader === null ? Promise.resolve(false) : journalReader(groupIdB64);
    },
    heldWelcomes() {
      version();
      return store.welcomes();
    },
    holdWelcome: (welcome) => store.holdWelcome(welcome),
    dropHeldWelcome: (messageId) => store.dropWelcome(messageId),
    stillActiveGroupOf(err) {
      const id = stillActiveGroupIdOf(err);
      return id === null ? null : base64Encode(id);
    },
    async deleteConversation(input) {
      const account = accountNow();
      if (account === null) throw new Error("no session");
      for (const s of input.segments) blockRows(account, s.groupIdB64);
      await deleteConversationStores({
        accountIdB64: account,
        segments: input.segments,
        headGroupIdB64: input.headGroupIdB64,
        conversationHex: input.conversationHex,
        chats,
        contexts,
        // A `groupStillActive` refusal is not a failure here: the
        // conversation's MLS state stays on disk, INERT (no row names it, so
        // nothing loads, reads or sends in it again), and everything else is
        // dropped with the registry row last. That is the delete of the
        // closed one-to-one of a contact who started over (UX 16.11, walk of
        // 2026-11-28): nobody ever removed this device from that group and
        // nobody is left to, so `Group::delete` refuses it; iOS
        // `removeGroupLocally` tolerates the same refusal.
        deleteGroup: async (groupIdB64) => {
          await withGroupMlsLock(groupIdB64, async () => {
            const entry = chats.get(groupIdB64);
            if (entry !== undefined) await deleteMlsGroup(account, entry, null, true);
          });
        },
      });
      // The row names the groups held.
      await chats.persistSnapshotDurable(account);
      for (const s of input.segments) {
        for (const listener of wipedListeners) listener(s.groupIdB64);
      }
    },
  };

  // ADR-0087 section 9 (phase B) — a `410 channel-deleted` from any channels
  // route ends that channel on this device: the owner deleted it.
  installChannelDeletedHandler(async (idHex) => {
    // A publisher request for a channel that is gone waits for nothing.
    const account = accountNow();
    if (account !== null) dropPendingPublisherRequest(account, idHex);
    const held = channelsState.channels().find((c) => c.id === idHex);
    if (held === undefined && !channelsState.keysByChannel().has(idHex)) return;
    const title = channelsState.metaByChannel().get(idHex)?.name ?? held?.localName ?? "";
    await api.endChannel(idHex, title, true);
  });
  onCleanup(() => installChannelDeletedHandler(null));

  return <MembershipEndContext.Provider value={api}>{props.children}</MembershipEndContext.Provider>;
}

/** The end of a membership, or an inert stand-in outside the provider. */
export function useMembershipEnd(): MembershipEndApi {
  return useContext(MembershipEndContext);
}
