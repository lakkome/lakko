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
  loadKeyAliases,
  peerContactByMemberPubkeys,
  subscribeAcceptedChanged,
  type AcceptedContact,
} from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { useGroupNames } from "./GroupNameContext";
import { useLineage } from "./use-lineage";
import { decodeReadPosition, encodeReadPosition, readUpToInSegment } from "./lineage";
import { base64Encode } from "../auth/account-id";

// eslint-disable-next-line @typescript-eslint/explicit-module-boundary-types
export function createConversationRows() {
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  const auth = useAuth();
  // ADR-0065 R.5 — one row per CONVERSATION: a conversation that continued
  // in a successor group keeps its row, and everything a person attached to
  // it (pin / mute / archive, folder, read position) stays keyed on its id.
  const lineageView = useLineage();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // Accepted contacts name a chat linked to one; a rename propagates here.
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  // ADR-0065 R.2 — a retired key still names the person.
  const [aliases, setAliases] = createSignal<Map<string, string>>(loadKeyAliases());
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => {
      setAccepted(loadAccepted());
      setAliases(loadKeyAliases());
    });
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
      contactNameByMember: peerContactByMemberPubkeys(memberB64s, ownB64, accepted(), aliases())
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
    // The read position is the CONVERSATION's (R.5); what it means for this
    // group's own message ids depends on which segment it is in.
    const cursor = sync.readCursors()[lineageView.conversationHexOf(groupIdBytes)];
    const upTo = readUpToInSegment(
      cursor === undefined ? null : decodeReadPosition(cursor.upToMessageId),
      lineageView.segmentIndexOf(groupIdBytes),
    );
    const syncedReadUpTo =
      upTo === null ? null : upTo === "all" ? BigInt(Number.MAX_SAFE_INTEGER) : BigInt(upTo);
    const summary = summarizeGroupSlot(logSlots()[hex], syncedReadUpTo);
    if (summary.preview !== null || summary.lastAt !== null) return summary;
    // A successor nothing was said in yet: the row keeps the last line and
    // the place of the conversation's earlier part, so it neither goes blank
    // nor jumps in the list when the conversation moves.
    const view = lineageView.resolve(base64Encode(groupIdBytes));
    const previous = view?.history.at(-1);
    if (view === undefined || previous === undefined || view.head.group.groupId !== groupIdBytes) {
      return summary;
    }
    const earlier = summarizeGroupSlot(
      logSlots()[syncGroupIdHex(previous.group.groupId)],
      BigInt(Number.MAX_SAFE_INTEGER),
    );
    return { ...earlier, unread: summary.unread };
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
  const endedLine = (g: ActiveGroup): string | null => {
    const membership = membershipState({
      isActive: readIsActive(g.group),
      leaveRequestedAtMs: undefined,
    });
    return chatListLine(membership);
  };

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
      ...lineageView.conversations().map((c) => {
        const g = c.head;
        const f = flagsMap[c.conversationHex];
        return {
          id: c.conversationIdB64,
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

  /** The store entry behind a chat or group row: the conversation's HEAD
   *  group (the one it opens and sends into), keyed on the row id, which is
   *  the conversation id. */
  const groupById = createMemo(
    (): ReadonlyMap<string, ActiveGroup> =>
      new Map(lineageView.conversations().map((c) => [c.conversationIdB64, c.head])),
  );

  /** The conversation id of a group this device holds, as bytes and base64:
   *  the key of every person-facing store (the synced flags and read
   *  position, a folder ref, the route). */
  const conversationIdOf = (g: ActiveGroup): Uint8Array =>
    lineageView.conversationIdOf(g.group.groupId);
  const conversationIdB64Of = (g: ActiveGroup): string => base64Encode(conversationIdOf(g));
  /** A read position at `messageId` of `g`, as the cursor's bytes. */
  const readPositionBytes = (g: ActiveGroup, messageId: bigint): Uint8Array =>
    encodeReadPosition({
      segment: lineageView.segmentIndexOf(g.group.groupId),
      messageId: Number(messageId),
    });

  /** The newest message id this device holds for a group, for Mark all as
   *  read. */
  const newestIdFor = (groupIdBytes: Uint8Array): bigint | null =>
    newestMessageId(logSlots()[syncGroupIdHex(groupIdBytes)]);

  return {
    conversationIdOf,
    conversationIdB64Of,
    readPositionBytes,
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
