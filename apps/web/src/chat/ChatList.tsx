// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The conversation list, used twice (docs/ux/client-redesign.md section 5.1):
 * once as Chats (two people) and once as Groups (everyone else), split by the
 * persisted conversation kind (`chat/conversation-kind-store.ts`) over the one
 * store. One row per conversation: avatar, name (bold while unread), the time
 * of the last message, its preview with a sender prefix in a group, then a
 * muted icon, a pin icon and the unread badge. Pinned rows first, then by the
 * last message time. Pin / mute / archive as hover actions, a right-click
 * menu, and the same menu on a long press. A search field filters by name and
 * preview.
 *
 * Labels come from the one profile resolver (ADR-0048 section 5); flags from
 * the cross-device sync runtime; unread, preview and time from the persisted
 * message log, which the open chat and the background receive both write.
 */
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { restoredKind } from "../auth/restore-marker";
import { ownPersonKeyB64, peopleCountOf, personKeysOf } from "./person-key";
import { decideConversationKind, type ConversationKind } from "./conversation-kind";
import { conversationTitle } from "./conversation-title";
import { sortConversations, type ConversationRow } from "./conversation-list";
import {
  observeConversationKind,
  persistedConversationKind,
  subscribeConversationKindChanged,
} from "./conversation-kind-store";
import { groupTitle } from "./group-title";
import { formatListTime } from "./list-time";
import { previewLine, rowMatchesQuery } from "./list-preview";
import { memberNameOf } from "../profile/member-label";
import { useChats, type ActiveGroup } from "./ChatContext";
import { chatListLine, membershipState, readIsActive } from "./membership-ended";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { loadMessageLog, type PersistedGroup } from "./message-log-store";
import { summarizeGroupSlot, type ChatListSummary } from "./background-log";
import { subscribeMessageLogChanged } from "./message-log-events";
import { pendingKnownGroups } from "../devices/state-bundle";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
  subscribeAcceptedChanged,
  type AcceptedContact,
} from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { useGroupNames } from "./GroupNameContext";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";

type Flag = "pinned" | "muted" | "archived";

const COPY: {
  readonly chat: {
    title: string;
    compose: string;
    emptyTitle: string;
    emptyAction: string;
    aria: string;
  };
} = {
  chat: {
    title: "Chats",
    compose: "New",
    emptyTitle: "No conversations yet",
    emptyAction: "Start one",
    aria: "Chats",
  },
};

/**
 * Every conversation in one list: one-to-one chats, groups and - once they
 * are mapped in - channels.
 *
 * `docs/ux/navigation.md` section 2. It used to be one pane per container,
 * rendered twice with a `kind` prop, and the newest thing that happened to
 * you was in one of them and you had to remember which. The prop is gone
 * rather than defaulted: a list sorted by WHEN cannot be filtered by kind
 * and still be that list.
 */
/**
 * The three ways to start something, `docs/ux/navigation.md` section 3.
 *
 * A row with an icon and a label and nothing else: an item that needs a
 * sentence to explain it has the wrong label. Reached from the compose
 * button and from the empty state, so a person never meets two different
 * sets of choices for the same intent.
 */
function CreateMenu(props: { onClose: () => void }): JSX.Element {
  const navigate = useNavigate();
  const go = (href: string): void => {
    props.onClose();
    navigate(href);
  };
  return (
    <>
      {/* Clicking anywhere else closes it, which is what makes the menu
          dismissable without a keyboard. */}
      <div class="lk-menu-backdrop" onClick={() => props.onClose()} />
      <div class="lk-menu" role="menu" aria-label="Create">
        <button type="button" role="menuitem" onClick={() => go(paths.contacts)}>
          <Icon name="compose" size={16} />
          New chat
        </button>
        <button type="button" role="menuitem" onClick={() => go(paths.newGroup)}>
          <Icon name="groups" size={16} />
          Create group
        </button>
        <button type="button" role="menuitem" onClick={() => go(paths.channelNew)}>
          <Icon name="channels" size={16} />
          Create channel
        </button>
      </div>
    </>
  );
}

export function ChatList(props: { activeGroupIdB64: string | null }): JSX.Element {
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  const auth = useAuth();
  const copy = (): (typeof COPY)["chat"] => COPY.chat;

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

  // docs/ux section 3 — the persisted kind. The effect records every
  // conversation's kind at first sight (a Welcome, a restore) and the one
  // allowed flip, chat -> group; the memo below reads what is stored.
  const [kindVersion, setKindVersion] = createSignal(0);
  onMount(() => {
    const unsubscribe = subscribeConversationKindChanged(() => setKindVersion((v) => v + 1));
    onCleanup(unsubscribe);
  });
  createEffect(() => {
    const account = accountId();
    if (account === null) return;
    for (const g of chats.groups()) {
      observeConversationKind(account, g.groupIdB64, peopleCountOf(g.group));
    }
  });
  const kindOf = (g: ActiveGroup): ConversationKind => {
    kindVersion();
    const account = accountId();
    const persisted = account === null ? null : persistedConversationKind(account, g.groupIdB64);
    return decideConversationKind(persisted, peopleCountOf(g.group));
  };

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
    const n = peopleCountOf(g.group);
    return `${n} ${n === 1 ? "person" : "people"}`;
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
  const orderedRows = createMemo((): readonly ConversationRow[] => {
    const flagsMap = sync.flags();
    return sortConversations(
      chats.groups().map((g) => {
        const f = flagsMap[syncGroupIdHex(g.group.groupId)];
        return {
          id: g.groupIdB64,
          kind: kindOf(g),
          title: chatLabel(g),
          preview: subOf(g),
          lastActivityMs: lastActivityMs(g),
          unread: summaryFor(g.group.groupId).unread,
          pinned: f?.pinned === true,
          muted: f?.muted === true,
          archived: f?.archived === true,
        };
      }),
    );
  });

  const sortedGroups = createMemo((): readonly ActiveGroup[] => {
    const byId = new Map(chats.groups().map((g) => [g.groupIdB64, g]));
    return orderedRows().flatMap((row) => {
      const g = byId.get(row.id);
      return g === undefined ? [] : [g];
    });
  });

  // The search field in the pane head filters by name and preview.
  const [query, setQuery] = createSignal("");
  const visibleGroups = (): readonly ActiveGroup[] => {
    const q = query();
    if (q.trim().length === 0) return sortedGroups();
    return sortedGroups().filter((g) => rowMatchesQuery(q, chatLabel(g), subOf(g)));
  };

  /** ADR-0050 — a device that landed through a recovery route (three words,
   *  the phrase, another device, a passkey) holds the identity and is admitted
   *  to the account's chats by its other devices or its contacts. Until the
   *  first chat arrives there is exactly one thing to say. */
  const synchronizing = (): boolean => {
    const joined = chats.groups().map((g) => g.groupIdB64);
    // Groups the pairing bundle named that this device has not joined yet
    // (ADR-0050 section 2.9); the stored list clears itself once all are in.
    if (pendingKnownGroups(joined) > 0) return true;
    return restoredKind() !== null && joined.length === 0;
  };

  async function toggleFlag(g: ActiveGroup, field: Flag): Promise<void> {
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

  // The context menu: one open at a time, anchored where the pointer was.
  // Opened by a right click or by a long press (the touch path).
  const [menu, setMenu] = createSignal<{ g: ActiveGroup; x: number; y: number } | null>(null);

  /** The create menu of `docs/ux/navigation.md` section 3: three items, no
   *  descriptions. Opened by the compose button and by the empty state
   *  alike, so there is one set of choices rather than two. */
  const [createOpen, setCreateOpen] = createSignal(false);

  /** Routed by the ROW's own kind, not the pane's: one list holds both, so
   *  there is no pane kind left to ask. */
  const hrefOf = (g: ActiveGroup): string => {
    const slug = base64UrlEncodeNoPad(g.group.groupId);
    return kindOf(g) === "chat" ? paths.chat(slug) : paths.groupChat(slug);
  };

  return (
    <aside class="lk-pane" aria-label={copy().aria}>
      <div class="lk-pane-head">
        <h1>{copy().title}</h1>
        {/* ADR-0050 / docs/ux/second-device.md section 2: one label while a
            device that landed through a recovery route is still being admitted
            to the account's chats. Chats appear as they arrive; the label goes
            when the first one does. Never a counter, never a per-chat row. */}
        <Show when={synchronizing()}>
          <span class="lk-pane-status" role="status">
            Synchronizing...
          </span>
        </Show>
        <div class="lk-menu-anchor">
          <button
            type="button"
            class="lk-icon-button accent"
            title={copy().compose}
            aria-label={copy().compose}
            aria-haspopup="menu"
            aria-expanded={createOpen()}
            onClick={() => setCreateOpen((open) => !open)}
          >
            <Icon name="compose" size={18} />
          </button>
          <Show when={createOpen()}>
            <CreateMenu onClose={() => setCreateOpen(false)} />
          </Show>
        </div>
      </div>
      <Show when={sortedGroups().length > 0}>
        <label class="lk-search lk-pane-search">
          <Icon name="search" size={16} />
          <input
            type="search"
            placeholder="Search"
            aria-label={`Search ${copy().title.toLowerCase()}`}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </label>
      </Show>
      <Show
        when={sortedGroups().length > 0}
        fallback={
          <div class="lk-empty">
            <Icon name="chat" size={40} strokeWidth={1.4} />
            <strong>{copy().emptyTitle}</strong>
            {/* The SAME menu the compose button opens, not a different set
                of choices in a different place. */}
            <button
              type="button"
              class="primary"
              aria-haspopup="menu"
              aria-expanded={createOpen()}
              onClick={() => setCreateOpen((open) => !open)}
            >
              {copy().emptyAction}
            </button>
          </div>
        }
      >
        <div class="lk-pane-body">
          <Show when={visibleGroups().length === 0}>
            <p class="lk-pane-nomatch">No results</p>
          </Show>
          <For each={visibleGroups()}>
            {(g) => {
              const key = syncGroupIdHex(g.group.groupId);
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
              const sub = (): string => subOf(g);
              const time = (): string => formatListTime(summary().lastAt, clock());
              const press = createLongPress((x, y) => setMenu({ g, x, y }));
              return (
                <A
                  href={hrefOf(g)}
                  class="lk-row"
                  classList={{
                    selected: props.activeGroupIdB64 === g.groupIdB64,
                    ended: endedLine(g) !== null,
                    archived: flags().archived,
                  }}
                  activeClass=""
                  inactiveClass=""
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setMenu({ g, x: e.clientX, y: e.clientY });
                  }}
                  onPointerDown={press.onPointerDown}
                  onPointerMove={press.onPointerMove}
                  onPointerUp={press.onPointerUp}
                  onPointerCancel={press.onPointerCancel}
                  onPointerLeave={press.onPointerLeave}
                  onClick={(e) => {
                    press.swallowClick(e);
                  }}
                >
                  <Avatar label={label()} id={g.groupIdB64} />
                  <div class="lk-row-body">
                    <div class="lk-row-line">
                      <span class="lk-row-title" classList={{ unread: unread() > 0 }}>
                        <span class="lk-ellipsis">{label()}</span>
                      </span>
                      <Show when={time().length > 0}>
                        <span class="lk-row-time" classList={{ unread: unread() > 0 }}>
                          {time()}
                        </span>
                      </Show>
                    </div>
                    <div class="lk-row-line">
                      <span class="lk-row-sub" data-testid="chat-preview">
                        {sub()}
                      </span>
                      <span class="lk-row-trail">
                        <Show when={flags().muted}>
                          <Icon name="bellOff" size={14} title="Muted" />
                        </Show>
                        <Show when={flags().pinned}>
                          <Icon name="pin" size={14} title="Pinned" />
                        </Show>
                        <Show when={unread() > 0}>
                          <span
                            class="lk-badge"
                            classList={{ muted: flags().muted }}
                            title={`${unread()} unread`}
                          >
                            {unread() >= 100 ? "99+" : unread()}
                          </span>
                        </Show>
                      </span>
                      <span class="lk-row-actions" role="group" aria-label="Conversation actions">
                        <button
                          type="button"
                          title={flags().pinned ? "Unpin" : "Pin"}
                          aria-label={flags().pinned ? "Unpin" : "Pin"}
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
                          aria-label={flags().muted ? "Unmute" : "Mute"}
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
                          aria-label={flags().archived ? "Unarchive" : "Archive"}
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
      <Show when={menu()}>
        {(m) => {
          const f = (): { pinned: boolean; muted: boolean; archived: boolean } => {
            const cur = sync.flags()[syncGroupIdHex(m().g.group.groupId)];
            return {
              pinned: cur?.pinned === true,
              muted: cur?.muted === true,
              archived: cur?.archived === true,
            };
          };
          const act = (field: Flag): void => {
            const g = m().g;
            setMenu(null);
            void toggleFlag(g, field);
          };
          return (
            <>
              <div
                class="lk-menu-backdrop"
                onClick={() => setMenu(null)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenu(null);
                }}
              />
              <div
                class="lk-menu lk-menu-floating"
                role="menu"
                aria-label={chatLabel(m().g)}
                style={{ left: `${m().x}px`, top: `${m().y}px` }}
              >
                <button type="button" role="menuitem" onClick={() => act("pinned")}>
                  <Icon name="pin" size={18} />
                  {f().pinned ? "Unpin" : "Pin"}
                </button>
                <button type="button" role="menuitem" onClick={() => act("muted")}>
                  <Icon name="bellOff" size={18} />
                  {f().muted ? "Unmute" : "Mute"}
                </button>
                <button type="button" role="menuitem" onClick={() => act("archived")}>
                  <Icon name="archive" size={18} />
                  {f().archived ? "Unarchive" : "Archive"}
                </button>
              </div>
            </>
          );
        }}
      </Show>
    </aside>
  );
}
