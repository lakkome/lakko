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
 * - Turns `Mod+K` / `Mod+N` / `Alt+Up` / `Alt+Down` into commands for the
 *   conversation list (`platform/shortcuts.ts`), and `Mod+Shift+1..5` into
 *   the five destinations.
 */
import { createEffect, createMemo, onCleanup, onMount, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useCallHost } from "../calls/CallHostContext";
import { createConversationRows } from "../chat/conversation-rows";
import { allChatsUnreadCount } from "../chat/folders/folder-view";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { requestConfirm } from "../ui/confirm-dialog";
import { onHostEvent, postHostState, requestHostQuit } from "./host";
import { isDesktop } from "./index";
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
  const { orderedRows } = createConversationRows();

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

  onMount(() => {
    // App.tsx mounts this in the desktop build only; asked again so a stray
    // mount in the browser takes no key.
    if (!isDesktop()) return;
    const offQuit = onHostEvent("quit-requested", () => void onQuitRequested());
    const offSettings = onHostEvent("open-settings", () => navigate(paths.settings));
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
      window.removeEventListener("keydown", onKey);
    });
  });

  return null;
}
