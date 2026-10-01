// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The merged conversation list's rows, built from the stores
 * (`docs/ux/navigation.md` section 2).
 *
 * Lifted out of `ChatList.tsx` so the chat-folder editor and its chat picker
 * (`docs/ux/chat-folders.md` 3.3) show the SAME rows - the same names, the
 * same unread state - without a second copy of how a row is named. Must be
 * called inside a component (it reads contexts and owns effects).
 */
import { createMemo, createSignal, onCleanup, onMount } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { ownPersonKeyB64, peopleCountOf, personKeysOf } from "./person-key";
import { t } from "../l10n";
import { conversationKindOf, type ConversationKind } from "./conversation-kind";
import { conversationTitle } from "./conversation-title";
import { channelRowModel } from "../channels/channel-row";
import { channelsState } from "../state/channels-state";
import { createChannelUnread } from "../feed/channel-unread";
import { sortConversations, type ConversationRow } from "./conversation-list";
import { uiRules } from "./ui-rules";
import { groupTitle } from "./group-title";
import { previewLine } from "./list-preview";
import { memberNameOf } from "../profile/member-label";
import { useChats, type ActiveGroup } from "./ChatContext";
import { chatListLine, membershipState, readIsActive } from "./membership-ended";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { loadMessageLog, type PersistedGroup } from "./message-log-store";
import { newestMessageId, summarizeGroupSlot, type ChatListSummary } from "./background-log";
import { subscribeMessageLogChanged } from "./message-log-events";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
  subscribeAcceptedChanged,
  type AcceptedContact,
} from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { useGroupNames } from "./GroupNameContext";

// eslint-disable-next-line @typescript-eslint/explicit-module-boundary-types
export function createConversationRows() {
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  const auth = useAuth();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // Accepted contacts name a chat linked to one; a rename propagates here.
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    onCleanup(unsubscribe);
  });

  // docs/ux section 3 — the kind the conversation carries (ADR-0062). It is
  // fixed at creation, so reading it from the group is all there is.
  const kindOf = (g: ActiveGroup): ConversationKind => conversationKindOf(g.group);

  /** The other member's key in a 1x1 chat, else `null`. */
  const peerKeyOf = (g: ActiveGroup): string | null => {
    const ownB64 = ownPersonKeyB64(chats.identity());
    const memberB64s = personKeysOf(g.group);
    if (memberB64s.length === 0) return null;
    const peers = memberB64s.filter((k) => k !== ownB64);
    return memberB64s.length === 2 && peers.length === 1 ? (peers[0] ?? null) : null;
  };

  /** Best label for a row (ADR-0048 section 5). A 1x1 chat is the other
   *  person, through the one resolver, read live from the roster so it works
   *  however the chat was created. A group keeps its own label. */
  const chatLabel = (g: ActiveGroup): string => {
    accepted();
    const ownB64 = ownPersonKeyB64(chats.identity());
    // ADR-0050 — people, not leaves.
    const memberB64s = personKeysOf(g.group);
    return conversationTitle({
      kind: kindOf(g),
      memberKeysB64: memberB64s,
      ownKeyB64: ownB64,
      fallbackLabel: g.label,
      sharedName: groupNames.nameOf(g.groupIdB64),
      contactNameById: accepted().find((c) => c.groupIdB64 === g.groupIdB64)?.displayName,
      contactNameByMember: peerContactByMemberPubkeys(memberB64s, ownB64, accepted())
        ?.displayName,
      nameOf: (k) => profiles.resolve(k).name,
      groupTitle,
    });
  };

  // Unread badge + last-message preview + time, from the persisted message
  // log. Reloads live on every log write (another tab, the open chat, the
  // background receive), filtered to this account.
  const [logSlots, setLogSlots] = createSignal<{ readonly [groupIdHex: string]: PersistedGroup }>(
    {},
  );

  async function reloadLog(): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session") return;
    const accountIdB64 = s.account.accountId;
    const deviceKey = await getDeviceKey(accountIdB64);
    if (deviceKey === null) return;
    const log = await loadMessageLog(accountIdB64, deviceKey);
    if (log === null) return;
    setLogSlots(log.groups);
  }

  onMount(() => {
    void reloadLog();
    const unsubscribe = subscribeMessageLogChanged((account) => {
      const s = auth.state();
      if (s.kind === "session" && s.account.accountId === account) {
        void reloadLog();
      }
    });
    onCleanup(unsubscribe);
  });

  // The row time relabels itself as the day turns ("14:05" becomes "Tue").
  const [clock, setClock] = createSignal(new Date());
  onMount(() => {
    const t = setInterval(() => setClock(new Date()), 60_000);
    onCleanup(() => clearInterval(t));
  });

  /** Unread count + preview for one row: rows above the later of the
   *  cross-device read cursor and the newest row this device has shown. */
  function summaryFor(groupIdBytes: Uint8Array): ChatListSummary {
    const hex = syncGroupIdHex(groupIdBytes);
    const cursor = sync.readCursors()[hex];
    let syncedReadUpTo: bigint | null = null;
    if (cursor !== undefined && cursor.upToMessageId.length >= 8) {
      let id = 0n;
      for (let i = 0; i < 8; i += 1) {
        id = (id << 8n) | BigInt(cursor.upToMessageId[i] as number);
      }
      syncedReadUpTo = id;
    }
    return summarizeGroupSlot(logSlots()[hex], syncedReadUpTo);
  }

  /** The last message's time as a sort key: the DS stamp, else the moment
   *  the conversation was created on this device. */
  const lastActivityMs = (g: ActiveGroup): number => {
    const at = summaryFor(g.group.groupId).lastAt;
    if (at !== null) {
      const t = new Date(at).getTime();
      if (!Number.isNaN(t)) return t;
    }
    return g.createdAtMs;
  };

  /** ADR-0045 UX section 6.2.4: the row's one muted line once the membership
   *  is over, or `null` while the reader is still a member. The chat stays in
   *  the list; only the badge goes quiet. */
  const endedLine = (g: ActiveGroup): string | null =>
    chatListLine(
      membershipState({
        isActive: readIsActive(g.group),
        leaveRequestedAtMs: undefined,
      }),
    );

  /** The second line: the ended notice, else the last message with its
   *  prefix, else the person's username (1x1) or the group's size. */
  const subOf = (g: ActiveGroup): string => {
    const ended = endedLine(g);
    if (ended !== null) return ended;
    const s = summaryFor(g.group.groupId);
    const line = previewLine({
      preview: s.preview,
      isOwn: s.previewIsOwn,
      senderName:
        s.lastAuthorKeyB64 === null ? null : memberNameOf(profiles.resolve(s.lastAuthorKeyB64)),
      isGroup: kindOf(g) === "group",
    });
    if (line.length > 0) return line;
    // Nothing said yet: a 1x1 shows the person's username, a group its size.
    // A member count on a 1x1 says nothing.
    const peer = kindOf(g) === "chat" ? peerKeyOf(g) : null;
    if (peer !== null) return profiles.resolve(peer).username ?? "";
    return t("chats.row.people", { count: peopleCountOf(g.group) });
  };

  /**
   * Every conversation, ordered.
   *
   * The rule lives in `conversation-list.ts` rather than here, because it is
   * the rule all three clients need and because a comparator written inside
   * a component cannot be run by a test. This maps the store into the shape
   * that module takes; when channels join the list they are mapped the same
   * way and nothing about the ordering changes.
   */
  /** A subscribed channel, in the shape the ordering takes. Its title,
   *  second line and time come from `channelRowModel`, which the Channels
   *  screen already uses - one vocabulary, not a second one. */
  // docs/ux/feed.md section 5 — each channel row's count of new posts.
  const channelUnread = createChannelUnread();
  const channelRows = (): readonly ConversationRow[] => {
    const meta = channelsState.metaByChannel();
    const posts = channelsState.postsByChannel();
    return channelsState.channels().map((c) => {
      const lastPostMs = posts.get(c.id)?.at(-1)?.serverTsMs ?? null;
      const model = channelRowModel(
        {
          id: c.id,
          sharedName: meta.get(c.id)?.name,
          localName: c.localName,
          localHandle: c.localHandle,
          isOwner: false,
          subscriberCount: null,
          lastPostMs,
        },
        clock(),
      );
      return {
        id: c.id,
        kind: "channel" as const,
        title: model.title,
        preview: model.subtitle,
        // Nothing posted yet sorts by when it was added, the same way a chat
        // with no messages sorts by when it was created.
        lastActivityMs: lastPostMs ?? c.addedAtMs,
        unread: channelUnread()(c.id),
        pinned: false,
        muted: false,
        archived: false,
      };
    });
  };

  const orderedRows = createMemo((): readonly ConversationRow[] => {
    const flagsMap = sync.flags();
    // The order is the shared rule, so the list waits for it rather than
    // falling back to a second, drifting copy. In the running app the rules
    // have resolved long before a session exists.
    const rules = uiRules();
    if (rules === null) return [];
    return sortConversations(
      [
      ...channelRows(),
      ...chats.groups().map((g) => {
        const f = flagsMap[syncGroupIdHex(g.group.groupId)];
        return {
          id: g.groupIdB64,
          kind: kindOf(g),
          title: chatLabel(g),
          preview: subOf(g),
          lastActivityMs: lastActivityMs(g),
          unread: summaryFor(g.group.groupId).unread,
          // The badge's meaning: something unread, or marked unread by hand.
          hasUnread:
            summaryFor(g.group.groupId).unread > 0 || f?.unreadOverride === true,
          pinned: f?.pinned === true,
          muted: f?.muted === true,
          archived: f?.archived === true,
        };
      }),
      ],
      rules,
    );
  });

  /** The store entry behind a chat or group row. */
  const groupById = createMemo(
    (): ReadonlyMap<string, ActiveGroup> =>
      new Map(chats.groups().map((g) => [g.groupIdB64, g])),
  );

  /** The newest message id this device holds for a group, for Mark all as
   *  read. */
  const newestIdFor = (groupIdBytes: Uint8Array): bigint | null =>
    newestMessageId(logSlots()[syncGroupIdHex(groupIdBytes)]);

  return {
    newestIdFor,
    accountId,
    accepted,
    kindOf,
    chatLabel,
    summaryFor,
    endedLine,
    subOf,
    clock,
    orderedRows,
    groupById,
  };
}

export type ConversationRows = ReturnType<typeof createConversationRows>;
