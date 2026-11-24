// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Join-request notifications and the unshown count
 * (`docs/ux/group-and-channel-privacy.md` section 8.1): hears every applied
 * poll of a group (`join-request-reads.ts`, from the open conversation and
 * from the background receive alike), prunes what this device keeps to the
 * requests still waiting (or drops it once the device is no longer owner or
 * admin, or no longer a member), and, for a poll that read the knock queue,
 * for each new request asks
 * the rule (`join-request-notices.ts`) whether it is announced, counted as
 * not shown, and notified.
 *
 * Mounted once in the router's root layout (a tap navigates). The
 * notification goes the way the channel post notification goes
 * (`feed/ChannelPostNotifier.tsx`): through the host in the desktop app,
 * else the browser's Notification API, else the service worker. Title
 * `Request to join`; body names the joiner and the group as its Chats row
 * shows it; tag per group and request; a tap opens the group's info at
 * `Join requests`. A request does not wake a closed app: the notification
 * comes when this device next reads the group's request queue.
 */
import { onCleanup, onMount, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { useAuth } from "../auth/AuthContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { OPEN_PATH_MESSAGE } from "../feed/ChannelPostNotifier";
import { t } from "../l10n";
import { isDesktop } from "../platform";
import { showHostNotification } from "../platform/host";
import { useProfiles } from "../profile/ProfileContext";
import { memberNameOf } from "../profile/member-label";
import { paths } from "../routes/paths";
import { useChats, type ActiveGroup } from "./ChatContext";
import { conversationKindOf } from "./conversation-kind";
import { conversationTitle } from "./conversation-title";
import { useGroupNames } from "./GroupNameContext";
import { groupTitle } from "./group-title";
import {
  joinRequestBody,
  joinRequestNotificationTag,
  joinRequestsPath,
  joinNoticesAfterRead,
  onJoinRequestArrived,
} from "./join-request-notices";
import { pruneKnockOrigins } from "./knock-origins";
import { updateJoinNotices } from "./join-request-notices-store";
import { subscribeJoinRequestReads, type JoinRequestRead } from "./join-request-reads";
import { pendingJoinRealName } from "./pending-join-label";
import { ownPersonKeyB64, personKeysOf } from "./person-key";
import { useLineage } from "./use-lineage";

/** A path this notifier opens: a conversation's info at `Join requests`. */
const JOIN_PATH_RE = /^\/(chat|groups)\/[A-Za-z0-9_-]{1,128}\?info=group&section=join-requests$/;

function notificationsAllowed(): boolean {
  // The desktop host shows them; the OS asks for permission itself.
  if (isDesktop()) return true;
  return typeof Notification !== "undefined" && Notification.permission === "granted";
}

async function showJoinNotification(
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
    console.warn("[chat] join request notification not shown", err instanceof Error ? err.name : "");
  }
}

export function JoinRequestNotifier(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const sync = useDeviceGroupSync();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const lineage = useLineage();
  const navigate = useNavigate();
  const location = useLocation();

  /** The conversation's two routes (Chats and Groups list). */
  const routesOf = (g: ActiveGroup): string[] => {
    const slugs = new Set([
      base64UrlEncodeNoPad(lineage.conversationIdOf(g.group.groupId)),
      base64UrlEncodeNoPad(g.group.groupId),
    ]);
    const out: string[] = [];
    for (const s of slugs) out.push(paths.chat(s), paths.groupChat(s));
    return out;
  };

  /** The conversation is open in front of the person. */
  const inFront = (g: ActiveGroup): boolean =>
    typeof document !== "undefined" &&
    document.visibilityState === "visible" &&
    document.hasFocus() &&
    routesOf(g).includes(location.pathname);

  /** The group's title as its Chats row shows it (the row's rule for a
   *  group: `conversation-rows.ts` `chatLabel`). */
  const titleOf = (g: ActiveGroup): string => {
    const ownB64 = ownPersonKeyB64(chats.identity());
    return conversationTitle({
      kind: conversationKindOf(g.group),
      memberKeysB64: personKeysOf(g.group),
      ownKeyB64: ownB64,
      fallbackLabel: g.label,
      sharedName: groupNames.nameOf(g.groupIdB64),
      contactNameById: undefined,
      contactNameByMember: undefined,
      nameOf: (k) => memberNameOf(profiles.resolve(k), "a join request notification"),
      groupTitle,
      handle: groupNames.handleOf(g.groupIdB64)?.handle ?? null,
    });
  };

  function onRead(read: JoinRequestRead): void {
    const s = auth.state();
    if (s.kind !== "session" || s.account.accountId !== read.accountIdB64) return;
    const g = chats.get(read.groupIdB64);
    // Every applied poll: keep only the requests still waiting, or drop the
    // group's sets once this device is no longer owner or admin, or no longer
    // a member. Only a poll that read the knock queue announces.
    updateJoinNotices(read.accountIdB64, read.groupIdB64, (cur) => joinNoticesAfterRead(cur, read));
    // UX section 8 — the recorded "Asked / Invited ... ago" of a request that
    // stopped waiting is dropped, so a later request is dated by its arrival.
    void pruneKnockOrigins(read.groupIdB64, read.manages && read.active ? read.pendingKeysB64 : []);
    if (g === undefined || !read.knocksRead || !read.manages || !read.active) return;
    const pending = read.pendingKeysB64;
    const conversationHex = lineage.conversationHexOf(g.group.groupId);
    const muted = sync.flags()[conversationHex]?.muted === true;
    const allowed = notificationsAllowed();
    const front = inFront(g);
    const toNotify: string[] = [];
    for (const key of read.arrivedKeysB64) {
      if (!pending.includes(key)) continue;
      updateJoinNotices(read.accountIdB64, read.groupIdB64, (cur) => {
        const out = onJoinRequestArrived(cur, key, { openAndVisible: front, muted, notificationsAllowed: allowed });
        if (out.notify) toNotify.push(key);
        return out.sets;
      });
    }
    if (toNotify.length === 0) return;
    const groupTitleText = titleOf(g);
    const groupSlug = base64UrlEncodeNoPad(lineage.conversationIdOf(g.group.groupId));
    const path = joinRequestsPath(paths.groupChat(groupSlug));
    for (const key of toNotify) {
      const leaf = base64Decode(key);
      const name = pendingJoinRealName(g.group, leaf, base64Encode, (k) => profiles.resolve(k));
      const body = joinRequestBody(name, groupTitleText);
      const text = t(body.key, body.params);
      void showJoinNotification(
        joinRequestNotificationTag(groupSlug, base64UrlEncodeNoPad(leaf)),
        t("notification.join_request.title"),
        text,
        path,
        (p) => navigate(p),
      );
    }
  }

  onMount(() => {
    const off = subscribeJoinRequestReads(onRead);
    const onSwMessage = (event: MessageEvent): void => {
      const data = event.data as { type?: unknown; path?: unknown } | null;
      if (data?.type !== OPEN_PATH_MESSAGE || typeof data.path !== "string") return;
      if (JOIN_PATH_RE.test(data.path)) navigate(data.path);
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
