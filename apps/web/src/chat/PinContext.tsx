// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 3 — the session-wide pin runtime. The twin of
 * `GroupNameContext.tsx`, for pinned messages instead of the group's name.
 *
 * Owns, for the signed-in account:
 *
 *   * the store of pins, shared and personal, keyed on the base64 group id
 *     and the DS `messageId` (`pin-store.ts`, encrypted in IndexedDB by
 *     `pin-idb.ts`);
 *   * the RECEIVE gate for a shared pin — the role-table verdict run under
 *     the same per-`(account, group)` serialising lock the group name and the
 *     push routing secret use (C-62 sub-invariant 5: the gate reads current
 *     state), from both the foreground poll and the background receive;
 *   * the SEND side — a shared pin or unpin composes one envelope, publishes
 *     it through `ChatClient.broadcastControl` and then applies it locally
 *     the way a receiver would (the sender never receives its own message
 *     back), and the GROWTH re-send (`publish-scheduler.ts`, throttled +
 *     jittered) re-emits every held shared pin with its ORIGINAL stamp so a
 *     newcomer sees the room's pins and a later unpin is never clobbered.
 *   * the personal pin, which touches nothing but the store.
 *
 * Mounted inside `<AuthProvider>` and `<ChatProvider>` and above the router,
 * beside `<GroupNameProvider>`, so the background receive and every route
 * share one instance.
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

import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { conversationSeedOf, useAuth } from "../auth/AuthContext";
import { base64Decode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { ProfilePublishScheduler } from "../profile/publish-scheduler";
import { readPushRotationRole, withRoutingRowLock, type PushRotationRole } from "../push/group-routing";
import { loadWasm } from "../wasm";
import { useChats, type ActiveGroup } from "./ChatContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import { readIsActive } from "./membership-ended";
import { conversationKindOf } from "./conversation-kind";
import { IdbPinBackend } from "./pin-idb";
import {
  PinStore,
  freshPinStamp,
  pinVerdict,
  pinVerdictAccepts,
  type PinVerdict,
  type StoredPin,
} from "./pin-store";

/** The receive-loop event this runtime consumes. */
export type PinEvent = Extract<IncomingChatEvent, { kind: "pin" }>;

export interface PinApi {
  /** Bumped on any change the bar may depend on. The readers below read it,
   *  so a reactive caller re-renders on its own. */
  readonly version: Accessor<number>;
  /** The shared pin map of a conversation (pins and stored unpins).
   *  Reactive. */
  shared(groupIdB64: string): ReadonlyMap<number, StoredPin>;
  /** The personal pins of a conversation: messageId -> pinnedAtMs.
   *  Reactive. */
  personal(groupIdB64: string): ReadonlyMap<number, bigint>;
  /** Whether this device may set or clear a shared pin in the group, read
   *  off the LIVE group now: either participant of a one-to-one, else the
   *  ADR-0022 owner / admin, else any member of a table-less group. */
  maySetShared(group: WasmGroup): Promise<boolean>;
  /** Run an incoming envelope through the gate and, on accept, store it.
   *  Never throws; `null` when there was nothing to decide (no session, a
   *  malformed envelope). */
  receive(entry: ActiveGroup, event: PinEvent): Promise<PinVerdict | null>;
  /** Pin (`true`) or unpin (`false`) one message for everyone: SEND first,
   *  then store — the bar does not change until the send succeeds. Throws
   *  the send failure. */
  setShared(entry: ActiveGroup, messageId: number, pinned: boolean): Promise<void>;
  /** Pin or unpin one message on this device only. */
  setPersonal(groupIdB64: string, messageId: number, pinned: boolean): Promise<void>;
  /** ADR-0065 R.5 — clear a SHARED pin of an older segment on this device
   *  alone (`pin-bar.ts` `pinRoute`): nothing is published. The unpin is
   *  stored with a fresh stamp, so a late re-send of the pin does not
   *  bring it back. */
  clearSharedLocally(groupIdB64: string, messageId: number): Promise<void>;
  /** The message was deleted: both records go with it (section 4.1). */
  messageDeleted(groupIdB64: string, messageId: number): Promise<void>;
  /** The conversation was deleted from this device: wipe its pins. */
  forgetConversation(groupIdB64: string): Promise<void>;
  /** `groupIdB64` gained at least one member: schedule the re-send. */
  groupGrew(groupIdB64: string): void;
}

const PinContext = createContext<PinApi>();

export function PinProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();

  const [version, setVersion] = createSignal(0);
  const bump = (): void => {
    setVersion((v) => v + 1);
  };

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  const store = new PinStore(new IdbPinBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[pins] store persist failed", err);
  });
  const unsubscribeStore = store.subscribe(bump);
  /** Resolves once the current account's row is loaded; `false` when the row
   *  could not be read at all (the verdict then refuses a changed action). */
  let opened: Promise<boolean> = Promise.resolve(true);

  // The chat client used ONLY to publish pins. Its echoes are shared with
  // every other ChatClient on the page (`chat/own-published.ts`).
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const account = accountId();
      if (account === null) return;
      await chats.persistSnapshot(account);
    },
  });

  async function liveRole(group: WasmGroup): Promise<PushRotationRole> {
    // The role table names ACCOUNT keys (ADR-0050), so the account identity
    // is what `ownRole` is asked with.
    // ADR-0066 section 3.5: the role table names the key the leaves are bound
    // to, which after a proven rotation is the predecessor.
    const account = chats.conversationAccountIdentity();
    if (account !== null) return readPushRotationRole(group, account);
    const secrets = auth.identitySecrets();
    if (secrets === null) return null;
    try {
      const wasm = await loadWasm();
      const identity = wasm.Identity.fromSeed(conversationSeedOf(secrets));
      try {
        return await readPushRotationRole(group, identity);
      } finally {
        identity.free();
      }
    } catch {
      return null;
    }
  }

  /** ADR-0062 — read from the group, not from the people count: a
   *  two-person group keeps the group rule (owner / admin). */
  function isOneToOne(group: WasmGroup): boolean {
    return conversationKindOf(group) === "chat";
  }

  async function maySetShared(group: WasmGroup): Promise<boolean> {
    if (readIsActive(group) === false) return false;
    if (isOneToOne(group)) return true;
    return (await liveRole(group)) !== "member";
  }

  /** Seal + publish one envelope into a group. */
  async function send(entry: ActiveGroup, envelope: Uint8Array): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) throw new Error("Sign in again.");
    if (readIsActive(entry.group) === false) throw new Error("You are no longer in this group.");
    await publisher.broadcastControl(secrets, entry.group, envelope);
  }

  /** Growth — re-send every held shared PIN (never an unpin: a newcomer
   *  holds nothing to undo), each with its ORIGINAL stamp, when the group
   *  grows. Only from a device that may set shared pins. */
  async function republishInto(groupIdB64: string): Promise<void> {
    if (accountId() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    if (readIsActive(entry.group) === false) return;
    const held = [...store.shared(groupIdB64)].filter(([, v]) => v.pinned);
    if (held.length === 0) return;
    if (!(await maySetShared(entry.group))) return;
    const wasm = await loadWasm();
    for (const [messageId, v] of held) {
      // The stamp is re-emitted VERBATIM (the ADR-0044 lesson): re-stamping
      // would make this re-send newer than an unpin that happened since.
      const envelope = wasm.composePinEnvelope(BigInt(messageId), true, v.updatedAtMs);
      await send(entry, envelope);
    }
  }

  const scheduler = new ProfilePublishScheduler<ReturnType<typeof setTimeout>>({
    now: () => Date.now(),
    setTimer: (fn, ms) => setTimeout(fn, ms),
    clearTimer: (t) => clearTimeout(t),
    random: () => Math.random(),
    publish: republishInto,
    onError: (_groupIdB64, err) => {
      // eslint-disable-next-line no-console
      console.warn("[pins] growth re-send failed", err);
    },
  });

  // Bind everything to the signed-in account.
  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      if (account === null) {
        scheduler.stop();
        store.close();
        opened = Promise.resolve(true);
        return;
      }
      scheduler.start();
      opened = store.open(account);
    });
  });
  onCleanup(() => {
    unsubscribeStore();
    scheduler.stop();
  });

  /** Read the live role table into the verdict's inputs. */
  function readRoleTable(group: WasmGroup): {
    ownerKey: Uint8Array | undefined;
    adminKeys: Uint8Array[];
    verifyFailed: boolean;
  } {
    let ownerKey: Uint8Array | undefined;
    const adminKeys: Uint8Array[] = [];
    let verifyFailed = false;
    try {
      ownerKey = group.roleTableOwner() ?? undefined;
      const flat = group.roleTableAdmins();
      for (let i = 0; i + 32 <= flat.length; i += 32) adminKeys.push(flat.slice(i, i + 32));
    } catch {
      verifyFailed = true;
    }
    return { ownerKey, adminKeys, verifyFailed };
  }

  const api: PinApi = {
    version,
    shared(groupIdB64) {
      version();
      return store.shared(groupIdB64);
    },
    personal(groupIdB64) {
      version();
      return store.personal(groupIdB64);
    },
    maySetShared,
    async receive(entry, event) {
      const account = accountId();
      if (account === null) return null;
      const incoming = event.pin;
      // A domain match that failed its bounds: dropped, never stored.
      if (incoming === undefined) return null;
      const groupIdB64 = entry.groupIdB64;
      try {
        return await withRoutingRowLock(account, groupIdB64, async () => {
          const readable = await opened;
          const stored = store.getShared(groupIdB64, incoming.messageId) ?? null;
          const table = readRoleTable(entry.group);
          let senderKey: Uint8Array | undefined;
          if (event.senderKey !== undefined) {
            try {
              senderKey = base64Decode(event.senderKey);
            } catch {
              senderKey = undefined;
            }
          }
          const wasm = await loadWasm();
          const verdict = pinVerdict({
            oneToOne: isOneToOne(entry.group),
            ownerKey: table.ownerKey,
            adminKeys: table.adminKeys,
            senderKey,
            stored,
            storedUnreadable: !readable,
            incoming: { pinned: incoming.pinned, updatedAtMs: incoming.updatedAtMs },
            verifyFailed: table.verifyFailed,
            isNewer: (a, b) => wasm.pinIsNewer(a, b),
          });
          if (!pinVerdictAccepts(verdict)) {
            // eslint-disable-next-line no-console
            console.warn("pin REFUSED (ADR-0052 role gate / order)", verdict, "senderLeaf", event.senderLeaf);
            return verdict;
          }
          if (verdict !== "accept-identical") {
            await store.setShared(account, groupIdB64, incoming.messageId, {
              pinned: incoming.pinned,
              updatedAtMs: incoming.updatedAtMs,
            });
          }
          return verdict;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("pin receive failed", err);
        return null;
      }
    },
    async setShared(entry, messageId, pinned) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await opened;
        const stored = store.getShared(entry.groupIdB64, messageId);
        const stamp = freshPinStamp(Date.now(), stored);
        const wasm = await loadWasm();
        const envelope = wasm.composePinEnvelope(BigInt(messageId), pinned, stamp);
        await send(entry, envelope);
        // Apply locally the way a receiver would: the stamp is by
        // construction strictly newer than what is stored, so the order
        // accepts it; the store is what the bar derives from.
        if (stored === undefined || wasm.pinIsNewer(stamp, stored.updatedAtMs)) {
          await store.setShared(account, entry.groupIdB64, messageId, { pinned, updatedAtMs: stamp });
        }
      });
    },
    async clearSharedLocally(groupIdB64, messageId) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, groupIdB64, async () => {
        await opened;
        const stored = store.getShared(groupIdB64, messageId);
        if (stored === undefined || !stored.pinned) return;
        await store.setShared(account, groupIdB64, messageId, {
          pinned: false,
          updatedAtMs: freshPinStamp(Date.now(), stored),
        });
      });
    },
    async setPersonal(groupIdB64, messageId, pinned) {
      const account = accountId();
      if (account === null) return;
      await opened;
      await store.setPersonal(account, groupIdB64, messageId, pinned ? BigInt(Date.now()) : null);
    },
    async messageDeleted(groupIdB64, messageId) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, groupIdB64, async () => {
        await opened;
        await store.dropMessage(account, groupIdB64, messageId);
      });
    },
    async forgetConversation(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, groupIdB64, async () => {
        await opened;
        await store.forgetGroup(account, groupIdB64);
      });
    },
    groupGrew(groupIdB64) {
      scheduler.groupGrew(groupIdB64);
    },
  };

  return <PinContext.Provider value={api}>{props.children}</PinContext.Provider>;
}

export function usePins(): PinApi {
  const ctx = useContext(PinContext);
  if (ctx === undefined) throw new Error("usePins() called outside <PinProvider>");
  return ctx;
}
