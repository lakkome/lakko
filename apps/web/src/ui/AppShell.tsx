// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Persistent shell for authenticated routes — sidebar with primary
 * navigation + a main pane that renders the active route via
 * `<Outlet />`. Lives outside `routes/` because it is structural
 * UI rather than a route per se.
 *
 * Navigation links are wired (1.16b). Contacts is fully implemented
 * (OPRF discovery + invite flows, 1.16d). The Home landing is still
 * a placeholder shell pending the inbox / conversation-list view.
 */
import type { JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "../routes/paths";
import { useAuth } from "../auth/AuthContext";
import { cycleTheme, useTheme } from "./theme";

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
    // Keeps localStorage + IDB intact — re-login on this device
    // remains cheap, and the device key stays usable for any
    // encrypted local stores.
    auth.logout();
    navigate(paths.login, { replace: true });
  }

  async function onForgetDevice(): Promise<void> {
    // Confirm — this is destructive. After this the account is
    // only reachable via the recovery passphrase (1.16c3c3
    // /recover flow).
    if (
      !window.confirm(
        "Forget this device? You'll lose the cached account on " +
          "this browser. The only way back is the 24-word " +
          "recovery passphrase you wrote down at signup.",
      )
    ) {
      return;
    }
    await auth.forgetDevice();
    navigate(paths.login, { replace: true });
  }

  // Solid's `<A>` adds an `active` class when the current location
  // matches its `href` prop. The default match is "starts with",
  // which would match `/contacts` from `/`; passing `end` switches
  // to exact match.
  //
  // 1.18e — DevProbe was a sidebar entry through 1.16-1.18 but
  // it's a smoke surface, not a user-facing page (renders raw
  // 8-byte pubkey fingerprints + KeyPackage byte counts). The
  // route at `/dev` stays reachable by URL for the maintainer's
  // self-debugging; the sidebar link is gone so end-users don't
  // wander into it.
  return (
    <div class="app-shell">
      <nav aria-label="Primary">
        <A href={paths.home} end>
          Home
        </A>
        <A href={paths.contacts}>Contacts</A>

        <div class="nav-spacer" />
        <div class="nav-footer">
          <SessionLabel />
          <div
            style={{
              "margin-top": "0.5rem",
              display: "flex",
              "flex-direction": "column",
              gap: "0.5rem",
            }}
          >
            <button type="button" onClick={onLogout}>
              Log out
            </button>
            <button
              type="button"
              onClick={() => {
                void onForgetDevice();
              }}
              style={{ "font-size": "0.85rem" }}
            >
              Forget this device
            </button>
            {/* 1.18f — theme toggle cycles system → dark → light
                → system. Label echoes the *current* state so the
                user sees what's active. */}
            <button
              type="button"
              onClick={cycleTheme}
              style={{ "font-size": "0.85rem" }}
              title="Cycle theme (system / dark / light)"
            >
              Theme: {useTheme()}
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
