// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "A device was added to your account" (docs/ux/identity-recovery.md
 * section 17.5): the watcher that folds the DeviceGroup roster into the
 * per-device notice state, and the banner drawn at the top of the Chats
 * list and of Settings > Devices. Every rule is in `added-notice.ts`.
 */
import { createEffect, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { currentLanguage, t } from "../l10n";
import { paths } from "../routes/paths";
import { Icon } from "../ui/icons";
import {
  addedNotice,
  bindAddedNotice,
  dismissAddedNotice,
  recordRoster,
  shouldNotify,
} from "./added-notice";
import { useDeviceGroupSync } from "./sync/DeviceGroupSyncContext";

/** Mounted once in the app shell. Draws nothing. */
export function DeviceAddedWatcher(): JSX.Element {
  const auth = useAuth();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();
  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  createEffect(() => bindAddedNotice(accountId()));

  createEffect(() => {
    const account = accountId();
    const roster = sync.deviceRoster();
    if (account === null || roster === null) return;
    const added = recordRoster(account, roster, Date.now());
    const permission =
      typeof Notification === "undefined" ? "unsupported" : Notification.permission;
    if (
      !shouldNotify({
        added,
        hidden: typeof document !== "undefined" && document.visibilityState === "hidden",
        permission,
      })
    ) {
      return;
    }
    // The app is running but not in front: a local notification. It names
    // no device and no place.
    try {
      const n = new Notification(t(addedNotice().titleKey), {
        body: t("devices.added_notice.notification_body"),
        tag: "lakko-device-added",
      });
      n.onclick = () => {
        window.focus();
        navigate(paths.devices);
        n.close();
      };
    } catch (err) {
      // Some browsers only allow notifications from a service worker; the
      // banner is there at the next look either way.
      // eslint-disable-next-line no-console
      console.warn("device-added notification not shown", err instanceof Error ? err.name : "");
    }
  });

  return <></>;
}

/** The banner. `onDevices`: this is the Devices screen itself, so `Open
 *  Devices` is not offered. */
export function DeviceAddedBanner(props: { readonly onDevices?: boolean }): JSX.Element {
  const auth = useAuth();
  const when = (): string => {
    const at = addedNotice().latestAtMs;
    if (at === null) return "";
    return new Date(at).toLocaleString(currentLanguage(), {
      dateStyle: "medium",
      timeStyle: "short",
    });
  };
  function dismiss(): void {
    const s = auth.state();
    if (s.kind === "session") dismissAddedNotice(s.account.accountId);
  }
  return (
    <Show when={addedNotice().visible}>
      {/* Polite: announced once when it appears, never takes focus. */}
      <div class="lk-alert accent lk-device-added" role="status" aria-live="polite">
        <span aria-hidden="true">
          <Icon name="devices" size={18} />
        </span>
        <div class="lk-fill">
          <strong>{t(addedNotice().titleKey)}</strong>
          <div>{t("devices.added_notice.when", { date: when() })}</div>
          <div>{t("devices.added_notice.not_you")}</div>
        </div>
        <Show when={props.onDevices !== true}>
          {/* A plain link (the router handles it): the banner is drawn
              inside lists that tests mount without a router. */}
          <a class="lk-button-link" href={paths.devices}>
            {t("devices.added_notice.open_devices")}
          </a>
        </Show>
        <button type="button" class="small ghost" onClick={dismiss}>
          {t("common.dismiss")}
        </button>
      </div>
    </Show>
  );
}
