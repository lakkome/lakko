// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Persistent shell for authenticated routes: the navigation rail on the left
 * (Chats · Groups · Channels · Contacts · Settings, the five destinations of
 * docs/ux/client-redesign.md section 3) and a main pane that renders the
 * active route via `<Outlet />`. Below 720 px the rail becomes a bottom tab
 * bar.
 *
 * Session actions (log out, forget this device, theme, media preferences)
 * live under Settings, not here: the rail is navigation only. Calls is a
 * destination again (`docs/ux/navigation.md` section 5), and a call link is
 * offered from inside a call rather than built anywhere in the rail
 * (`docs/ux/call-links.md` section 3).
 */
import { Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "../routes/paths";
import { useAuth } from "../auth/AuthContext";
import { DeviceRequestPrompt } from "../devices/DeviceRequestPrompt";
import { AttentionBadge, DeviceAddedWatcher } from "../devices/DeviceAddedNotice";
import { addedNotice, badgeVisible } from "../devices/added-notice";
import { RemovedDeviceScreen } from "../devices/RemovedDeviceScreen";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { Avatar } from "./Avatar";
import { OWN_AVATAR_KEY } from "../profile/avatar-runtime";
import { Icon, type IconName } from "./icons";
import { t } from "../l10n";

/**
 * `docs/ux/navigation.md` section 1 - four destinations, not five.
 *
 * Groups and Channels are gone as destinations: they were three places for
 * one activity, and a list sorted by when cannot be three lists. Both remain
 * as kinds of conversation inside Chats, so their screens stay reachable and
 * count as Chats while open - which is what the `prefix` list is for.
 *
 * Calls is back. It was removed on the recorded ground that it held no
 * history, only a create-link panel; that was true of what existed and was
 * never an argument against call history, and the design system's own
 * information architecture kept it as destination 4 throughout.
 */
const ITEMS: ReadonlyArray<{ href: string; labelKey: string; icon: IconName; prefix: string[] }> = [
  // A group conversation still lives under `/groups/<id>`, so that prefix
  // keeps Chats lit while one is open - the list is one list now, and which
  // URL a conversation happens to sit at is not something the rail should
  // show.
  { href: paths.home, labelKey: "nav.chats", icon: "chat", prefix: ["/chat/", "/groups", "/channels"] },
  // `docs/ux/feed.md` section 2 - second, one tap from the home.
  { href: paths.feed, labelKey: "nav.feed", icon: "feed", prefix: [] },
  { href: paths.contacts, labelKey: "nav.contacts", icon: "contacts", prefix: [] },
  { href: paths.calls, labelKey: "nav.calls", icon: "calls", prefix: [] },
  { href: paths.settings, labelKey: "nav.settings", icon: "settings", prefix: ["/devices"] },
];

/**
 * Layout wrapper rendered around every protected route. The route content is
 * the `<Outlet />` received as `children` (the `@solidjs/router` v0.16 nested
 * layout pattern).
 */
export function AppShell(props: { children?: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const sync = useDeviceGroupSync();
  const navigate = useNavigate();
  const location = useLocation();

  const username = (): string => {
    const s = auth.state();
    return s.kind === "session" ? s.account.username : "";
  };

  // Chats owns `/` and `/chat/*`; a plain prefix match on `/` would light it
  // up everywhere, so every item decides its own active state here.
  const isActive = (item: (typeof ITEMS)[number]): boolean => {
    const p = location.pathname;
    if (item.href === paths.home) {
      return p === "/" || item.prefix.some((x) => p.startsWith(x));
    }
    return p === item.href || p.startsWith(`${item.href}/`) || item.prefix.some((x) => p.startsWith(x));
  };

  // docs/ux/identity-recovery.md 17.5: a device joined the account and no
  // device of it has looked at Settings > Devices since.
  const attention = (item: (typeof ITEMS)[number]): boolean =>
    item.href === paths.settings && badgeVisible(addedNotice());

  // docs/ux/second-device.md section 3.1: another device removed this one.
  // The removed screen replaces the whole app, rail and prompts included.
  return (
    <Show when={!sync.deviceRemoved()} fallback={<RemovedDeviceScreen />}>
    <div class="app-shell">
      <nav class="lk-rail" aria-label={t("nav.rail_a11y")}>
        <img class="lk-rail-logo" src="/icon-192.png" alt="Lakko" width="40" height="40" />
        {ITEMS.map((item) => (
          <A
            href={item.href}
            class="lk-rail-item"
            classList={{ active: isActive(item) }}
            inactiveClass=""
            activeClass=""
            aria-current={isActive(item) ? "page" : undefined}
            aria-label={attention(item) ? t("nav.settings.attention_a11y") : undefined}
          >
            <span class="lk-rail-icon">
              <Icon name={item.icon} size={22} />
              <Show when={attention(item)}>
                <AttentionBadge />
              </Show>
            </span>
            <span>{t(item.labelKey)}</span>
          </A>
        ))}
        <div class="lk-rail-spacer" />
        <button
          type="button"
          class="lk-rail-me"
          title={username().length > 0 ? `@${username()} · ${t("nav.settings")}` : t("nav.settings")}
          onClick={() => navigate(paths.settingsSection("profile"))}
        >
          <Avatar label={username().length > 0 ? username() : "?"} id={username()} size="sm" personKey={OWN_AVATAR_KEY} />
        </button>
      </nav>
      <main>{props.children}</main>
      {/* ADR-0049 section 2: the "Is this you?" prompt for a new device
          asking to be confirmed without a camera. */}
      <DeviceRequestPrompt />
      {/* docs/ux/identity-recovery.md 17.5: watches the account's device
          list for a device this one has not acknowledged. */}
      <DeviceAddedWatcher />
      {/* The call UI (`CallHostView`) is mounted by the router's root layout in
          `App.tsx`, not here: a call from a `/c/` link runs on a public route
          outside this shell, and must show the same overlay there. */}
    </div>
    </Show>
  );
}
