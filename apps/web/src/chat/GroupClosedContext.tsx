// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5) — the session-wide closed-group runtime. The
 * twin of `GroupNameContext.tsx`, for the group-closed notice instead of
 * the name.
 *
 * Owns, for the signed-in account:
 *
 *   * the store of closed groups, keyed on the base64 group id
 *     (`group-closed-store.ts`, encrypted in IndexedDB by
 *     `group-closed-idb.ts`);
 *   * the RECEIVE gate — the role-table verdict run under the same
 *     per-`(account, group)` serialising lock the group name uses (C-62
 *     sub-invariant 5), from both the foreground poll and the background
 *     receive;
 *   * the SEND side — the recreating admin posts the notice into the OLD
 *     group naming the new one (`close`): store first (this device decided),
 *     then publish; a send failure is reported to the caller, who keeps
 *     the sheet open with it.
 *
 * No growth re-send: a closed group gains no members (the UX says a device
 * that was not invited to the successor keeps the bar alone, permanently).
 *
 * Mounted beside `<GroupNameProvider>`, for the same reasons.
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
import { withRoutingRowLock } from "../push/group-routing";
import { loadWasm } from "../wasm";
import { useChats, type ActiveGroup } from "./ChatContext";
import { ChatClient, type IncomingChatEvent } from "./client";
import { conversationKindOf } from "./conversation-kind";
import { IdbGroupClosedBackend } from "./group-closed-idb";
import {
  GroupClosedStore,
  groupClosedVerdict,
  groupClosedVerdictAccepts,
  type GroupClosedVerdict,
  type StoredGroupClosed,
} from "./group-closed-store";
import { readIsActive } from "./membership-ended";
import { ownPersonKeyB64 } from "./person-key";

/** The receive-loop event this runtime consumes. */
export type GroupClosedEvent = Extract<IncomingChatEvent, { kind: "group-closed" }>;

export interface GroupClosedApi {
  /** Bumped on any change. `closedOf` reads it, so a reactive caller
   *  re-renders on its own. */
  readonly version: Accessor<number>;
  /** The stored notice for a group, `undefined` when it is not closed.
   *  Reactive. */
  closedOf(groupIdB64: string): StoredGroupClosed | undefined;
  /** Run an incoming envelope through the gate and, on accept, close the
   *  group in place. Never throws; `null` when there was nothing to decide. */
  receive(entry: ActiveGroup, event: GroupClosedEvent): Promise<GroupClosedVerdict | null>;
  /** Close `entry` as the recreating admin, naming the successor: store
   *  first, then publish into the OLD group. Throws the send failure. */
  close(entry: ActiveGroup, successorGroupId: Uint8Array | null): Promise<void>;
  /** Forget a group's notice (`Delete this chat`). */
  forget(groupIdB64: string): Promise<void>;
}

const GroupClosedContext = createContext<GroupClosedApi>();

export function GroupClosedProvider(props: { children: JSX.Element }): JSX.Element {
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

  const store = new GroupClosedStore(new IdbGroupClosedBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[group-closed] store persist failed", err);
  });
  const unsubscribeStore = store.subscribe(bump);
  let opened: Promise<boolean> = Promise.resolve(true);

  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const account = accountId();
      if (account === null) return;
      await chats.persistSnapshot(account);
    },
  });

  async function send(entry: ActiveGroup, envelope: Uint8Array): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) throw new Error("Sign in again.");
    if (readIsActive(entry.group) === false) throw new Error("You are no longer in this group.");
    await publisher.broadcastControl(secrets, entry.group, envelope);
  }

  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      if (account === null) {
        store.close();
        opened = Promise.resolve(true);
        return;
      }
      opened = store.open(account);
    });
  });
  onCleanup(() => {
    unsubscribeStore();
  });

  /** A stamp strictly after whatever this device holds for the group. */
  function freshStamp(groupIdB64: string): bigint {
    const now = BigInt(Date.now());
    const held = store.get(groupIdB64);
    return held !== undefined && held.closedAtMs >= now ? held.closedAtMs + 1n : now;
  }

  const api: GroupClosedApi = {
    version,
    closedOf(groupIdB64) {
      version();
      return store.get(groupIdB64);
    },
    async receive(entry, event) {
      const account = accountId();
      if (account === null) return null;
      const incoming = event.groupClosed;
      if (incoming === undefined) return null;
      const groupIdB64 = entry.groupIdB64;
      try {
        return await withRoutingRowLock(account, groupIdB64, async () => {
          const readable = await opened;
          const stored = store.get(groupIdB64) ?? null;
          let ownerKey: Uint8Array | undefined;
          const adminKeys: Uint8Array[] = [];
          let verifyFailed = false;
          try {
            ownerKey = entry.group.roleTableOwner() ?? undefined;
            const flat = entry.group.roleTableAdmins();
            for (let i = 0; i + 32 <= flat.length; i += 32) adminKeys.push(flat.slice(i, i + 32));
          } catch {
            verifyFailed = true;
          }
          let senderKey: Uint8Array | undefined;
          if (event.senderKey !== undefined) {
            try {
              senderKey = base64Decode(event.senderKey);
            } catch {
              senderKey = undefined;
            }
          }
          const wasm = await loadWasm();
          const verdict = groupClosedVerdict({
            kind: conversationKindOf(entry.group),
            ownerKey,
            adminKeys,
            senderKey,
            stored,
            storedUnreadable: !readable,
            incoming,
            verifyFailed,
            isNewer: (a, b) => wasm.groupClosedIsNewer(a, b),
          });
          if (!groupClosedVerdictAccepts(verdict)) {
            // eslint-disable-next-line no-console
            console.warn("group-closed notice REFUSED (ADR-0065 role gate / order)", verdict, "senderLeaf", event.senderLeaf);
            return verdict;
          }
          if (verdict !== "accept-identical") {
            // The closer is the MLS-authenticated sender resolved at receipt
            // (C-68), stored beside the notice so the terminal row can name
            // them after a reload.
            await store.set(account, groupIdB64, {
              closedAtMs: incoming.closedAtMs,
              successorGroupIdB64: incoming.successorGroupIdB64,
              ...(event.senderKey !== undefined ? { closedByKeyB64: event.senderKey } : {}),
            });
          }
          return verdict;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("group-closed receive failed", err);
        return null;
      }
    },
    async close(entry, successorGroupId) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await opened;
        const wasm = await loadWasm();
        const stamp = freshStamp(entry.groupIdB64);
        const envelope = wasm.composeGroupClosedEnvelope(stamp, successorGroupId ?? undefined);
        const decoded = wasm.decodeGroupClosedEnvelope(envelope);
        const successor = decoded.successorGroupId;
        const own = ownPersonKeyB64(chats.identity());
        const value: StoredGroupClosed = {
          closedAtMs: decoded.closedAtMs,
          successorGroupIdB64:
            successor === undefined || successor.length === 0 ? null : base64Encode(successor),
          ...(own !== null ? { closedByKeyB64: own } : {}),
        };
        decoded.free();
        // This device decided: the store holds it before the send, so a
        // failed publish still leaves the admin's own view closed and the
        // caller may retry the send.
        await store.set(account, entry.groupIdB64, value);
        await send(entry, envelope);
      });
    },
    async forget(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      await store.remove(account, groupIdB64);
    },
  };

  return <GroupClosedContext.Provider value={api}>{props.children}</GroupClosedContext.Provider>;
}

export function useGroupClosed(): GroupClosedApi {
  const ctx = useContext(GroupClosedContext);
  if (ctx === undefined) throw new Error("useGroupClosed() called outside <GroupClosedProvider>");
  return ctx;
}
