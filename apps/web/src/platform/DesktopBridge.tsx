// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page's side of the desktop host (ADR-0085 section 4,
 * `docs/ux/desktop.md` sections 2, 3 and 5). Renders nothing; mounted once,
 * inside the router, and only in the desktop build.
 *
 * - Tells the host whether a call is live (a quit during a call asks first)
 *   and how many conversations are unread (the app-icon badge): the same
 *   count the folder tabs show, over All chats.
 * - Answers the host's `quit-requested` with "End the call and quit?", and
 *   `open-settings` by opening Settings.
 * - Raises the system notifications of `desktop.md` sections 4 and 4.1: a
 *   message when a conversation's unread count rises (rules in
 *   `desktop-notifications.ts`), a ringing call when the window is not in
 *   front, with Accept and Decline, taken back when the ring ends; a click on
 *   one opens its conversation (`open-target`). A press of Accept or Decline
 *   reaches `calls/CallActionListener.tsx`.
 * - Says once that Lakko is still running when the window first hides on
 *   Windows and Linux, and keeps the tray menu in the interface language.
 * - Turns `Mod+K` / `Mod+N` / `Alt+Up` / `Alt+Down` into commands for the
 *   conversation list (`platform/shortcuts.ts`), and `Mod+Shift+1..5` into
 *   the five destinations.
 */
import { createEffect, createMemo, createSignal, onCleanup, onMount, untrack, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { base64UrlEncodeNoPad } from "../auth/account-id";
import { useCallHost } from "../calls/CallHostContext";
import type { ConversationRow } from "../chat/conversation-list";
import { createConversationRows } from "../chat/conversation-rows";
import { allChatsUnreadCount } from "../chat/folders/folder-view";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { requestConfirm } from "../ui/confirm-dialog";
import { callAlert, callTag, messageNotifications, type NotifyRow } from "./desktop-notifications";
import {
  onHostEvent,
  postHostState,
  requestHostAttention,
  requestHostQuit,
  setHostTrayLabels,
  showHostNotification,
  takeHostLink,
  withdrawHostNotification,
} from "./host";
import { isDesktop } from "./index";
import { appPathOf } from "./lakko-link";
import {
  conversationListMounted,
  isMacPlatform,
  keyTargetOf,
  requestListCommand,
  shortcutFor,
  type Destination,
} from "./shortcuts";

/** Where `Mod+Shift+1..5` go. */
const DESTINATION_PATHS: Readonly<Record<Destination, string>> = {
  chats: paths.home,
  feed: paths.feed,
  contacts: paths.contacts,
  calls: paths.calls,
  settings: paths.settings,
};

export function DesktopBridge(): JSX.Element {
  const callHost = useCallHost();
  const navigate = useNavigate();
  const location = useLocation();
  const { orderedRows, groupById, kindOf, conversationIdOf } = createConversationRows();

  /** Where a row opens, as the conversation list routes it. */
  const hrefOf = (row: ConversationRow): string | null => {
    if (row.kind === "channel") return paths.channel(row.id);
    const g = groupById().get(row.id);
    if (g === undefined) return null;
    const slug = base64UrlEncodeNoPad(conversationIdOf(g));
    return kindOf(g) === "chat" ? paths.chat(slug) : paths.groupChat(slug);
  };

  /** Whether the window is in front of the person. */
  const [focused, setFocused] = createSignal(typeof document !== "undefined" && document.hasFocus());

  const callLive = (): boolean => callHost.anyCallInProgress();
  const unread = createMemo(() => allChatsUnreadCount(orderedRows()));

  // Post on a change of either value, and once at start.
  let posted: string | null = null;
  createEffect(() => {
    const state = { callLive: callLive(), unread: unread() };
    const key = `${state.callLive ? 1 : 0}:${state.unread}`;
    if (key === posted) return;
    posted = key;
    void postHostState(state);
  });

  // Messages: announce a rise in a conversation's unread count.
  let counts: ReadonlyMap<string, number> | null = null;
  createEffect(() => {
    const rows: NotifyRow[] = orderedRows().map((r) => ({
      key: `${r.kind}:${r.id}`,
      unread: r.unread,
      muted: r.muted,
      title: r.title,
      preview: r.preview,
      href: hrefOf(r),
      channel: r.kind === "channel",
    }));
    const view = untrack(() => ({ focused: focused(), path: location.pathname }));
    const { next, notify } = messageNotifications(counts, rows, view, t("notification.message.new"));
    counts = next;
    for (const n of notify) void showHostNotification(n);
  });

  // Calls: one alert per ringing 1x1 call.
  let alertedCall: string | null = null;
  createEffect(() => {
    const ringing = callHost.ringingCallIdHex();
    const quiet = callHost.quietIncomingCall() !== null;
    if (ringing === null) {
      if (alertedCall !== null) void withdrawHostNotification(callTag(alertedCall));
      alertedCall = null;
      return;
    }
    const groupB64 = untrack(() => callHost.activeGroupIdB64());
    const callerRow = untrack(() =>
      orderedRows().find((r) => r.kind !== "channel" && groupById().get(r.id)?.groupIdB64 === groupB64),
    );
    const alert = callAlert({
      ringingCallId: ringing,
      quiet,
      alreadyAlerted: alertedCall,
      focused: untrack(focused),
      callerTitle: callerRow?.title ?? t("call.notification.incoming"),
      incomingWord: t("call.notification.incoming"),
      acceptWord: t("call.incoming.accept"),
      declineWord: t("call.incoming.decline"),
      video: untrack(() => callHost.ringingOfferedVideo()),
    });
    if (alert.kind === "none") return;
    alertedCall = ringing;
    if (alert.kind === "attention") void requestHostAttention();
    else void showHostNotification(alert.notification);
  });

  createEffect(() => {
    void setHostTrayLabels({ open: t("desktop.tray.open"), quit: t("desktop.tray.quit") });
  });

  let asking = false;
  async function onQuitRequested(): Promise<void> {
    if (asking) return;
    try {
      if (callLive()) {
        asking = true;
        const ok = await requestConfirm({
          title: t("desktop.quit_call.title"),
          body: null,
          confirmLabel: t("desktop.quit_call.confirm"),
          cancelLabel: t("common.cancel"),
          tone: "danger",
        });
        if (!ok) return;
        await callHost.endCurrentCall();
      }
      await postHostState({ callLive: false, unread: unread() });
      await requestHostQuit();
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[desktop] quit failed", err);
    } finally {
      asking = false;
    }
  }

  // A file dropped outside a conversation would otherwise replace the page.
  // The conversation's own handlers run first and claim the event.
  onMount(() => {
    const refuse = (e: DragEvent): void => {
      if (e.defaultPrevented) return;
      e.preventDefault();
      if (e.dataTransfer !== null) e.dataTransfer.dropEffect = "none";
    };
    window.addEventListener("dragover", refuse);
    window.addEventListener("drop", refuse);
    onCleanup(() => {
      window.removeEventListener("dragover", refuse);
      window.removeEventListener("drop", refuse);
    });
  });

  onMount(() => {
    // App.tsx mounts this in the desktop build only; asked again so a stray
    // mount in the browser takes no key.
    if (!isDesktop()) return;
    const offQuit = onHostEvent("quit-requested", () => void onQuitRequested());
    const offSettings = onHostEvent("open-settings", () => navigate(paths.settings));
    const openArrivedLink = async (): Promise<void> => {
      const link = await takeHostLink();
      if (link !== null) navigate(appPathOf(link));
    };
    const offLink = onHostEvent("link-arrived", () => void openArrivedLink());
    // A link that started the app arrived before this page existed.
    void openArrivedLink();
    const offTarget = onHostEvent("open-target", (target) => {
      if (target !== null) navigate(target);
    });
    const offStillRunning = onHostEvent("still-running", () => {
      void showHostNotification({
        tag: "still-running",
        title: t("desktop.still_running.title"),
        body: t("desktop.still_running.body"),
        target: null,
      });
    });
    const onFocus = (): void => { setFocused(true); };
    const onBlur = (): void => { setFocused(false); };
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    const mac = isMacPlatform();
    const onKey = (e: KeyboardEvent): void => {
      if (e.defaultPrevented || e.isComposing) return;
      const cmd = shortcutFor(e, mac, keyTargetOf(e.target));
      if (cmd === null) return;
      if (typeof cmd === "object") {
        e.preventDefault();
        navigate(DESTINATION_PATHS[cmd.destination]);
        return;
      }
      if (cmd === "previous" || cmd === "next") {
        // Only where a conversation list is on screen to move through.
        if (!conversationListMounted()) return;
        e.preventDefault();
        requestListCommand(cmd);
        return;
      }
      e.preventDefault();
      if (!conversationListMounted()) navigate(paths.home);
      requestListCommand(cmd);
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      offQuit();
      offSettings();
      offLink();
      offTarget();
      offStillRunning();
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("keydown", onKey);
    });
  });

  return null;
}
