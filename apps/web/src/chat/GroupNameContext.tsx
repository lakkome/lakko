// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 — the session-wide shared-group-name runtime. The twin of
 * `profile/ProfileContext.tsx`, for the group's name instead of a member's.
 *
 * Owns, for the signed-in account:
 *
 *   * the store of shared group names, keyed on the base64 group id
 *     (`group-name-store.ts`, encrypted in IndexedDB by `group-name-idb.ts`);
 *   * the RECEIVE gate — the role-table verdict run under the same
 *     per-`(account, group)` serialising lock the push routing secret uses
 *     (C-62 sub-invariant 5: the gate reads current state), from both the
 *     foreground poll and the background receive;
 *   * the SEND side — a rename or a creation writes the store and publishes
 *     the envelope through `ChatClient.broadcastControl`, and the GROWTH
 *     re-send (`publish-scheduler.ts`, throttled + jittered) re-emits the
 *     held name with its ORIGINAL stamp so a newcomer learns it and a later
 *     rename is never clobbered.
 *
 * Mounted inside `<AuthProvider>` and `<ChatProvider>` and above the router,
 * beside `<ProfileProvider>`, so the mailbox loop, the background receive and
 * every route share one instance.
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
import { IdbGroupNameBackend } from "./group-name-idb";
import {
  GroupNameStore,
  groupNameVerdict,
  groupNameVerdictAccepts,
  type GroupNameVerdict,
  type StoredGroupName,
} from "./group-name-store";
import { readIsActive } from "./membership-ended";
import { conversationKindOf } from "./conversation-kind";

/** The receive-loop event this runtime consumes. */
export type GroupNameEvent = Extract<IncomingChatEvent, { kind: "group-name" }>;

export interface GroupNameApi {
  /** Bumped on any change a title may depend on. `nameOf` reads it, so a
   *  reactive caller re-renders on its own. */
  readonly version: Accessor<number>;
  /** The shared name to show for a group, `null` when none / cleared.
   *  Reactive. */
  nameOf(groupIdB64: string): string | null;
  /** The stored value with its stamp, or `undefined`. Reactive. */
  stored(groupIdB64: string): StoredGroupName | undefined;
  /** Run an incoming envelope through the gate and, on accept, re-title in
   *  place. Never throws; `null` when there was nothing to decide (no
   *  session, a malformed envelope). */
  receive(entry: ActiveGroup, event: GroupNameEvent): Promise<GroupNameVerdict | null>;
  /** Rename (or clear, with an empty name) as the owner / an admin: SEND
   *  first, then store — the local title does not change until the send
   *  succeeds (UX spec §4). Throws the composer's `groupNameError` or the
   *  send failure. */
  rename(entry: ActiveGroup, name: string): Promise<void>;
  /** The creator's name right after the creating commit: store first, then
   *  send best-effort (a failed send is covered by the growth re-send). */
  recordCreated(entry: ActiveGroup, name: string): Promise<void>;
  /** `groupIdB64` gained at least one member: schedule the re-send. */
  groupGrew(groupIdB64: string): void;
  /** This device's ADR-0022 role, read off the LIVE group now. */
  liveRole(group: WasmGroup): Promise<PushRotationRole>;
}

const GroupNameContext = createContext<GroupNameApi>();

export function GroupNameProvider(props: { children: JSX.Element }): JSX.Element {
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

  const store = new GroupNameStore(new IdbGroupNameBackend(getDeviceKey), (err) => {
    // eslint-disable-next-line no-console
    console.warn("[group-name] store persist failed", err);
  });
  const unsubscribeStore = store.subscribe(bump);
  /** Resolves once the current account's row is loaded; `false` when the row
   *  could not be read at all (the verdict then refuses a changed name). */
  let opened: Promise<boolean> = Promise.resolve(true);

  // The chat client used ONLY to publish names. Its echoes are shared with
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
    // is what `ownRole` is asked with. The session-long instance when it is
    // there; a throwaway one from the seed otherwise.
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

  /** Seal + publish one envelope into a group. */
  async function send(entry: ActiveGroup, envelope: Uint8Array): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) throw new Error("Sign in again.");
    if (readIsActive(entry.group) === false) throw new Error("You are no longer in this group.");
    await publisher.broadcastControl(secrets, entry.group, envelope);
  }

  /** Trigger 3 — re-send the held name, with its ORIGINAL stamp, when the
   *  group grows. Owner / admin only (or any member of a table-less group);
   *  a device holding no name, or a clear, has nothing a newcomer needs. */
  async function republishInto(groupIdB64: string): Promise<void> {
    if (accountId() === null) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    if (readIsActive(entry.group) === false) return;
    const held = store.get(groupIdB64);
    if (held === undefined || held.name.length === 0) return;
    if ((await liveRole(entry.group)) === "member") return;
    const wasm = await loadWasm();
    // The stamp is re-emitted VERBATIM (the ADR-0044 lesson): re-stamping
    // would make this re-send newer than a rename that happened since.
    const envelope = wasm.composeGroupNameEnvelope(held.name, held.updatedAtMs);
    await send(entry, envelope);
  }

  const scheduler = new ProfilePublishScheduler<ReturnType<typeof setTimeout>>({
    now: () => Date.now(),
    setTimer: (fn, ms) => setTimeout(fn, ms),
    clearTimer: (t) => clearTimeout(t),
    random: () => Math.random(),
    publish: republishInto,
    onError: (_groupIdB64, err) => {
      // eslint-disable-next-line no-console
      console.warn("[group-name] growth re-send failed", err);
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

  /** A stamp strictly after whatever this device holds for the group, so a
   *  local rename always orders after the name it replaces even when the
   *  setter's clock lags another admin's. */
  function freshStamp(groupIdB64: string): bigint {
    const now = BigInt(Date.now());
    const held = store.get(groupIdB64);
    return held !== undefined && held.updatedAtMs >= now ? held.updatedAtMs + 1n : now;
  }

  /** Compose and read back the normalised value the receivers will see. */
  async function compose(name: string, stamp: bigint): Promise<{ envelope: Uint8Array; value: StoredGroupName }> {
    const wasm = await loadWasm();
    const envelope = wasm.composeGroupNameEnvelope(name, stamp);
    const decoded = wasm.decodeGroupNameEnvelope(envelope);
    const value: StoredGroupName = { name: decoded.name, updatedAtMs: decoded.updatedAtMs };
    decoded.free();
    return { envelope, value };
  }

  const api: GroupNameApi = {
    version,
    nameOf(groupIdB64) {
      version();
      return store.nameOf(groupIdB64);
    },
    stored(groupIdB64) {
      version();
      return store.get(groupIdB64);
    },
    async receive(entry, event) {
      const account = accountId();
      if (account === null) return null;
      const incoming = event.groupName;
      // A domain match that failed its bounds: dropped, never stored.
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
          const verdict = groupNameVerdict({
            kind: conversationKindOf(entry.group),
            ownerKey,
            adminKeys,
            senderKey,
            stored,
            storedUnreadable: !readable,
            incoming,
            verifyFailed,
            isNewer: (a, b) => wasm.groupNameIsNewer(a, b),
          });
          if (!groupNameVerdictAccepts(verdict)) {
            // eslint-disable-next-line no-console
            console.warn("group name REFUSED (ADR-0051 role gate / order)", verdict, "senderLeaf", event.senderLeaf);
            return verdict;
          }
          if (verdict !== "accept-identical") await store.set(account, groupIdB64, incoming);
          return verdict;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("group name receive failed", err);
        return null;
      }
    },
    async rename(entry, name) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await opened;
        const { envelope, value } = await compose(name, freshStamp(entry.groupIdB64));
        await send(entry, envelope);
        await store.set(account, entry.groupIdB64, value);
      });
    },
    async recordCreated(entry, name) {
      const account = accountId();
      if (account === null) return;
      if (name.trim().length === 0) return;
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await opened;
        const { envelope, value } = await compose(name, freshStamp(entry.groupIdB64));
        await store.set(account, entry.groupIdB64, value);
        try {
          await send(entry, envelope);
        } catch (err) {
          // The store holds it; the next growth re-send carries it.
          // eslint-disable-next-line no-console
          console.warn("[group-name] initial send failed", err);
        }
      });
    },
    groupGrew(groupIdB64) {
      scheduler.groupGrew(groupIdB64);
    },
    liveRole,
  };

  return <GroupNameContext.Provider value={api}>{props.children}</GroupNameContext.Provider>;
}

export function useGroupNames(): GroupNameApi {
  const ctx = useContext(GroupNameContext);
  if (ctx === undefined) throw new Error("useGroupNames() called outside <GroupNameProvider>");
  return ctx;
}
