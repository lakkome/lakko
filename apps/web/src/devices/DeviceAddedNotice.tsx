// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device was added to your account" (docs/ux/identity-recovery.md
 * section 17.5, amended 2026-10-29): the watcher that folds the DeviceGroup
 * roster and the account's synced acknowledgements into the per-device
 * notice state and keeps the system notification in step with it, and the
 * explanation drawn above the list in Settings > Devices. Every rule is in
 * `added-notice.ts`. The banner at the top of the Chats list is gone.
 */
import { createEffect, onCleanup, Show, untrack, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { t } from "../l10n";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import {
  addedNotice,
  bindAddedNotice,
  noticeTag,
  notificationAction,
  recordAcknowledged,
  recordRoster,
  badgeVisible,
  type AddedNoticeView,
} from "./added-notice";
import { useDeviceGroupSync } from "./sync/DeviceGroupSyncContext";
import { isDesktop } from "../platform";
import { showHostNotification, withdrawHostNotification } from "../platform/host";
import { displayLocale } from "../l10n/display-locale";

/** The message the service worker posts when its copy of the notification
 *  is tapped (`public/sw.js`, `data.lakkoDevicesNotice`). */
const OPEN_DEVICES_MESSAGE = "LAKKO_OPEN_DEVICES";

function notificationPermission(): "granted" | "denied" | "default" | "unsupported" {
  // The desktop app shows it through the host (ADR-0085): the webview has no
  // Notification API, and the OS asks for permission itself.
  if (isDesktop()) return "granted";
  return typeof Notification === "undefined" ? "unsupported" : Notification.permission;
}

/** The page's own handle on the notification it posted, by tag. */
const shownHere = new Map<string, Notification>();

async function swRegistration(): Promise<ServiceWorkerRegistration | undefined> {
  try {
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return undefined;
    return await navigator.serviceWorker.getRegistration();
  } catch {
    return undefined;
  }
}

/** Post (or replace) the account's notification. It names no device and no
 *  place; tapping it opens Settings > Devices. */
async function postNotice(tag: string, view: AddedNoticeView, openDevices: () => void): Promise<void> {
  const title = t(view.titleKey);
  const body = t("devices.added_notice.notification_body");
  if (isDesktop()) {
    // A click opens Settings > Devices through the host's `open-target`.
    await showHostNotification({ tag, title, body, target: paths.devices });
    return;
  }
  try {
    const n = new Notification(title, { body, tag });
    n.onclick = () => {
      window.focus();
      openDevices();
      n.close();
    };
    shownHere.get(tag)?.close();
    shownHere.set(tag, n);
    return;
  } catch {
    // Some browsers (Chrome on Android) only allow notifications from a
    // service worker: show it there, with the same tag.
  }
  const reg = await swRegistration();
  if (reg === undefined) return;
  try {
    await reg.showNotification(title, { body, tag, data: { lakkoDevicesNotice: true } });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("device-added notification not shown", err instanceof Error ? err.name : "");
  }
}

/** Withdraw the account's notification wherever it was shown from. */
async function withdrawNotice(tag: string): Promise<void> {
  if (isDesktop()) {
    await withdrawHostNotification(tag);
    return;
  }
  shownHere.get(tag)?.close();
  shownHere.delete(tag);
  const reg = await swRegistration();
  if (reg === undefined) return;
  try {
    for (const n of await reg.getNotifications({ tag })) n.close();
  } catch {
    // best-effort
  }
}

/** Mounted once in the app shell. Draws nothing. */
export function DeviceAddedWatcher(): JSX.Element {
  const auth = useAuth();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();
  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };
  const openDevices = (): void => navigate(paths.devices);

  createEffect(() => bindAddedNotice(accountId()));

  /** Act on one fold: post when devices became pending, withdraw when
   *  nothing is pending any more. */
  function afterFold(account: string, added: number, pendingBefore: number): void {
    const view = untrack(addedNotice);
    const action = notificationAction({
      added,
      pendingBefore,
      pendingAfter: view.newKeys.length,
      permission: notificationPermission(),
    });
    if (action === "post") void postNotice(noticeTag(account), view, openDevices);
    else if (action === "withdraw") void withdrawNotice(noticeTag(account));
  }

  // The account's acknowledgements (from this device or another one).
  createEffect(() => {
    const account = accountId();
    const acked = sync.acknowledgedDevices();
    if (account === null) return;
    const before = untrack(addedNotice).newKeys.length;
    recordAcknowledged(account, acked);
    afterFold(account, 0, before);
  });

  // The roster: a device this device has not seen yet becomes pending.
  createEffect(() => {
    const account = accountId();
    const roster = sync.deviceRoster();
    if (account === null || roster === null) return;
    const before = untrack(addedNotice).newKeys.length;
    const added = recordRoster(account, roster, Date.now());
    afterFold(account, added, before);
  });

  // The service worker's copy of the notification was tapped.
  if (typeof navigator !== "undefined" && "serviceWorker" in navigator) {
    const onMessage = (ev: MessageEvent): void => {
      const data = ev.data as { type?: unknown } | null;
      if (data !== null && typeof data === "object" && data.type === OPEN_DEVICES_MESSAGE) openDevices();
    };
    navigator.serviceWorker.addEventListener("message", onMessage);
    onCleanup(() => navigator.serviceWorker.removeEventListener("message", onMessage));
  }

  return <></>;
}

/**
 * The explanation above the list in Settings > Devices (17.5 surface 3):
 * title, when, and what to do if it was not you. No button: this screen is
 * where the `Remove` button is, and opening it is the acknowledgement. Read
 * as the first element of the screen; never a live region, never focused.
 */
export function DeviceAddedExplanation(props: { readonly view: AddedNoticeView | null }): JSX.Element {
  const when = (): string => {
    const at = props.view?.latestAtMs ?? null;
    if (at === null) return "";
    return new Date(at).toLocaleString(displayLocale(), {
      dateStyle: "medium",
      timeStyle: "short",
    });
  };
  return (
    <Show when={props.view !== null && props.view.visible}>
      <div class="lk-alert accent lk-device-added">
        <span aria-hidden="true">
          <Icon name="devices" size={18} />
        </span>
        <div class="lk-fill">
          <strong>{t(props.view?.titleKey ?? "devices.added_notice.title")}</strong>
          <div>{t("devices.added_notice.when", { date: when() })}</div>
          <div>{t("devices.added_notice.not_you")}</div>
        </div>
      </div>
    </Show>
  );
}

/** The exclamation-mark badge (17.5 surface 2): warning colour, never colour
 *  alone (the host element carries the accessible name). */
export function AttentionBadge(): JSX.Element {
  return (
    <span class="lk-attention-badge" aria-hidden="true">
      !
    </span>
  );
}

/** Whether a device added to the account is waiting to be seen: the
 *  condition for every "!" badge. */
export function devicesNeedAttention(): boolean {
  return badgeVisible(addedNotice());
}

/** The badge on the `Devices` row inside Settings, while something is
 *  pending. The row's link carries `settings.devices.attention_a11y`. */
export function DevicesAttention(): JSX.Element {
  return (
    <Show when={devicesNeedAttention()}>
      {" "}
      <AttentionBadge />
    </Show>
  );
}
