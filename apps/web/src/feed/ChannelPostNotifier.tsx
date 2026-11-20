// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channel post notifications (`docs/ux/channel-notifications.md`): hears
 * every channel read the post sync makes (`ChannelPostSync.tsx`), asks the
 * shared rule which post notifies (`channel-notify.ts`), shows it, and keeps
 * the per-channel "last notified post" cursor on this device
 * (`channel-notified-store.ts`).
 *
 * Mounted once in the router's root layout (it navigates on a tap). The
 * notification goes the way the account's other page-raised notification
 * goes (`devices/DeviceAddedNotice.tsx`): through the host in the desktop
 * app, else the browser's Notification API, else the service worker where a
 * browser allows notifications only from there. Title: the channel's name as
 * its Chats row shows it; body: the row's second line for that post; tag:
 * the channel, so a newer post replaces the older notification; a tap opens
 * the channel, at its newest end, where the newest notified post is.
 */
import { onCleanup, onMount, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { channelHandleOf, channelReaderDeviceRoleOf, channelRoleOf } from "../channels/channel-access";
import { channelFlagsOf } from "../channels/channel-flags";
import { channelRowPreview, channelTitle } from "../channels/channel-row";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { t } from "../l10n";
import { isDesktop } from "../platform";
import { showHostNotification } from "../platform/host";
import { paths } from "../routes/paths";
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { subscribeChannelReads } from "./ChannelPostSync";
import { channelNotificationTag, decideChannelRead, type ChannelRead } from "./channel-notify";
import { readChannelNotified, writeChannelNotified } from "./channel-notified-store";

/** The message the service worker posts when its copy of a channel
 *  notification is tapped in an open tab (`public/sw.js`). */
export const OPEN_PATH_MESSAGE = "LAKKO_OPEN_PATH";

function notificationsAllowed(): boolean {
  // The desktop host shows them; the OS asks for permission itself.
  if (isDesktop()) return true;
  return typeof Notification !== "undefined" && Notification.permission === "granted";
}

async function showChannelNotification(
  tag: string,
  title: string,
  body: string,
  path: string,
  open: (path: string) => void,
): Promise<void> {
  if (isDesktop()) {
    await showHostNotification({ tag, title, body, target: path });
    return;
  }
  try {
    const n = new Notification(title, { body, tag });
    n.onclick = () => {
      window.focus();
      open(path);
      n.close();
    };
    return;
  } catch {
    // Some browsers (Chrome on Android) allow notifications only from a
    // service worker.
  }
  try {
    const reg = await navigator.serviceWorker?.getRegistration();
    await reg?.showNotification(title, { body, tag, data: { lakkoOpenPath: path } });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[feed] channel notification not shown", err instanceof Error ? err.name : "");
  }
}

export function ChannelPostNotifier(): JSX.Element {
  const auth = useAuth();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();
  const location = useLocation();

  /** The channel at `path` is open in front of the person. */
  const shows = (path: string): boolean =>
    typeof document !== "undefined" &&
    document.visibilityState === "visible" &&
    document.hasFocus() &&
    location.pathname === path;

  async function onReads(accountIdB64: string, reads: readonly ChannelRead[]): Promise<void> {
    const s = auth.state();
    if (s.kind !== "session" || s.account.accountId !== accountIdB64) return;
    const wasm = await loadWasm();
    const stored = readChannelNotified(accountIdB64);
    const held = channelsState.channels();
    const keep = new Set(held.map((c) => c.id));
    for (const read of reads) {
      const id = read.channelIdHex;
      const path = paths.channel(id);
      const role = channelRoleOf(id);
      const decision = decideChannelRead(wasm, read, {
        muted: channelFlagsOf(sync.flags(), id).muted,
        openAndVisible: shows(path),
        // A reader device of the owner or a publisher (UX second-device.md
        // section 4a) announces nothing: the account posts there itself.
        own: role === "owner" || role === "publisher" || channelReaderDeviceRoleOf(id) !== null,
        lastNotified: stored[id] ?? 0,
        nowMs: Date.now(),
      });
      writeChannelNotified(accountIdB64, id, decision.lastNotified, keep);
      if (decision.post === null || !notificationsAllowed()) continue;
      const channel = held.find((c) => c.id === id);
      const title = channelTitle(
        channelsState.metaByChannel().get(id)?.name,
        channel?.localName ?? "",
        channelHandleOf(id) ?? channel?.localHandle,
      );
      const body = channelRowPreview(decision.post, t("notification.message.new"));
      await showChannelNotification(channelNotificationTag(id), title, body, path, (p) => navigate(p));
    }
  }

  onMount(() => {
    const off = subscribeChannelReads((account, reads) => {
      onReads(account, reads).catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[feed] channel notification failed", err);
      });
    });
    const onSwMessage = (event: MessageEvent): void => {
      const data = event.data as { type?: unknown; path?: unknown } | null;
      if (data?.type !== OPEN_PATH_MESSAGE || typeof data.path !== "string") return;
      if (data.path.startsWith("/channel/")) navigate(data.path);
    };
    const sw = typeof navigator === "undefined" ? undefined : navigator.serviceWorker;
    sw?.addEventListener("message", onSwMessage);
    onCleanup(() => {
      off();
      sw?.removeEventListener("message", onSwMessage);
    });
  });

  return null;
}
