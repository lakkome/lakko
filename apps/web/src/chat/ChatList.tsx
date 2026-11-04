// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The conversation list, used twice (docs/ux/client-redesign.md section 5.1):
 * once as Chats (one-to-one) and once as Groups, split by the kind each
 * conversation carries in its GroupContext (ADR-0062, `Group.groupKind()`)
 * over the one store. One row per conversation: avatar, name (bold while unread), the time
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
import { dropRequestSent, requestsSent } from "./requests-sent";
import { RequestsSentScreen } from "./RequestsSentScreen";
import { unseenReactionIds } from "./unseen-reactions";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  untrack,
  type JSX,
} from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { isDesktop } from "../platform";
import {
  clearListCommand,
  folderTabIndexFor,
  neighbourHref,
  noteConversationListMounted,
  pendingListCommand,
  type ListCommand,
} from "../platform/shortcuts";

import { useCallHost } from "../calls/CallHostContext";
import { quietCallKindLabel, quietCallOfferFor } from "../calls/quiet-call-offer";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { oneToOnePeerKey, ownPersonKeyB64, personKeysOf } from "./person-key";
import { PeopleSearch } from "../search/PeopleSearch";
import { rowHasUnread, type ConversationRow } from "./conversation-list";
import { createConversationRows } from "./conversation-rows";
import { useLineage } from "./use-lineage";
import { uiRules } from "./ui-rules";
import { badgeText } from "../feed/feed-marks";
import { formatListTime } from "./list-time";
import { rowMatchesQuery } from "./list-preview";
import { showsEmptyState } from "./list-empty-state";
import { useChats, type ActiveGroup } from "./ChatContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import type { ChatListSummary } from "./background-log";
import { showsSynchronizing } from "./synchronizing";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { createLongPress } from "../ui/long-press";
import { t } from "../l10n";
import { FolderBar, type FolderTab } from "./folders/FolderBar";
import { MembershipEndBanners } from "./MembershipEndNotices";
import { RestoredNotice, useRestoredList } from "./RestoredNotice";
import { RecallDoneNotice } from "../devices/RecallDoneNotice";
import { KeysRotatedNotice } from "../devices/KeysRotatedNotice";
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
import { conversationKindOf } from "./conversation-kind";
import {
  loadPendingContacts,
  pendingContacts,
  lastPendingMessageText,
  pendingNameOf,
  shownPendingContacts,
  type PendingContact,
} from "../contacts/pending-contacts";
import { displayLocale } from "../l10n/display-locale";
import { channelsState } from "../state/channels-state";

type Flag = "pinned" | "muted" | "archived";

/** The pane's copy, rendered in the current language on every read. */
const COPY = {
  chat: {
    title: (): string => t("nav.chats"),
    compose: (): string => t("chats.compose"),
    emptyTitle: (): string => t("chats.empty.title"),
    emptyAction: (): string => t("chats.empty.action"),
    aria: (): string => t("nav.chats"),
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
      <div class="lk-menu" role="menu" aria-label={t("chats.create.menu")}>
        <button type="button" role="menuitem" onClick={() => go(paths.contacts)}>
          <Icon name="compose" size={16} />
          {t("chats.create.chat")}
        </button>
        <button type="button" role="menuitem" onClick={() => go(paths.newGroup)}>
          <Icon name="groups" size={16} />
          {t("chats.create.group")}
        </button>
        <button type="button" role="menuitem" onClick={() => go(paths.channelNew)}>
          <Icon name="channels" size={16} />
          {t("chats.create.channel")}
        </button>
        {/* ADR-0065 section 3 — Saved, from the menu that holds New group. */}
        <div class="lk-menu-sep" />
        <button type="button" role="menuitem" onClick={() => go(paths.saved)}>
          <Icon name="bookmark" size={16} />
          {t("saved.title")}
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
 * it does NOT have is the global pin / mute glyphs - those are keyed on a
 * conversation group id a channel does not carry (the pin shown is the one
 * inside the open folder). The badge is
 * the channel's count of new posts (docs/ux/feed.md section 5, "Channel
 * posts"): the posts newer than the newest one read, in the feed or in the
 * channel, drawn like a chat's unread badge.
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
      <Avatar label={props.row.title} id={props.row.id} personKey={props.row.personKey} />
      <div class="lk-row-body">
        <div class="lk-row-line">
          <span class="lk-row-title" classList={{ unread: props.row.unread > 0 }}>
            <span class="lk-ellipsis">{props.row.title}</span>
          </span>
        </div>
        <div class="lk-row-line">
          <span class="lk-row-sub">{props.row.preview}</span>
          <span class="lk-row-trail">
            <Show when={props.pinned}>
              <Icon name="pin" size={14} title={t("chats.row.pinned")} />
            </Show>
            <Show when={props.row.unread > 0}>
              <span class="lk-badge" title={t("channels.row.new_posts_a11y", { count: props.row.unread })}>
                {badgeText(props.row.unread)}
              </span>
            </Show>
          </span>
        </div>
      </div>
    </A>
  );
}

export function ChatList(props: { activeGroupIdB64: string | null }): JSX.Element {
  const chats = useChats();
  const [requestsOpen, setRequestsOpen] = createSignal(false);
  // ADR-0087 UX 6.2 — a request leaves the list once its group is in Chats.
  createEffect(() => {
    for (const r of requestsSent()) if (chats.get(r.idB64) !== undefined) dropRequestSent(r.idB64);
  });
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
    conversationIdOf,
    conversationIdB64Of,
    readPositionBytes,
  } = createConversationRows();
  const lineageView = useLineage();

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

  // ADR-0079 — a person added through their invite link is in the list at
  // once, under the name the link states (UX section 4), until their app
  // answers and the one-to-one takes over the row. Shown in All chats only:
  // a folder is a view of conversations, and this one does not exist yet.
  createEffect(() => {
    const account = accountId();
    if (account !== null) void loadPendingContacts(account);
  });
  const invitedName = (p: PendingContact): string =>
    pendingNameOf(p) ?? t("invite.contact.page.unnamed");
  const invitedRows = createMemo((): PendingContact[] => {
    const own = ownPersonKeyB64(chats.identity());
    const peers = new Set<string>();
    for (const g of chats.groups()) {
      if (conversationKindOf(g.group) !== "chat") continue;
      for (const k of personKeysOf(g.group)) if (k !== own) peers.add(k);
    }
    const shown = shownPendingContacts(pendingContacts(), peers);
    const q = query();
    return searching() ? shown.filter((p) => rowMatchesQuery(q, invitedName(p), "")) : shown;
  });

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
    return orderedRows().filter((row) => rowMatchesQuery(q, row.title, row.preview, row.handle));
  };

  /** The bar's tabs with their counts. */
  const tabs = createMemo((): FolderTab[] => {
    const rules = uiRules();
    const all = orderedRows();
    return [
      { id: ALL_CHATS, name: t("folders.all_chats"), count: 0 },
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
      // ADR-0073 R.5 — the read position and the flags are the
      // CONVERSATION's; the newest message is the head group's.
      const conversationId = conversationIdOf(g);
      const newest = newestIdFor(g.group.groupId);
      try {
        if (newest !== null) {
          await sync.setReadCursor(conversationId, readPositionBytes(g, newest), BigInt(Date.now()));
        }
        const f = sync.flags()[syncGroupIdHex(conversationId)];
        if (f?.unreadOverride === true) {
          await sync.setFlags(conversationId, { ...f, unreadOverride: false });
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("mark all as read: one conversation failed", err);
      }
    }
  }

  // Ctrl+1..9 (Cmd on macOS) select the first nine tabs, All chats being 1.
  // Never with Shift: Mod+Shift+1..5 are the desktop destinations.
  onMount(() => {
    const onKey = (e: KeyboardEvent): void => {
      const index = folderTabIndexFor(e);
      if (index === null) return;
      if (folders().length === 0) return;
      const t = tabs()[index];
      if (t === undefined) return;
      e.preventDefault();
      selectTab(accountId(), t.id);
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  // docs/ux/identity-recovery.md 7.2: what a restored device says while its
  // conversations are on their way back.
  const restoredList = useRestoredList(() => orderedRows().length);

  /** The people in this device's groups, as base64 account keys - what the
   *  people half of the search field (`search/PeopleSearch.tsx`) matches
   *  locally beside the contacts. */
  const groupPeopleKeys = createMemo((): readonly string[] => {
    const out = new Set<string>();
    for (const g of chats.groups()) for (const k of personKeysOf(g.group)) out.add(k);
    return [...out];
  });

  /** The other person of a one-to-one, or `null`. */
  const chatPeerOf = (g: ActiveGroup): string | null =>
    kindOf(g) === "chat" ? oneToOnePeerKey(personKeysOf(g.group), ownPersonKeyB64(chats.identity())) : null;

  /** The people whose one-to-one is among the matching conversations. */
  /** The ids of the conversation rows the local filter shows (a group's
   *  conversation id, a channel's hex id), so a directory result for one of
   *  them is not listed twice (one conversation, one row). */
  const visibleRowIds = (): ReadonlySet<string> => new Set(visibleRows().map((row) => row.id));
  const visibleChatPeers = (): ReadonlySet<string> => {
    const out = new Set<string>();
    for (const row of visibleRows()) {
      const g = groupById().get(row.id);
      const peer = g === undefined ? null : chatPeerOf(g);
      if (peer !== null) out.add(peer);
    }
    return out;
  };

  /** The people added through their link whose row is on screen. */
  const listedInvited = (): ReadonlySet<string> => new Set(invitedRows().map((p) => p.signaturePubkeyB64));

  /** The route of the one-to-one with a person, whatever the list shows. */
  const chatHrefOf = (personKeyB64: string): string | null => {
    const g = chats.groups().find((candidate) => chatPeerOf(candidate) === personKeyB64);
    return g === undefined ? null : hrefOf(g);
  };

  /** The name of a channel this device already knows (a subscription or its
   *  own); a channel it does not know has only its handle to show. */
  const channelNameOf = (channelIdHex: string): string | null => {
    const shared = channelsState.metaByChannel().get(channelIdHex)?.name?.trim() ?? "";
    if (shared.length > 0) return shared;
    const local =
      channelsState.channels().find((c) => c.id === channelIdHex)?.localName.trim() ?? "";
    return local.length > 0 ? local : null;
  };

  /** A channel the people search found by its handle opens its subscribe
   *  page. */
  /** A channel from search: its Subscribe subscribes in one step and the
   *  channel opens under its name (`docs/ux/group-and-channel-privacy.md`
   *  section 5.1); a channel this account follows just opens. */
  const openFoundChannel = (channelIdHex: string, handle: string, name?: string, subscribe?: boolean): void => {
    setQuery("");
    navigate(paths.channelSubscribe(channelIdHex), {
      state: { handle, ...(name !== undefined ? { name } : {}), ...(subscribe === true ? { subscribe: true } : {}) },
    });
  };

  /** ADR-0050 section 2.9 — a device added through another device was
   *  handed, in the pairing bundle, the groups that device holds, and is
   *  being admitted to them. While any is still missing there is exactly one
   *  thing to say (`synchronizing.ts`). A device restored from the phrase
   *  with no other device alive shows no such label. */
  const synchronizing = (): boolean =>
    showsSynchronizing(chats.groups().map((g) => g.groupIdB64));

  async function toggleFlag(g: ActiveGroup, field: Flag): Promise<void> {
    // Inside a folder, Pin acts on the folder's own pins (UX 2.3); the
    // All-chats pin is untouched.
    const folder = currentFolder();
    if (field === "pinned" && folder !== null) {
      const ref = refOfRow(kindOf(g), conversationIdB64Of(g));
      if (ref === null) return;
      try {
        await togglePinInFolder(sync, folder, ref);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("folder pin failed", err);
      }
      return;
    }
    const groupId = conversationIdOf(g);
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
    // ADR-0073 R.5 — the route is the CONVERSATION's: it stays the same when
    // the conversation continues in a successor group.
    const slug = base64UrlEncodeNoPad(conversationIdOf(g));
    return kindOf(g) === "chat" ? paths.chat(slug) : paths.groupChat(slug);
  };

  // docs/ux/desktop.md section 5 - the desktop shortcuts this list carries
  // out (`platform/shortcuts.ts`): focus the search field, open the create
  // menu, open the previous / next conversation in the list's own order.
  let searchInput: HTMLInputElement | undefined;
  const location = useLocation();
  const rowHref = (row: ConversationRow): string | null => {
    if (row.kind === "channel") return paths.channel(row.id);
    const g = groupById().get(row.id);
    return g === undefined ? null : hrefOf(g);
  };
  const runListCommand = (cmd: ListCommand): void => {
    if (cmd === "search") {
      searchInput?.focus();
      searchInput?.select();
    } else if (cmd === "create") {
      setCreateOpen(true);
    } else {
      const hrefs = visibleRows()
        .map(rowHref)
        .filter((h): h is string => h !== null);
      const to = neighbourHref(hrefs, location.pathname, cmd);
      if (to !== null) navigate(to);
    }
  };
  if (isDesktop()) {
    onMount(() => onCleanup(noteConversationListMounted()));
    createEffect(() => {
      const cmd = pendingListCommand();
      if (cmd === null) return;
      clearListCommand();
      // After mount: the search field's ref is set by then.
      queueMicrotask(() => untrack(() => runListCommand(cmd)));
    });
  }

  return (
    <aside class="lk-pane" aria-label={copy().aria()}>
      <div class="lk-pane-head">
        <h1>{copy().title()}</h1>
        {/* ADR-0050 / docs/ux/second-device.md section 2: one label while a
            device added through another device is still being admitted to the
            groups the pairing bundle named. Chats appear as they arrive; the
            label goes when the last of them has. Never a counter, never a
            per-chat row. */}
        <Show when={synchronizing()}>
          <span class="lk-pane-status" role="status">
            {t("chats.synchronizing")}
          </span>
        </Show>
        <div class="lk-menu-anchor">
          <button
            type="button"
            class="lk-icon-button accent"
            title={copy().compose()}
            aria-label={copy().compose()}
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
      {/* `docs/ux/navigation.md` section 4 - the field is there with an
          empty list too: finding the first person to talk to is what a new
          account needs it for. */}
      <label class="lk-search lk-pane-search">
        <Icon name="search" size={16} />
        <input
          ref={searchInput}
          type="search"
          placeholder={t("chats.search.placeholder")}
          aria-label={t("chats.search.label")}
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
      </label>
      <MembershipEndBanners activeSlug={props.activeGroupIdB64} />
      {/* docs/ux/identity-recovery.md 18.4: the account was taken back here. */}
      <RecallDoneNotice />
      <KeysRotatedNotice />
      <Show when={restoredList() === "banner"}>
        <RestoredNotice state="banner" />
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
      {/* The empty state stands only while nothing is typed; once something
          is, it gives way to what the search finds, the people half
          included (`docs/ux/who-can-find-me.md` section 6.1). */}
      <Show
        when={!showsEmptyState(orderedRows().length + invitedRows().length, query())}
        fallback={
          <Show when={restoredList() !== "card"} fallback={<RestoredNotice state="card" />}>
            <div class="lk-empty">
              <Icon name="chat" size={40} strokeWidth={1.4} />
              <strong>{copy().emptyTitle()}</strong>
              {/* The SAME menu the compose button opens, not a different set
                  of choices in a different place. */}
              <button
                type="button"
                class="primary"
                aria-haspopup="menu"
                aria-expanded={createOpen()}
                onClick={() => setCreateOpen((open) => !open)}
              >
                {copy().emptyAction()}
              </button>
            </div>
          </Show>
        }
      >
        <div class="lk-pane-body">
          {/* ADR-0087 UX 6.2 — the Requests sent row, at the top, while one is open. */}
          <Show when={currentFolder() === null && !searching() && requestsSent().length > 0}>
            <button type="button" class="lk-row" onClick={() => setRequestsOpen(true)}>
              <span class="lk-avatar" aria-hidden="true">
                <Icon name="groups" size={18} />
              </span>
              <div class="lk-row-body">
                <div class="lk-row-line">
                  <span class="lk-row-title">{t("requests_sent.title")}</span>
                </div>
              </div>
              <span class="lk-row-trail">
                <span class="lk-badge muted">{requestsSent().length}</span>
              </span>
            </button>
          </Show>
          <RequestsSentScreen open={requestsOpen()} onBack={() => setRequestsOpen(false)} />
          {/* UX 2.4 - one sentence whatever the filter; the person made it. */}
          <Show when={visibleRows().length === 0 && !searching() && currentFolder() !== null}>
            <div class="lk-folder-empty">
              <p>{t("folders.empty")}</p>
              <button
                type="button"
                class="secondary"
                onClick={() => {
                  const f = currentFolder();
                  if (f !== null) openFolderEditor({ mode: "edit", folderIdHex: f.idHex });
                }}
              >
                {t("folders.edit")}
              </button>
            </div>
          </Show>
          <Show when={currentFolder() === null || searching()}>
            <For each={invitedRows()}>
              {(p) => (
                <A href={paths.pendingChat(p.accountIdHex)} class="lk-row" activeClass="" inactiveClass="">
                  <Avatar label={invitedName(p)} id={p.signaturePubkeyB64} personKey={p.signaturePubkeyB64} />
                  <div class="lk-row-body">
                    <div class="lk-row-line">
                      <span class="lk-row-title">
                        <span class="lk-ellipsis">{invitedName(p)}</span>
                      </span>
                    </div>
                    <div class="lk-row-line">
                      <span class="lk-row-sub">{lastPendingMessageText(p)}</span>
                    </div>
                  </div>
                </A>
              )}
            </For>
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
              const key = syncGroupIdHex(conversationIdOf(g));
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
              const time = (): string => formatListTime(summary().lastAt, clock(), displayLocale());
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
                  <Avatar
                    label={label()}
                    id={conversationIdB64Of(g)}
                    personKey={row.personKey}
                    conversationKey={row.personKey === undefined ? g.groupIdB64 : undefined}
                  />
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
                          <Icon name="bellOff" size={14} title={t("chats.row.muted")} />
                        </Show>
                        <Show when={pinnedHere(row)}>
                          <Icon name="pin" size={14} title={t("chats.row.pinned")} />
                        </Show>
                        {/* Reactions section 12 — a small heart beside the
                            badge while others' reactions to the person's
                            own messages here are unseen. */}
                        <Show when={unseenReactionIds(accountId(), g.groupIdB64).length > 0}>
                          <span class="lk-list-unseen-reactions" title={t("chats.row.unseen_reactions")}>
                            <Icon name="heart" size={14} />
                          </span>
                        </Show>
                        <Show when={unread() > 0}>
                          <span
                            class="lk-badge"
                            classList={{ muted: flags().muted }}
                            title={t("chats.row.unread", { count: unread() })}
                          >
                            {unread() >= 100 ? t("chats.row.unread_overflow") : unread().toLocaleString(displayLocale())}
                          </span>
                        </Show>
                      </span>
                      <span class="lk-row-actions" role="group" aria-label={t("chats.row.actions")}>
                        <button
                          type="button"
                          title={pinnedHere(row) ? t("chats.action.unpin") : t("chats.action.pin")}
                          aria-label={pinnedHere(row) ? t("chats.action.unpin") : t("chats.action.pin")}
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
                          title={flags().muted ? t("chats.action.unmute") : t("chats.action.mute")}
                          aria-label={flags().muted ? t("chats.action.unmute") : t("chats.action.mute")}
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
                          title={flags().archived ? t("chats.action.unarchive") : t("chats.action.archive")}
                          aria-label={flags().archived ? t("chats.action.unarchive") : t("chats.action.archive")}
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
                          aria-label={t("chats.row.call.label", { kind: quietCallKindLabel(offer().video), name: label() })}
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
                            aria-label={t("call.incoming.decline_from", { name: label() })}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              void callHost.declineIncomingCall();
                            }}
                          >
                            {t("call.incoming.decline")}
                          </button>
                          <button
                            type="button"
                            class="primary"
                            aria-label={t("call.incoming.accept_from", { name: label() })}
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              void callHost.acceptIncomingCall(offer().video);
                            }}
                          >
                            {t("call.incoming.accept")}
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
              types, and, from three characters with nothing to press, what
              the network finds by username, name and channel handle, in the
              same list under People. */}
          <Show when={query().trim().length > 0}>
            <PeopleSearch
              scope="chats"
              query={query}
              conversationMatches={() => visibleRows().length}
              groupPeopleKeys={groupPeopleKeys}
              onOpenChannel={openFoundChannel}
              listedB64={listedInvited}
              standingAboveB64={visibleChatPeers}
              standingConversationIds={visibleRowIds}
              chatHrefOf={chatHrefOf}
              channelNameOf={channelNameOf}
              onOpenConversation={(idB64) => {
                const g = chats.get(idB64) ?? lineageView.resolve(idB64)?.head;
                if (g === undefined) return;
                setQuery("");
                navigate(paths.groupChat(base64UrlEncodeNoPad(g.group.groupId)));
              }}
            />
          </Show>
        </div>
      </Show>
      <Show when={menu()}>
        {(m) => {
          const g = (): ActiveGroup | null => m().g;
          const f = (): { muted: boolean; archived: boolean } => {
            const grp = g();
            const cur = grp === null ? undefined : sync.flags()[syncGroupIdHex(conversationIdOf(grp))];
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
                    {pinnedHere(m().row) ? t("chats.action.unpin") : t("chats.action.pin")}
                  </button>
                  <Show when={pinBlocked(m().row)}>
                    <p class="lk-menu-note">{t("folders.pin_limit", { count: MAX_FOLDER_PINS })}</p>
                  </Show>
                </Show>
                <Show when={g() !== null}>
                  <button type="button" role="menuitem" onClick={() => act("muted")}>
                    <Icon name="bellOff" size={18} />
                    {f().muted ? t("chats.action.unmute") : t("chats.action.mute")}
                  </button>
                  <button type="button" role="menuitem" onClick={() => act("archived")}>
                    <Icon name="archive" size={18} />
                    {f().archived ? t("chats.action.unarchive") : t("chats.action.archive")}
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
                    {t("folders.remove_from_folder")}
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
          const count = (): number => tabs().find((tab) => tab.id === tm().id)?.count ?? 0;
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
                aria-label={folder()?.name ?? t("folders.all_chats")}
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
                          {t("folders.mark_all_read")}
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
                        {t("folders.edit_all")}
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
                        {t("folders.edit")}
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
                          {t("folders.mark_all_read")}
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
                        {t("folders.reorder")}
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
                        {t("folders.delete")}
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
