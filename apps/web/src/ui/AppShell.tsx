// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Persistent shell for authenticated routes: the navigation rail on the left
 * (Chats · Contacts · Channels · Calls · Settings, the design system's five
 * destinations) and a main pane that renders the active route via
 * `<Outlet />`. On a narrow screen the rail becomes a bottom tab bar.
 *
 * Session actions (log out, forget this device, theme, media preferences)
 * live under Settings, not here: the rail is navigation only.
 */
import type { JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "../routes/paths";
import { useAuth } from "../auth/AuthContext";
import { Avatar } from "./Avatar";
import { Icon, type IconName } from "./icons";

const ITEMS: ReadonlyArray<{ href: string; label: string; icon: IconName; prefix: string[] }> = [
  { href: paths.home, label: "Chats", icon: "chat", prefix: ["/chat/"] },
  { href: paths.contacts, label: "Contacts", icon: "contacts", prefix: [] },
  { href: paths.channels, label: "Channels", icon: "channels", prefix: [] },
  { href: paths.calls, label: "Calls", icon: "calls", prefix: ["/call/"] },
  { href: paths.settings, label: "Settings", icon: "settings", prefix: ["/devices"] },
];

/**
 * Layout wrapper rendered around every protected route. The route content is
 * the `<Outlet />` received as `children` (the `@solidjs/router` v0.16 nested
 * layout pattern).
 */
export function AppShell(props: { children?: JSX.Element }): JSX.Element {
  const auth = useAuth();
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

  return (
    <div class="app-shell">
      <nav class="lk-rail" aria-label="Primary">
        <img class="lk-rail-logo" src="/icon-192.png" alt="Lakko" width="40" height="40" />
        {ITEMS.map((item) => (
          <A
            href={item.href}
            class="lk-rail-item"
            classList={{ active: isActive(item) }}
            inactiveClass=""
            activeClass=""
            aria-current={isActive(item) ? "page" : undefined}
          >
            <Icon name={item.icon} size={22} />
            <span>{item.label}</span>
          </A>
        ))}
        <div class="lk-rail-spacer" />
        <button
          type="button"
          class="lk-rail-me"
          title={username().length > 0 ? `@${username()} · Settings` : "Settings"}
          onClick={() => navigate(paths.settingsSection("profile"))}
        >
          <Avatar label={username().length > 0 ? username() : "?"} id={username()} size="sm" />
        </button>
      </nav>
      <main>{props.children}</main>
      {/* The call UI (`CallHostView`) is mounted by the router's root layout in
          `App.tsx`, not here: a call from a `/c/` link runs on a public route
          outside this shell, and must show the same overlay there. */}
    </div>
  );
}
