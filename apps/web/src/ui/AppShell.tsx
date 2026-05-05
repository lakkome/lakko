// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Persistent shell for authenticated routes — sidebar with primary
 * navigation + a main pane that renders the active route via
 * `<Outlet />`. Lives outside `routes/` because it is structural
 * UI rather than a route per se.
 *
 * Phase 1.16b: navigation links are wired but every destination
 * (Home, Contacts) is a stub. Real content arrives in 1.16d / 1.16e.
 */
import type { JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "../routes/paths";
import { useAuth } from "../auth/AuthContext";

/**
 * Layout wrapper rendered around every protected route. The actual
 * route content is rendered by the `<Outlet />` we receive as
 * `children` — this is the standard `@solidjs/router` v0.16 pattern
 * for nested layouts.
 */
export function AppShell(props: { children?: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  function onLogout(): void {
    auth.logout();
    navigate(paths.login, { replace: true });
  }

  // Solid's `<A>` adds an `active` class when the current location
  // matches its `href` prop. The default match is "starts with",
  // which would match `/contacts` from `/`; passing `end` switches
  // to exact match.
  return (
    <div class="app-shell">
      <nav aria-label="Primary">
        <A href={paths.home} end>
          Home
        </A>
        <A href={paths.contacts}>Contacts</A>
        <A href={paths.dev}>Dev probe</A>

        <div class="nav-spacer" />
        <div class="nav-footer">
          <SessionLabel />
          <div style={{ "margin-top": "0.5rem" }}>
            <button type="button" onClick={onLogout}>
              Log out
            </button>
          </div>
        </div>
      </nav>
      <main>{props.children}</main>
    </div>
  );
}

/** Tiny presentational helper — shows the active username, or a
 * placeholder while the auth context is still loading. */
function SessionLabel(): JSX.Element {
  const auth = useAuth();
  return (
    <span>
      {(() => {
        const s = auth.state();
        if (s.kind === "session") return `Signed in as @${s.account.username}`;
        if (s.kind === "loading") return "Restoring session…";
        return "Anonymous";
      })()}
    </span>
  );
}
