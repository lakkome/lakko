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
import { ownsGroup } from "./group-owner";
import { notePublicRecordChanged } from "./handle-owed";
import { ChatClient, type IncomingChatEvent } from "./client";
import { IdbGroupNameBackend } from "./group-name-idb";
import {
  GroupSettingStore,
  PHOTO_CODEC,
  REACTIONS_CODEC,
  photoKey,
  reactionsKey,
  type StoredGroupPhoto,
  type StoredGroupReactions,
} from "./group-settings-store";
import type { StoredConversationHandle } from "./group-settings-envelope";
import { ALLOW_ALL, type AllowedSetting } from "./reaction-rules";
import { avatarToWasm, b64ToBytes, bytesToB64, type AvatarData } from "../profile/avatar-model";
import type { ProfileAvatar as WasmProfileAvatar } from "../../wasm/lakko_ffi_wasm";
import { usernameCertKey } from "../reach/scoped-discovery";
import { avatarLookupChanged, setConversationAvatarLookup } from "../profile/avatar-runtime";
import { avatarBlobIdsHex } from "../profile/avatar-upload";
import { deleteBlobs } from "../storage/blob-deletes";
import { installGroupPublicLookup } from "../ds/knock-seeds";
import { openRequestsSent } from "./requests-sent";
import { lookupKeyForName } from "../discovery/handle-lookup";
import {
  GroupNameStore,
  groupNameVerdict,
  groupNameVerdictAccepts,
  type GroupNameVerdict,
  type StoredGroupName,
} from "./group-name-store";
import { readIsActive } from "./membership-ended";
import { conversationKindOf } from "./conversation-kind";
import { durableSnapshotHook } from "./persist-hooks";
import {
  groupNamesToStore,
  subscribeCarriedNames,
  takeCarriedGroupNames,
} from "../devices/carried-state";

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
  /** ADR-0073 R.6 step 4 — the conversation continues in `to`: file the
   *  name `from` holds under `to` with its ORIGINAL stamp and send it there
   *  (ADR-0051: a re-send never re-stamps). Idempotent; nothing happens when
   *  `from` holds no name or `to` already holds one at least as new. */
  carryOver(from: ActiveGroup, to: ActiveGroup): Promise<void>;
  /** Drop a group's name (ADR-0087 section 9). */
  forgetGroup(groupIdB64: string): Promise<void>;
  /** `groupIdB64` gained at least one member: schedule the re-send. */
  groupGrew(groupIdB64: string): void;
  /** This device committed an add: the name goes now, not on the growth
   *  schedule, since the newcomer's list has no name until it arrives. The
   *  schedule exists for the other members. */
  addedHere(groupIdB64: string): void;
  /** This device's ADR-0022 role, read off the LIVE group now. */
  liveRole(group: WasmGroup): Promise<PushRotationRole>;
  /** ADR-0087 section 18.1 — the group's photo as this device holds it. */
  photoOf(groupIdB64: string): StoredGroupPhoto | undefined;
  /** `reactions-and-emoji.md` section 9 — what the group takes (all when
   *  nothing is held). */
  reactionsOf(groupIdB64: string): AllowedSetting;
  /** ADR-0087 section 11 — the group's `@name`, as received or set here. */
  handleOf(groupIdB64: string): StoredConversationHandle | undefined;
  /** The role-gated receive of a photo or reactions setting (a handle is
   *  taken from any member: its signature is the gate). */
  receiveSetting(entry: ActiveGroup, event: GroupSettingEvent): Promise<void>;
  /** Owner or admin: set or remove the photo, and send it. */
  setPhoto(entry: ActiveGroup, avatar: AvatarData | null): Promise<void>;
  /** Owner or admin: set what the group takes, and send it. */
  setReactions(entry: ActiveGroup, allowed: AllowedSetting): Promise<void>;
  /** The owner set a new handle envelope: keep it and send it. */
  setHandle(entry: ActiveGroup, value: StoredConversationHandle): Promise<void>;
}

export type GroupSettingEvent = Extract<IncomingChatEvent, { kind: "group-setting" }>;

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
  const photos = new GroupSettingStore<StoredGroupPhoto>("group-photos", PHOTO_CODEC);
  const reactionSettings = new GroupSettingStore<StoredGroupReactions>("group-reactions", REACTIONS_CODEC);
  const handles = new GroupSettingStore<StoredConversationHandle>("group-handles", HANDLE_CODEC);
  const unsubscribePhotos = photos.subscribe(() => {
    bump();
    avatarLookupChanged();
  });
  // UX 4.5.5 — the one avatar circle draws a group from this store.
  setConversationAvatarLookup((groupIdB64) => photos.get(groupIdB64)?.avatar ?? null);
  const unsubscribeReactions = reactionSettings.subscribe(bump);
  const unsubscribeHandles = handles.subscribe(bump);
  // ADR-0087 section 5 — the knock reader asks whether a group is public.
  installGroupPublicLookup((groupIdB64) => handles.get(groupIdB64)?.isPublic === true);
  let settingsOpened: Promise<boolean> = Promise.resolve(true);
  /** Resolves once the current account's row is loaded; `false` when the row
   *  could not be read at all (the verdict then refuses a changed name). */
  let opened: Promise<boolean> = Promise.resolve(true);

  // The chat client used ONLY to publish names. Its echoes are shared with
  // every other ChatClient on the page (`chat/own-published.ts`).
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, accountId),
  });

  async function liveRole(group: WasmGroup): Promise<PushRotationRole> {
    // The role table names ACCOUNT keys (ADR-0050), so the account identity
    // is what `ownRole` is asked with. The session-long instance when it is
    // there; a throwaway one from the seed otherwise.
    // ADR-0074 section 3.5: the role table names the key the leaves are bound
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
  /**
   * UX 4.5.4 — a public group's record carries its name and photo: the owner's
   * device owes its republish when it applies a change to either, whoever
   * made it. Nothing for a private group or a device that does not own it
   * (only the owner holds the handle key). Recorded before the change is kept
   * (no crash loses it) and again after (a republish that ran in between
   * published the old value, and this one carries the new).
   */
  async function owePublicRecord(account: string, entry: ActiveGroup): Promise<void> {
    await settingsOpened;
    const held = handles.get(entry.groupIdB64);
    if (held === undefined || !held.isPublic) return;
    if (
      !ownsGroup(entry.group, [
        chats.conversationAccountIdentity()?.signaturePublicKey,
        chats.accountIdentity()?.signaturePublicKey,
      ])
    ) {
      return;
    }
    await notePublicRecordChanged(account, "group", entry.groupIdB64);
  }

  /** {@link owePublicRecord} on a receive path: a failure is logged and the
   *  change still kept (the next change, or the lease renewal, republishes). */
  async function owePublicRecordQuietly(account: string, entry: ActiveGroup): Promise<void> {
    try {
      await owePublicRecord(account, entry);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[group-handle] republish not recorded", err instanceof Error ? err.name : "");
    }
  }

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
    const wasm = await loadWasm();
    // ADR-0087 section 11 — the handle is signed by the handle key, so any
    // member relays it to a newcomer, verbatim.
    const handle = handles.get(groupIdB64);
    if (handle !== undefined) await send(entry, handle.envelope);
    if ((await liveRole(entry.group)) === "member") return;
    const held = store.get(groupIdB64);
    // The stamp is re-emitted VERBATIM (the ADR-0044 lesson): re-stamping
    // would make this re-send newer than a rename that happened since.
    if (held !== undefined && held.name.length > 0) {
      await send(entry, wasm.composeGroupNameEnvelope(held.name, held.updatedAtMs));
    }
    // ADR-0087 section 18.1 and reactions section 9 — the photo and the
    // allowed reactions travel the same way, with their stamps.
    const photo = photos.get(groupIdB64);
    if (photo !== undefined) await send(entry, composePhotoEnvelope(wasm, photo));
    const reactions = reactionSettings.get(groupIdB64);
    if (reactions !== undefined) await send(entry, composeReactionsEnvelope(wasm, reactions));
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
        photos.close();
        reactionSettings.close();
        handles.close();
        opened = Promise.resolve(true);
        settingsOpened = Promise.resolve(true);
        return;
      }
      scheduler.start();
      opened = store.open(account);
      void openRequestsSent(account);
      settingsOpened = Promise.all([photos.open(account), reactionSettings.open(account), handles.open(account)]).then(
        (r) => r.every((x) => x),
      );
      void takeCarried(account);
    });
  });

  /**
   * The pairing state bundle v3 (`devices/carried-state.ts`): the names the
   * confirming device holds, taken for the bound account and stored under
   * the same per-group lock and the same "strictly newer stamp wins" rule
   * an envelope passes. The bundle comes from this account's own device, so
   * the owner/admin gate of the in-group path does not apply; a one-to-one
   * this device already holds still takes no name (ADR-0062). Nothing is
   * posted to any group.
   */
  async function takeCarried(account: string): Promise<void> {
    const carried = takeCarriedGroupNames(account);
    if (carried.size === 0) return;
    try {
      const wasm = await loadWasm();
      for (const [groupIdB64, value] of carried) {
        const entry = chats.get(groupIdB64);
        if (entry !== undefined && conversationKindOf(entry.group) === "chat") continue;
        await withRoutingRowLock(account, groupIdB64, async () => {
          await opened;
          if (accountId() !== account) return;
          const toStore = groupNamesToStore(
            new Map([[groupIdB64, value]]),
            (id) => store.get(id),
            (a, b) => wasm.groupNameIsNewer(a, b),
          );
          const next = toStore.get(groupIdB64);
          if (next !== undefined) await store.set(account, groupIdB64, next);
        });
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[group-name] carried names not stored", err instanceof Error ? err.name : "");
    }
  }
  const unsubscribeCarried = subscribeCarriedNames(() => {
    const account = accountId();
    if (account !== null) void takeCarried(account);
  });
  onCleanup(() => {
    unsubscribeStore();
    unsubscribePhotos();
    setConversationAvatarLookup(null);
    installGroupPublicLookup(null);
    unsubscribeReactions();
    unsubscribeHandles();
    unsubscribeCarried();
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
    async forgetGroup(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, groupIdB64, async () => {
        await opened;
        await settingsOpened;
        await store.forget(account, groupIdB64);
        await photos.forget(account, groupIdB64);
        await reactionSettings.forget(account, groupIdB64);
        await handles.forget(account, groupIdB64);
      });
    },
    photoOf(groupIdB64) {
      version();
      return photos.get(groupIdB64);
    },
    reactionsOf(groupIdB64) {
      version();
      return reactionSettings.get(groupIdB64)?.allowed ?? ALLOW_ALL;
    },
    handleOf(groupIdB64) {
      version();
      return handles.get(groupIdB64);
    },
    async receiveSetting(entry, event) {
      const account = accountId();
      if (account === null) return;
      const setting = event.setting;
      if (setting === undefined || setting.kind === "knockSeed") return;
      const groupIdB64 = entry.groupIdB64;
      const wasm = await loadWasm();
      if (setting.kind === "handle") {
        // Section 11: any member may relay it; the signature by the handle
        // key the certificate names is the gate, and the strictly newer
        // stamp wins.
        if (!(await handleVerifies(wasm, entry.group.groupId, setting.value))) return;
        await withRoutingRowLock(account, groupIdB64, async () => {
          await settingsOpened;
          const held = handles.get(groupIdB64);
          if (held !== undefined && !wasm.conversationHandleIsNewer(setting.value.updatedAtMs, held.updatedAtMs)) {
            return;
          }
          await handles.set(account, groupIdB64, setting.value);
        });
        return;
      }
      await withRoutingRowLock(account, groupIdB64, async () => {
        const readable = await settingsOpened;
        const isPhoto = setting.kind === "photo";
        const storedValue = isPhoto ? photos.get(groupIdB64) : reactionSettings.get(groupIdB64);
        const keyOf = (v: StoredGroupPhoto | StoredGroupReactions): string =>
          isPhoto ? photoKey(v as StoredGroupPhoto) : reactionsKey(v as StoredGroupReactions);
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
        try {
          senderKey = event.senderKey === undefined ? undefined : base64Decode(event.senderKey);
        } catch {
          senderKey = undefined;
        }
        // The group name's gate, over a canonical string of the value: owner
        // or admin resolved at receipt, strictly newer stamp, an identical
        // re-send accepted.
        const verdict = groupNameVerdict({
          kind: conversationKindOf(entry.group),
          ownerKey,
          adminKeys,
          senderKey,
          stored:
            storedValue === undefined ? null : { name: keyOf(storedValue), updatedAtMs: storedValue.updatedAtMs },
          storedUnreadable: !readable,
          incoming: { name: keyOf(setting.value), updatedAtMs: setting.value.updatedAtMs },
          verifyFailed,
          isNewer: (a, b) => (isPhoto ? wasm.groupPhotoIsNewer(a, b) : wasm.groupReactionsIsNewer(a, b)),
        });
        if (!groupNameVerdictAccepts(verdict) || verdict === "accept-identical") return;
        if (setting.kind === "photo") {
          await owePublicRecordQuietly(account, entry);
          await photos.set(account, groupIdB64, setting.value);
          await owePublicRecordQuietly(account, entry);
        } else await reactionSettings.set(account, groupIdB64, setting.value);
      });
    },
    async setPhoto(entry, avatar) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await settingsOpened;
        const wasm = await loadWasm();
        const held = photos.get(entry.groupIdB64);
        const now = BigInt(Date.now());
        const stamp = held !== undefined && held.updatedAtMs >= now ? held.updatedAtMs + 1n : now;
        const value: StoredGroupPhoto = { avatar, updatedAtMs: stamp };
        await owePublicRecord(account, entry);
        await send(entry, composePhotoEnvelope(wasm, value));
        await photos.set(account, entry.groupIdB64, value);
        await owePublicRecordQuietly(account, entry);
        // ADR-0087 section 18.5 — the replaced photo is deleted by the
        // devices of whoever set it: durable (owed until the server answers),
        // and a photo another account uploaded answers 403, which ends it.
        const replaced = avatarBlobIdsHex(held?.avatar ?? null).filter(
          (id) => !avatarBlobIdsHex(avatar).includes(id),
        );
        if (replaced.length > 0) void deleteBlobs(account, replaced);
      });
    },
    async setReactions(entry, allowed) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await settingsOpened;
        const wasm = await loadWasm();
        const held = reactionSettings.get(entry.groupIdB64);
        const now = BigInt(Date.now());
        const stamp = held !== undefined && held.updatedAtMs >= now ? held.updatedAtMs + 1n : now;
        const value: StoredGroupReactions = { allowed, updatedAtMs: stamp };
        await send(entry, composeReactionsEnvelope(wasm, value));
        await reactionSettings.set(account, entry.groupIdB64, value);
      });
    },
    async setHandle(entry, value) {
      const account = accountId();
      if (account === null) throw new Error("Sign in again.");
      await withRoutingRowLock(account, entry.groupIdB64, async () => {
        await settingsOpened;
        await handles.set(account, entry.groupIdB64, value);
      });
      try {
        await send(entry, value.envelope);
      } catch (err) {
        // Kept; the growth re-send carries it.
        // eslint-disable-next-line no-console
        console.warn("[group-handle] send failed", err instanceof Error ? err.name : "");
      }
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
          if (verdict !== "accept-identical") {
            await owePublicRecordQuietly(account, entry);
            await store.set(account, groupIdB64, incoming);
            await owePublicRecordQuietly(account, entry);
          }
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
        await owePublicRecord(account, entry);
        await send(entry, envelope);
        await store.set(account, entry.groupIdB64, value);
        await owePublicRecordQuietly(account, entry);
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
    async carryOver(from, to) {
      const account = accountId();
      if (account === null) return;
      await opened;
      const held = store.get(from.groupIdB64);
      if (held === undefined) return;
      await withRoutingRowLock(account, to.groupIdB64, async () => {
        const already = store.get(to.groupIdB64);
        if (already !== undefined && already.updatedAtMs >= held.updatedAtMs) return;
        const { envelope, value } = await compose(held.name, held.updatedAtMs);
        await store.set(account, to.groupIdB64, value);
        try {
          await send(to, envelope);
        } catch (err) {
          // The store holds it; the growth re-send carries it.
          // eslint-disable-next-line no-console
          console.warn("[group-name] carry-over send failed", err);
        }
      });
    },
    groupGrew(groupIdB64) {
      scheduler.groupGrew(groupIdB64);
    },
    addedHere(groupIdB64) {
      void scheduler.joined(groupIdB64);
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

function composePhotoEnvelope(wasm: Awaited<ReturnType<typeof loadWasm>>, v: StoredGroupPhoto): Uint8Array {
  const avatar = v.avatar === null ? undefined : avatarToWasm<WasmProfileAvatar>(wasm, v.avatar);
  const photo = new wasm.GroupPhoto(avatar, v.updatedAtMs);
  try {
    return wasm.composeGroupPhotoEnvelope(photo);
  } finally {
    photo.free();
  }
}

function composeReactionsEnvelope(wasm: Awaited<ReturnType<typeof loadWasm>>, v: StoredGroupReactions): Uint8Array {
  const a =
    v.allowed.mode === "off"
      ? wasm.AllowedReactions.off()
      : v.allowed.mode === "only"
        ? wasm.AllowedReactions.only([...v.allowed.emoji])
        : wasm.AllowedReactions.all();
  const value = new wasm.GroupReactions(a, v.updatedAtMs);
  try {
    return wasm.composeGroupReactionsEnvelope(value);
  } finally {
    value.free();
  }
}

/** Section 11 — a handle envelope counts only when its signature verifies
 *  under the write key its certificate names, and the certificate verifies
 *  under the discovery key for the lookup key of the shown name. */
async function handleVerifies(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  groupId: Uint8Array,
  value: StoredConversationHandle,
): Promise<boolean> {
  try {
    const lookupKey = await handleLookupKey(value.handle);
    if (lookupKey === null) return false;
    const cert = wasm.verifyHandleBinding(value.certificate, await usernameCertKey(), lookupKey);
    const writePk = cert.writePk;
    cert.free();
    const h = wasm.decodeConversationHandleEnvelope(value.envelope);
    try {
      h.verify(groupId, writePk);
    } finally {
      h.free();
    }
    return true;
  } catch {
    return false;
  }
}

/** One lookup key per name, kept for the page: a name is evaluated once. */
const lookupKeyCache = new Map<string, Promise<Uint8Array | null>>();

function handleLookupKey(handle: string): Promise<Uint8Array | null> {
  const held = lookupKeyCache.get(handle);
  if (held !== undefined) return held;
  const run = lookupKeyForName(handle).catch(() => {
    lookupKeyCache.delete(handle);
    return null;
  });
  lookupKeyCache.set(handle, run);
  return run;
}

const HANDLE_CODEC = {
  encode: (v: StoredConversationHandle): unknown => ({
    h: v.handle,
    p: v.isPublic,
    t: v.updatedAtMs.toString(),
    c: bytesToB64(v.certificate),
    e: bytesToB64(v.envelope),
  }),
  decode: (raw: unknown): StoredConversationHandle | null => {
    if (raw === null || typeof raw !== "object") return null;
    const r = raw as { h?: unknown; p?: unknown; t?: unknown; c?: unknown; e?: unknown };
    if (typeof r.h !== "string" || typeof r.p !== "boolean" || typeof r.c !== "string" || typeof r.e !== "string") {
      return null;
    }
    if (typeof r.t !== "string" || !/^\d+$/.test(r.t)) return null;
    try {
      return { handle: r.h, isPublic: r.p, updatedAtMs: BigInt(r.t), certificate: b64ToBytes(r.c), envelope: b64ToBytes(r.e) };
    } catch {
      return null;
    }
  },
};
