// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0059 section 6 — channels people in your groups follow. The session
 * runtime, a sibling of `chat/PinContext.tsx`.
 *
 * Owns, for the signed-in account:
 *
 *   * the RECEIVE side: a member's list is filed under the member's ACCOUNT
 *     key resolved at receipt (C-68), strictly newer stamp wins
 *     (`sharedChannelsIsNewer`), under the same per-`(account, group)`
 *     serialising lock the pins and the group name use (C-62 sub-invariant 5);
 *   * the "Share my channels with this group" switch per group, and the SEND
 *     side: the list is sent when the switch goes on, an empty list when it
 *     goes off, again whenever the set of listed channels this device follows
 *     changes (a subscribe, an unsubscribe, a channel becoming listed or
 *     unlisted), and re-sent VERBATIM with its original stamp when the group
 *     grows (`publish-scheduler.ts`, throttled + jittered), so a newcomer
 *     learns it and no member takes the re-send for a newer list;
 *   * the sealed row both live in (`discover-store.ts`, `shared`).
 *
 * Only channels whose own metadata says `listed` are shared
 * (`channelsToShare`), so a channel reached through a private handle never is.
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
import { getDeviceKey } from "../auth/key-store";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { ChatClient, type IncomingChatEvent } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { channelIdFromHex } from "../channels/composer";
import { ProfilePublishScheduler } from "../profile/publish-scheduler";
import { withRoutingRowLock } from "../push/group-routing";
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { KeyedError } from "../l10n/keyed-error";
import { loadDiscoverRow, saveDiscoverRow } from "./discover-store";
import {
  applyIncomingList,
  channelsToShare,
  decodeSharedChannelsRow,
  emptySharedChannelsRow,
  encodeSharedChannelsRow,
  freshListStamp,
  sameChannels,
  type SharedChannel,
  type SharedChannelsRow,
  type SharedChannelsVerdict,
  type SharedList,
} from "./shared-channels";
import { durableSnapshotHook } from "../chat/persist-hooks";

export type SharedChannelsEvent = Extract<IncomingChatEvent, { kind: "shared-channels" }>;

/** How long a change of the followed set waits before it is sent, so a burst
 *  (several subscriptions in a row) becomes one list (chosen). */
const RESEND_DEBOUNCE_MS = 3_000;

export interface SharedChannelsApi {
  /** Bumped on every change of the row. */
  readonly version: Accessor<number>;
  /** The whole row (received lists, switches, what was sent). Reactive. */
  row(): SharedChannelsRow;
  /** Whether the switch is on in the group. Reactive. */
  isSharing(groupIdB64: string): boolean;
  /** Turn the switch on or off: sends the list (or an empty one), then
   *  stores the switch. Throws the send failure; the switch does not change. */
  setSharing(entry: ActiveGroup, on: boolean): Promise<void>;
  /** File an incoming list. Never throws; `null` when nothing was decided. */
  receive(entry: ActiveGroup, event: SharedChannelsEvent): Promise<SharedChannelsVerdict | null>;
  /** Drop everything held for a group: the lists its members shared, what
   *  was sent there, the switch (ADR-0087 section 9). */
  forgetGroup(groupIdB64: string): Promise<void>;
  /** The group gained a member: schedule the verbatim re-send. */
  groupGrew(groupIdB64: string): void;
}

const SharedChannelsContext = createContext<SharedChannelsApi>();

export function SharedChannelsProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();

  const [row, setRow] = createSignal<SharedChannelsRow>(emptySharedChannelsRow());
  const [version, setVersion] = createSignal(0);
  let current: SharedChannelsRow = emptySharedChannelsRow();
  let opened: Promise<void> = Promise.resolve();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  function install(next: SharedChannelsRow): void {
    current = next;
    setRow(next);
    setVersion((v) => v + 1);
  }

  async function persist(account: string): Promise<void> {
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) return;
    await saveDiscoverRow(account, "shared", deviceKey, encodeSharedChannelsRow(current));
  }

  async function open(account: string): Promise<void> {
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) {
      install(emptySharedChannelsRow());
      return;
    }
    const json = await loadDiscoverRow(account, "shared", deviceKey).catch(() => null);
    if (accountId() !== account) return;
    install(json === null ? emptySharedChannelsRow() : decodeSharedChannelsRow(json));
  }

  // Publishes only the lists. Its echoes are shared with every other
  // ChatClient on the page.
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, accountId),
  });

  async function send(entry: ActiveGroup, list: SharedList): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null || chats.identity() === null) throw new KeyedError("common.error.session_ended");
    if (readIsActive(entry.group) === false) throw new KeyedError("chat.ended.no_longer_member");
    const wasm = await loadWasm();
    const ids = new Uint8Array(list.channels.length * 16);
    list.channels.forEach((c, i) => ids.set(channelIdFromHex(c.channelIdHex), i * 16));
    const envelope = wasm.composeSharedChannelsEnvelope(
      list.updatedAtMs,
      ids,
      list.channels.map((c) => c.handle),
      list.channels.map((c) => c.name),
    );
    await publisher.broadcastControl(secrets, entry.group, envelope);
  }

  /** The list this device would share now. */
  function currentShareable(): SharedChannel[] {
    const subscribed = channelsState.subscriberKeysByChannel();
    const meta = channelsState.metaByChannel();
    return channelsToShare(
      channelsState.channels().map((c) => {
        const m = meta.get(c.id);
        return {
          channelIdHex: c.id,
          subscribed: subscribed.has(c.id),
          listed: m?.listed === true,
          handle: c.localHandle,
          name: m?.name ?? c.localName,
        };
      }),
    );
  }

  /** Send `channels` fresh-stamped into the group and record it as sent. */
  async function sendFresh(account: string, entry: ActiveGroup, channels: SharedChannel[]): Promise<void> {
    await withRoutingRowLock(account, entry.groupIdB64, async () => {
      await opened;
      const previous = current.sent.get(entry.groupIdB64);
      const list: SharedList = { updatedAtMs: freshListStamp(Date.now(), previous), channels };
      await send(entry, list);
      const sent = new Map(current.sent);
      sent.set(entry.groupIdB64, list);
      install({ ...current, sent });
      await persist(account);
    });
  }

  // Growth: re-send what was last sent, with its ORIGINAL stamp.
  async function republishInto(groupIdB64: string): Promise<void> {
    const account = accountId();
    if (account === null || !current.shareOn.has(groupIdB64)) return;
    const entry = chats.get(groupIdB64);
    const sent = current.sent.get(groupIdB64);
    if (entry === undefined || sent === undefined || readIsActive(entry.group) === false) return;
    await send(entry, sent);
  }

  const scheduler = new ProfilePublishScheduler<ReturnType<typeof setTimeout>>({
    now: () => Date.now(),
    setTimer: (fn, ms) => setTimeout(fn, ms),
    clearTimer: (t) => clearTimeout(t),
    random: () => Math.random(),
    publish: republishInto,
    onError: (_groupIdB64, err) => {
      // eslint-disable-next-line no-console
      console.warn("[shared-channels] growth re-send failed", err);
    },
  });

  let boundAccount: string | null = null;
  createEffect(() => {
    const account = accountId();
    untrack(() => {
      if (account === boundAccount) return;
      boundAccount = account;
      if (account === null) {
        scheduler.stop();
        install(emptySharedChannelsRow());
        opened = Promise.resolve();
        return;
      }
      scheduler.start();
      opened = open(account).catch(() => install(emptySharedChannelsRow()));
    });
  });

  // A change of the followed listed set: re-send to every group the switch is
  // on in whose last list differs. Debounced; compared against what was
  // sent, so a reload that changes nothing sends nothing.
  let resendTimer: ReturnType<typeof setTimeout> | null = null;
  createEffect(() => {
    // Tracks the channel list, subscriptions and metadata, and the row.
    currentShareable();
    row();
    if (resendTimer !== null) clearTimeout(resendTimer);
    resendTimer = setTimeout(() => {
      resendTimer = null;
      void resendChanged();
    }, RESEND_DEBOUNCE_MS);
  });
  async function resendChanged(): Promise<void> {
    const account = accountId();
    if (account === null || !channelsState.hydrated()) return;
    await opened;
    const channels = currentShareable();
    for (const groupIdB64 of current.shareOn) {
      const sent = current.sent.get(groupIdB64);
      if (sent !== undefined && sameChannels(sent.channels, channels)) continue;
      const entry = chats.get(groupIdB64);
      if (entry === undefined || readIsActive(entry.group) === false) continue;
      try {
        await sendFresh(account, entry, channels);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[shared-channels] re-send failed", err);
      }
    }
  }
  onCleanup(() => {
    scheduler.stop();
    if (resendTimer !== null) clearTimeout(resendTimer);
  });

  const api: SharedChannelsApi = {
    version,
    row: () => row(),
    isSharing(groupIdB64) {
      return row().shareOn.has(groupIdB64);
    },
    async setSharing(entry, on) {
      const account = accountId();
      if (account === null) throw new KeyedError("common.error.session_ended");
      await opened;
      await sendFresh(account, entry, on ? currentShareable() : []);
      const shareOn = new Set(current.shareOn);
      if (on) shareOn.add(entry.groupIdB64);
      else shareOn.delete(entry.groupIdB64);
      install({ ...current, shareOn });
      await persist(account);
    },
    async receive(entry, event) {
      const account = accountId();
      if (account === null) return null;
      const list = event.list;
      if (list === undefined) return null;
      try {
        return await withRoutingRowLock(account, entry.groupIdB64, async () => {
          await opened;
          const wasm = await loadWasm();
          const { row: next, verdict } = applyIncomingList(
            current,
            entry.groupIdB64,
            event.senderKey,
            list,
            (a, b) => wasm.sharedChannelsIsNewer(a, b),
          );
          if (verdict === "accept") {
            install(next);
            await persist(account);
          }
          return verdict;
        });
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("shared channels receive failed", err);
        return null;
      }
    },
    async forgetGroup(groupIdB64) {
      const account = accountId();
      if (account === null) return;
      await withRoutingRowLock(account, groupIdB64, async () => {
        await opened;
        if (
          !current.received.has(groupIdB64) &&
          !current.sent.has(groupIdB64) &&
          !current.shareOn.has(groupIdB64)
        ) {
          return;
        }
        const received = new Map(current.received);
        received.delete(groupIdB64);
        const sent = new Map(current.sent);
        sent.delete(groupIdB64);
        const shareOn = new Set(current.shareOn);
        shareOn.delete(groupIdB64);
        install({ ...current, received, sent, shareOn });
        await persist(account);
      });
    },
    groupGrew(groupIdB64) {
      if (current.shareOn.has(groupIdB64)) scheduler.groupGrew(groupIdB64);
    },
  };

  return <SharedChannelsContext.Provider value={api}>{props.children}</SharedChannelsContext.Provider>;
}

export function useSharedChannels(): SharedChannelsApi {
  const ctx = useContext(SharedChannelsContext);
  if (ctx === undefined) throw new Error("useSharedChannels() called outside <SharedChannelsProvider>");
  return ctx;
}
