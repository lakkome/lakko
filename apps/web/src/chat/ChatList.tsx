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

import { useCallHost } from "../calls/CallHostContext";
import { quietCallKindLabel, quietCallOfferFor } from "../calls/quiet-call-offer";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { restoredKind } from "../auth/restore-marker";
import { personKeysOf } from "./person-key";
import { PeopleSearch } from "../search/PeopleSearch";
import { rowHasUnread, type ConversationRow } from "./conversation-list";
import { createConversationRows } from "./conversation-rows";
import { uiRules } from "./ui-rules";
import { formatListTime } from "./list-time";
import { rowMatchesQuery } from "./list-preview";
import { useChats, type ActiveGroup } from "./ChatContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import type { ChatListSummary } from "./background-log";
import { pendingKnownGroups } from "../devices/state-bundle";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";
import { FolderBar, type FolderTab } from "./folders/FolderBar";
import { AddToFolderItems } from "./folders/AddToFolderMenu";
import {
  deleteFolder,
  openFolderEditor,
  togglePinInFolder,
  toggleMembership,
} from "./folders/folder-actions";
import {
  MAX_FOLDER_PINS,
  membershipOf,
  refOfRow,
  type FolderModel,
} from "./folders/folder-model";
import {
  folderRowsOrdered,
  folderUnreadCount,
  isPinnedInFolder,
  rowIsInFolder,
} from "./folders/folder-view";
import { ALL_CHATS, loadSelectedTab, resolveTab, selectTab, selectedTab } from "./folders/selected-tab";

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

/**
 * A subscribed channel in the conversation list.
 *
 * Deliberately short: the list says nothing about which kind a row is, so
 * this draws the same avatar, title and second line as every other row. What
 * it does NOT have is the unread badge, the pin / mute glyphs and the row
 * menu - those are keyed on a conversation group id a channel does not
 * carry, and giving a channel a badge that can never be non-zero would be a
 * worse lie than leaving it out.
 */
function ChannelRow(props: {
  row: ConversationRow;
  /** Pinned inside the open folder (a channel has no global pin). */
  pinned: boolean;
  onMenu: (x: number, y: number) => void;
}): JSX.Element {
  const press = createLongPress((x, y) => props.onMenu(x, y));
  return (
    <A
      href={paths.channel(props.row.id)}
      class="lk-row"
      activeClass=""
      inactiveClass=""
      onContextMenu={(e) => {
        e.preventDefault();
        props.onMenu(e.clientX, e.clientY);
      }}
      onPointerDown={press.onPointerDown}
      onPointerMove={press.onPointerMove}
      onPointerUp={press.onPointerUp}
      onPointerCancel={press.onPointerCancel}
      onPointerLeave={press.onPointerLeave}
      onClick={(e) => press.swallowClick(e)}
    >
      <Avatar label={props.row.title} id={props.row.id} />
      <div class="lk-row-body">
        <div class="lk-row-line">
          <span class="lk-row-title">
            <span class="lk-ellipsis">{props.row.title}</span>
          </span>
        </div>
        <div class="lk-row-line">
          <span class="lk-row-sub">{props.row.preview}</span>
          <span class="lk-row-trail">
            <Show when={props.pinned}>
              <Icon name="pin" size={14} title="Pinned" />
            </Show>
          </span>
        </div>
      </div>
    </A>
  );
}

export function ChatList(props: { activeGroupIdB64: string | null }): JSX.Element {
  const chats = useChats();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();
  const callHost = useCallHost();
  const copy = (): (typeof COPY)["chat"] => COPY.chat;

  const {
    accountId,
    kindOf,
    chatLabel,
    summaryFor,
    endedLine,
    subOf,
    clock,
    orderedRows,
    groupById,
    newestIdFor,
  } = createConversationRows();

  // ----- chat folders (`docs/ux/chat-folders.md`) ------------------------

  // The selected tab is this device's, never synced (UX 2.5).
  createEffect(() => loadSelectedTab(accountId()));
  const folders = (): readonly FolderModel[] => sync.folders();
  /** The open tab, falling back to All chats when that folder is gone. */
  const tab = (): string => resolveTab(selectedTab(), folders().map((f) => f.idHex));
  const currentFolder = (): FolderModel | null =>
    folders().find((f) => f.idHex === tab()) ?? null;

  // The search field in the pane head filters by name and preview.
  const [query, setQuery] = createSignal("");
  const searching = (): boolean => query().trim().length > 0;

  /** The rows of the open tab: All chats, or the folder's own view. */
  const tabRows = createMemo((): readonly ConversationRow[] => {
    const all = orderedRows();
    const folder = currentFolder();
    const rules = uiRules();
    if (folder === null || rules === null) return all;
    return folderRowsOrdered(all, folder, rules);
  });

  /** The ordered list, filtered by what was typed. A search is "where is
   *  it", so it searches EVERYTHING whichever tab is open (UX 2.4). */
  const visibleRows = (): readonly ConversationRow[] => {
    const q = query();
    if (!searching()) return tabRows();
    return orderedRows().filter((row) => rowMatchesQuery(q, row.title, row.preview));
  };

  /** The bar's tabs with their counts. */
  const tabs = createMemo((): FolderTab[] => {
    const rules = uiRules();
    const all = orderedRows();
    return [
      { id: ALL_CHATS, name: "All chats", count: 0 },
      ...folders().map((f) => ({
        id: f.idHex,
        name: f.name,
        count: rules === null ? 0 : folderUnreadCount(all, f, rules),
      })),
    ];
  });

  const [tabMenu, setTabMenu] = createSignal<{ id: string; x: number; y: number } | null>(null);

  /** Mark every conversation in the tab as read: for each, the same read
   *  cursor opening the chat moves, and a manual "unread" cleared. */
  async function markAllRead(tabId: string): Promise<void> {
    const rules = uiRules();
    const folder = folders().find((f) => f.idHex === tabId) ?? null;
    const rows =
      folder === null || rules === null ? orderedRows() : folderRowsOrdered(orderedRows(), folder, rules);
    for (const row of rows) {
      if (row.kind === "channel" || !rowHasUnread(row)) continue;
      const g = groupById().get(row.id);
      if (g === undefined) continue;
      const groupId = g.group.groupId;
      const newest = newestIdFor(groupId);
      try {
        if (newest !== null) {
          const idBytes = new Uint8Array(8);
          let v = newest;
          for (let i = 7; i >= 0; i -= 1) {
            idBytes[i] = Number(v & 0xffn);
            v >>= 8n;
          }
          await sync.setReadCursor(groupId, idBytes, BigInt(Date.now()));
        }
        const f = sync.flags()[syncGroupIdHex(groupId)];
        if (f?.unreadOverride === true) {
          await sync.setFlags(groupId, { ...f, unreadOverride: false });
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("mark all as read: one conversation failed", err);
      }
    }
  }

  // Ctrl+1..9 (Cmd on macOS) select the first nine tabs, All chats being 1.
  onMount(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
      if (e.key < "1" || e.key > "9") return;
      if (folders().length === 0) return;
      const t = tabs()[Number(e.key) - 1];
      if (t === undefined) return;
      e.preventDefault();
      selectTab(accountId(), t.id);
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  /** The people in this device's groups, as base64 account keys - what the
   *  people half of the search field (`search/PeopleSearch.tsx`) matches
   *  locally beside the contacts. */
  const groupPeopleKeys = createMemo((): readonly string[] => {
    const out = new Set<string>();
    for (const g of chats.groups()) for (const k of personKeysOf(g.group)) out.add(k);
    return [...out];
  });

  /** A channel the people search found by its handle opens its subscribe
   *  page. */
  const openFoundChannel = (channelIdHex: string, handle: string): void => {
    setQuery("");
    navigate(paths.channelSubscribe(channelIdHex), { state: { handle } });
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
    // Inside a folder, Pin acts on the folder's own pins (UX 2.3); the
    // All-chats pin is untouched.
    const folder = currentFolder();
    if (field === "pinned" && folder !== null) {
      const ref = refOfRow(kindOf(g), g.groupIdB64);
      if (ref === null) return;
      try {
        await togglePinInFolder(sync, folder, ref);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("folder pin failed", err);
      }
      return;
    }
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
  const [menu, setMenu] = createSignal<{
    row: ConversationRow;
    g: ActiveGroup | null;
    x: number;
    y: number;
  } | null>(null);

  /** Whether the row shows as pinned in the open tab: the folder's own pin
   *  inside a folder, the global pin in All chats. */
  const pinnedHere = (row: ConversationRow): boolean => {
    const folder = currentFolder();
    return folder === null ? row.pinned : isPinnedInFolder(row, folder);
  };
  /** Pin is disabled inside a folder that already holds ten pins. */
  const pinBlocked = (row: ConversationRow): boolean => {
    const folder = currentFolder();
    return folder !== null && !isPinnedInFolder(row, folder) && folder.pinned.length >= MAX_FOLDER_PINS;
  };

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
      <Show when={orderedRows().length > 0}>
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
      {/* `docs/ux/chat-folders.md` section 2: the folder bar, once one folder
          exists. Hidden while searching - a search covers everything. */}
      <Show when={folders().length > 0 && !searching() && orderedRows().length > 0}>
        <FolderBar
          tabs={tabs()}
          selected={tab()}
          onSelect={(id) => selectTab(accountId(), id)}
          onMenu={(id, x, y) => setTabMenu({ id, x, y })}
        />
      </Show>
      <Show
        when={orderedRows().length > 0}
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
          {/* UX 2.4 - one sentence whatever the filter; the person made it. */}
          <Show when={visibleRows().length === 0 && !searching() && currentFolder() !== null}>
            <div class="lk-folder-empty">
              <p>Nothing here right now</p>
              <button
                type="button"
                class="secondary"
                onClick={() => {
                  const f = currentFolder();
                  if (f !== null) openFolderEditor({ mode: "edit", folderIdHex: f.idHex });
                }}
              >
                Edit folder
              </button>
            </div>
          </Show>
          <For each={visibleRows()}>
            {(row) => {
              // A channel is not the MLS-group shape the rest of this row is
              // written against - no roster, no unread summary, no flags
              // keyed on a conversation id - so it draws its own short row
              // and the group row below is untouched.
              if (row.kind === "channel") {
                return (
                  <ChannelRow
                    row={row}
                    pinned={currentFolder() !== null && pinnedHere(row)}
                    onMenu={(x, y) => setMenu({ row, g: null, x, y })}
                  />
                );
              }
              const g = groupById().get(row.id);
              if (g === undefined) return null;
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
              /**
               * A call from this conversation, arriving while it is muted
               * (`docs/ux/calls.md` section 2.1.1, second surface).
               *
               * The row is where the person already looks to see what is
               * happening, and answering from it needs no navigation. Whether
               * the call is this row's, and whether it is still inside the ring
               * window, are `calls/quiet-call-offer.ts`'s to say — the same
               * question the muted conversation's own panel asks, asked once.
               */
              const quietCall = () =>
                quietCallOfferFor(callHost.quietIncomingCall(), g.groupIdB64, Date.now());
              const press = createLongPress((x, y) => setMenu({ row, g, x, y }));
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
                    setMenu({ row, g, x: e.clientX, y: e.clientY });
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
                        <Show when={pinnedHere(row)}>
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
                          title={pinnedHere(row) ? "Unpin" : "Pin"}
                          aria-label={pinnedHere(row) ? "Unpin" : "Pin"}
                          aria-pressed={pinnedHere(row)}
                          disabled={pinBlocked(row)}
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
                    {/* `docs/ux/calls.md` section 2.1.1 — "that chat's own row
                        carries Accept and Decline while the call is ringing".
                        Its own line rather than squeezed onto the preview's,
                        and NOT behind the hover gate the pin / mute / archive
                        actions sit behind: a call has seconds, and a control
                        that appears only under a pointer is no control at all
                        on a touch screen.

                        Both buttons stop the click reaching the row, which is a
                        link: answering a call is not navigation, and section
                        2.1.1 is explicit that this surface "needs no navigation
                        to answer from". The in-call screen is the call host's
                        and comes up over whatever is on screen anyway.

                        Two actions here, not three: section 2.1.1 names Accept
                        and Decline for the row, and `Accept without video`
                        would be a third control on a line that also carries the
                        kind of call, inside a list row. It is on the muted
                        conversation's own panel, which is where a person who
                        cares which way they answer is one tap away. */}
                    <Show when={quietCall()}>
                      {(offer) => (
                        <div
                          class="lk-row-call"
                          role="group"
                          aria-label={`${quietCallKindLabel(offer().video)} from ${label()}`}
                          /* The row arms a long press that opens its pin /
                             mute / archive menu. A thumb held on Accept for
                             half a second must answer the call, not open that
                             menu, so the press never reaches the row: the
                             detector only arms on pointerdown, so stopping it
                             here leaves nothing armed and nothing to cancel. */
                          onPointerDown={(e) => e.stopPropagation()}
                        >
                          <span class="lk-row-call-kind">
                            {quietCallKindLabel(offer().video)}
                          </span>
                          <button
                            type="button"
                            class="danger"
                            aria-label={`Decline call from ${label()}`}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              void callHost.declineIncomingCall();
                            }}
                          >
                            Decline
                          </button>
                          <button
                            type="button"
                            class="primary"
                            aria-label={`Accept call from ${label()}`}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              void callHost.acceptIncomingCall(offer().video);
                            }}
                          >
                            Accept
                          </button>
                        </div>
                      )}
                    </Show>
                  </div>
                </A>
              );
            }}
          </For>
          {/* `docs/ux/who-can-find-me.md` section 6.1 - the people half of
              the field: the people this device knows, matched as the person
              types, and one Search people row that asks the network for the
              username, the name and the channel handle, and adds what it
              finds to the same list. `docs/ux/navigation.md` section 4. */}
          <Show when={query().trim().length > 0}>
            <PeopleSearch
              query={query}
              conversationMatches={() => visibleRows().length}
              groupPeopleKeys={groupPeopleKeys}
              onOpenChannel={openFoundChannel}
            />
          </Show>
        </div>
      </Show>
      <Show when={menu()}>
        {(m) => {
          const g = (): ActiveGroup | null => m().g;
          const f = (): { muted: boolean; archived: boolean } => {
            const grp = g();
            const cur = grp === null ? undefined : sync.flags()[syncGroupIdHex(grp.group.groupId)];
            return { muted: cur?.muted === true, archived: cur?.archived === true };
          };
          const act = (field: Flag): void => {
            const grp = g();
            setMenu(null);
            if (grp !== null) void toggleFlag(grp, field);
          };
          const ref = () => refOfRow(m().row.kind, m().row.id);
          /** Pin on a channel: only a folder's own pin exists for it. */
          const pinChannel = (): void => {
            const folder = currentFolder();
            const r = ref();
            setMenu(null);
            if (folder !== null && r !== null) void togglePinInFolder(sync, folder, r);
          };
          const canPin = (): boolean => m().row.kind !== "channel" || currentFolder() !== null;
          // Every read of `m()` happens BEFORE `setMenu(null)`: the accessor is
          // stale once the menu's `<Show>` closes.
          const removeFromFolder = (): void => {
            const folder = currentFolder();
            const r = ref();
            const rules = uiRules();
            const row = m().row;
            setMenu(null);
            if (folder === null || r === null || rules === null) return;
            const inFolder = rowIsInFolder(row, folder, rules);
            void toggleMembership(sync, folder, r, membershipOf(folder, r, inFolder));
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
                aria-label={m().row.title}
                style={{ left: `${m().x}px`, top: `${m().y}px` }}
              >
                <Show when={canPin()}>
                  <button
                    type="button"
                    role="menuitem"
                    disabled={pinBlocked(m().row)}
                    onClick={() => {
                      const grp = g();
                      if (grp === null) pinChannel();
                      else act("pinned");
                    }}
                  >
                    <Icon name="pin" size={18} />
                    {pinnedHere(m().row) ? "Unpin" : "Pin"}
                  </button>
                  <Show when={pinBlocked(m().row)}>
                    <p class="lk-menu-note">You can pin up to 10 chats in a folder.</p>
                  </Show>
                </Show>
                <Show when={g() !== null}>
                  <button type="button" role="menuitem" onClick={() => act("muted")}>
                    <Icon name="bellOff" size={18} />
                    {f().muted ? "Unmute" : "Mute"}
                  </button>
                  <button type="button" role="menuitem" onClick={() => act("archived")}>
                    <Icon name="archive" size={18} />
                    {f().archived ? "Unarchive" : "Archive"}
                  </button>
                </Show>
                <Show when={ref()}>
                  {(r) => (
                    <AddToFolderItems
                      row={m().row}
                      conversation={r()}
                      folders={folders()}
                      onDone={() => setMenu(null)}
                    />
                  )}
                </Show>
                <Show when={currentFolder() !== null && ref() !== null}>
                  <button type="button" role="menuitem" onClick={removeFromFolder}>
                    <Icon name="close" size={18} />
                    Remove from folder
                  </button>
                </Show>
              </div>
            </>
          );
        }}
      </Show>
      <Show when={tabMenu()}>
        {(tm) => {
          const folder = (): FolderModel | null =>
            folders().find((x) => x.idHex === tm().id) ?? null;
          const count = (): number => tabs().find((t) => t.id === tm().id)?.count ?? 0;
          const anyUnread = (): boolean => orderedRows().some((r) => rowHasUnread(r));
          const close = (): void => {
            setTabMenu(null);
          };
          return (
            <>
              <div
                class="lk-menu-backdrop"
                onClick={close}
                onContextMenu={(e) => {
                  e.preventDefault();
                  close();
                }}
              />
              <div
                class="lk-menu lk-menu-floating"
                role="menu"
                aria-label={folder()?.name ?? "All chats"}
                style={{ left: `${tm().x}px`, top: `${tm().y}px` }}
              >
                <Show
                  when={folder()}
                  fallback={
                    <>
                      <Show when={anyUnread()}>
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            close();
                            void markAllRead(ALL_CHATS);
                          }}
                        >
                          <Icon name="dcheck" size={18} />
                          Mark all as read
                        </button>
                      </Show>
                      <button
                        type="button"
                        role="menuitem"
                        onClick={() => {
                          close();
                          navigate(paths.settingsSection("folders"));
                        }}
                      >
                        <Icon name="folder" size={18} />
                        Edit folders
                      </button>
                    </>
                  }
                >
                  {(fo) => (
                    <>
                      <button
                        type="button"
                        role="menuitem"
                        onClick={() => {
                          const id = fo().idHex;
                          close();
                          openFolderEditor({ mode: "edit", folderIdHex: id });
                        }}
                      >
                        <Icon name="pencil" size={18} />
                        Edit folder
                      </button>
                      <Show when={count() > 0}>
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            const id = fo().idHex;
                            close();
                            void markAllRead(id);
                          }}
                        >
                          <Icon name="dcheck" size={18} />
                          Mark all as read
                        </button>
                      </Show>
                      <button
                        type="button"
                        role="menuitem"
                        onClick={() => {
                          close();
                          navigate(paths.settingsSection("folders"));
                        }}
                      >
                        <Icon name="lines" size={18} />
                        Reorder folders
                      </button>
                      <button
                        type="button"
                        role="menuitem"
                        class="danger-item"
                        onClick={() => {
                          const target = fo();
                          close();
                          void deleteFolder(sync, target);
                        }}
                      >
                        <Icon name="trash" size={18} />
                        Delete folder
                      </button>
                    </>
                  )}
                </Show>
              </div>
            </>
          );
        }}
      </Show>
    </aside>
  );
}
