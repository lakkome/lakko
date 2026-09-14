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
import { forgetDeviceConfirmation } from "../auth/destructive-confirm";
import { requestConfirm } from "./confirm-dialog";
import { cycleTheme, useTheme } from "./theme";
import { autoplayVideoNotes, setAutoplayVideoNotes } from "../media/media-prefs";
import { CallHostView } from "../calls/CallHostContext";

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
    // /recover flow), which is what the body says.
    //
    // ADR-0045 UX §14 — an in-page dialog rather than `window.confirm`: the
    // copy has to read as the app stating a fact about the user's own data,
    // and the action has to name what it acts on. A cancel (or Escape, or a
    // host that never mounted) resolves `false` and leaves the wipe
    // unstarted.
    if (!(await requestConfirm(forgetDeviceConfirmation()))) {
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
        <A href={paths.channels}>Channels</A>
        {/* ADR-0048 UX §7 — Settings → Profile lives in the shell menu. */}
        <A href={paths.profile}>Profile</A>

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
            {/* ADR-0047 UX §3 — incoming video notes autoplay muted only when
                this is on. Default off: autoplay downloads media. */}
            <button
              type="button"
              role="switch"
              aria-checked={autoplayVideoNotes()}
              onClick={() => setAutoplayVideoNotes(!autoplayVideoNotes())}
              style={{ "font-size": "0.85rem" }}
            >
              Autoplay video notes: {autoplayVideoNotes() ? "On" : "Off"}
            </button>
          </div>
        </div>
      </nav>
      <main>{props.children}</main>
      {/* The call UI — the 1x1 ringing banner, the in-call overlay and remote
          audio of a 1x1 or group call, the banner for a group call in a chat
          that is not open — on every authenticated screen, outside the routed
          content so a route change neither hides a ringing call nor unmounts a
          live one. */}
      <CallHostView />
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
        if (s.kind === "session") {
          return s.account.username.length > 0
            ? `Signed in as @${s.account.username}`
            : "Signed in (guest)";
        }
        if (s.kind === "loading") return "Restoring session…";
        return "Anonymous";
      })()}
    </span>
  );
}
