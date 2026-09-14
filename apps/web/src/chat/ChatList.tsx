// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat list: one row per joined group, the newest message as a preview,
 * an unread badge, and pin / mute / archive on hover. Moved here from the
 * Contacts route, where it lived as "Active MLS groups" while there was no
 * home to put it on; the logic is unchanged.
 *
 * Labels come from the one profile resolver (ADR-0048 §5); flags from the
 * cross-device sync runtime; unread + preview from the persisted message log,
 * which the open chat and the background receive both write.
 */
import {
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { useChats, type ActiveGroup } from "./ChatContext";
import { chatListLine, membershipState, readIsActive } from "./membership-ended";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { loadMessageLog, type PersistedGroup } from "./message-log-store";
import { summarizeGroupSlot, type ChatListSummary } from "./background-log";
import { subscribeMessageLogChanged } from "./message-log-events";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
  subscribeAcceptedChanged,
  type AcceptedContact,
} from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";

export function ChatList(props: { activeGroupIdB64: string | null }): JSX.Element {
  const chats = useChats();
  const profiles = useProfiles();
  const sync = useDeviceGroupSync();
  const auth = useAuth();
  const navigate = useNavigate();

  // Accepted contacts name a chat linked to one; a rename propagates here.
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    onCleanup(unsubscribe);
  });

  /** Best label for a chat row (ADR-0048 §5). A 1x1 chat is the other person,
   *  through the one resolver, read live from the MLS roster so it works
   *  however the chat was created. A group keeps its own label. */
  const chatLabel = (g: ActiveGroup): string => {
    accepted();
    const own = chats.identity();
    const ownB64 = own === null ? null : base64Encode(own.signaturePublicKey);
    let memberB64s: string[] = [];
    try {
      memberB64s = (g.group.members() as Array<{ signaturePublicKey: Uint8Array }>).map((m) =>
        base64Encode(m.signaturePublicKey),
      );
    } catch {
      // members() can throw on a torn group; fall through to the label.
    }
    const peers = memberB64s.filter((k) => k !== ownB64);
    if (memberB64s.length === 2 && peers.length === 1 && peers[0] !== undefined) {
      return profiles.resolve(peers[0]).listLabel;
    }
    const byId = accepted().find((c) => c.groupIdB64 === g.groupIdB64);
    if (byId !== undefined && byId.displayName.length > 0) return byId.displayName;
    const byMember = peerContactByMemberPubkeys(memberB64s, ownB64, accepted());
    if (byMember !== undefined && byMember.displayName.length > 0 && memberB64s.length <= 2) {
      return byMember.displayName;
    }
    return g.label;
  };

  // Unread badge + last-message preview, from the persisted message log.
  // Reloads live on every log write (another tab, the open chat, the
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

  /** Pinned first, archived last, otherwise creation order. */
  const sortedGroups = (): readonly ActiveGroup[] => {
    const flagsMap = sync.flags();
    const items = [...chats.groups()];
    items.sort((a, b) => {
      const fa = flagsMap[syncGroupIdHex(a.group.groupId)];
      const fb = flagsMap[syncGroupIdHex(b.group.groupId)];
      const aPinned = fa?.pinned === true;
      const bPinned = fb?.pinned === true;
      if (aPinned !== bPinned) return aPinned ? -1 : 1;
      const aArchived = fa?.archived === true;
      const bArchived = fb?.archived === true;
      if (aArchived !== bArchived) return aArchived ? 1 : -1;
      return 0;
    });
    return items;
  };

  /** ADR-0045 UX §6.2.4: the row's one muted line once the membership is
   *  over, or `null` while the reader is still a member. The chat stays in
   *  the list; only the badge goes quiet. */
  const endedLine = (g: ActiveGroup): string | null =>
    chatListLine(
      membershipState({
        isActive: readIsActive(g.group),
        leaveRequestedAtMs: undefined,
      }),
    );

  async function toggleFlag(
    g: ActiveGroup,
    field: "pinned" | "muted" | "archived",
  ): Promise<void> {
    const groupId = g.group.groupId;
    const key = syncGroupIdHex(groupId);
    const cur = sync.flags()[key];
    const next = {
      pinned: cur?.pinned ?? false,
      muted: cur?.muted ?? false,
      archived: cur?.archived ?? false,
      unreadOverride: cur?.unreadOverride ?? false,
    };
    next[field] = !next[field];
    try {
      await sync.setFlags(groupId, next);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`setFlags(${field}) failed`, err);
    }
  }

  return (
    <aside class="lk-pane" aria-label="Chats">
      <div class="lk-pane-head">
        <h1>Chats</h1>
        <button
          type="button"
          class="lk-icon-button accent"
          title="New chat"
          aria-label="New chat"
          onClick={() => navigate(paths.contacts)}
        >
          <Icon name="compose" size={18} />
        </button>
      </div>
      <Show
        when={chats.groups().length > 0}
        fallback={
          <div class="lk-empty">
            <Icon name="chat" size={40} strokeWidth={1.4} />
            <strong>No conversations yet</strong>
            <span>Start a chat with a contact, or share your invite link.</span>
            <button type="button" class="primary" onClick={() => navigate(paths.contacts)}>
              New chat
            </button>
          </div>
        }
      >
        <div class="lk-pane-body">
          <For each={sortedGroups()}>
            {(g) => {
              const key = syncGroupIdHex(g.group.groupId);
              const href = paths.chat(base64UrlEncodeNoPad(g.group.groupId));
              const flags = (): { pinned: boolean; muted: boolean; archived: boolean } => {
                const f = sync.flags()[key];
                return {
                  pinned: f?.pinned === true,
                  muted: f?.muted === true,
                  archived: f?.archived === true,
                };
              };
              const summary = (): ChatListSummary => summaryFor(g.group.groupId);
              const unread = (): number => (endedLine(g) === null ? summary().unread : 0);
              const label = (): string => chatLabel(g);
              const sub = (): string => {
                const ended = endedLine(g);
                if (ended !== null) return ended;
                const s = summary();
                if (s.preview) return `${s.previewIsOwn ? "You: " : ""}${s.preview}`;
                const n = g.group.memberCount;
                return `${n} member${n === 1 ? "" : "s"}`;
              };
              return (
                <A
                  href={href}
                  class="lk-row"
                  classList={{
                    selected: props.activeGroupIdB64 === g.groupIdB64,
                    ended: endedLine(g) !== null,
                  }}
                  activeClass=""
                  inactiveClass=""
                  style={{ opacity: flags().archived ? "0.6" : undefined }}
                >
                  <Avatar label={label()} id={g.groupIdB64} size="md" />
                  <div class="lk-row-body">
                    <div class="lk-row-line">
                      <span class="lk-row-title">
                        <Show when={flags().pinned}>
                          <Icon name="pin" size={14} title="Pinned" />
                        </Show>
                        <span style={{ overflow: "hidden", "text-overflow": "ellipsis" }}>
                          {label()}
                        </span>
                        <Show when={flags().muted}>
                          <Icon name="bellOff" size={14} title="Muted" />
                        </Show>
                      </span>
                      <Show when={unread() > 0}>
                        <span
                          class="lk-badge"
                          classList={{ muted: flags().muted }}
                          title={`${unread()} unread`}
                        >
                          {unread() >= 100 ? "99+" : unread()}
                        </span>
                      </Show>
                    </div>
                    <div class="lk-row-line">
                      <span class="lk-row-sub" data-testid="chat-preview">
                        {sub()}
                      </span>
                      <span class="lk-row-actions">
                        <button
                          type="button"
                          title={flags().pinned ? "Unpin" : "Pin"}
                          aria-pressed={flags().pinned}
                          onClick={(e) => {
                            e.preventDefault();
                            void toggleFlag(g, "pinned");
                          }}
                        >
                          <Icon name="pin" size={16} />
                        </button>
                        <button
                          type="button"
                          title={flags().muted ? "Unmute" : "Mute"}
                          aria-pressed={flags().muted}
                          onClick={(e) => {
                            e.preventDefault();
                            void toggleFlag(g, "muted");
                          }}
                        >
                          <Icon name="bellOff" size={16} />
                        </button>
                        <button
                          type="button"
                          title={flags().archived ? "Unarchive" : "Archive"}
                          aria-pressed={flags().archived}
                          onClick={(e) => {
                            e.preventDefault();
                            void toggleFlag(g, "archived");
                          }}
                        >
                          <Icon name="archive" size={16} />
                        </button>
                      </span>
                    </div>
                  </div>
                </A>
              );
            }}
          </For>
        </div>
      </Show>
    </aside>
  );
}
