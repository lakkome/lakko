// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Route-level access guards.
 *
 *   - `<RequireSession>` — wraps protected routes. If the user is
 *      anonymous, redirect to the login page; if the auth context
 *      is still resolving, show a small loading placeholder.
 *
 *   - `<RequireAnonymous>` — wraps the login / signup screens.
 *      If the user is already signed in, redirect home so they do
 *      not see a stale form.
 *
 * Both guards are declarative wrappers — pass children through and
 * the consumer never has to think about navigation imperatively.
 *
 * The guards read the real auth state behind `useAuth()` —
 * `AuthContext` runs OPAQUE-PAKE round-trips (1.16c) and rehydrates
 * persisted sessions on mount, so the guards see a genuine
 * `loading` / `anon` / `session` value.
 */
import { Show, type JSX } from "solid-js";
import { Navigate } from "@solidjs/router";
import { useAuth } from "./AuthContext";
import { paths } from "../routes/paths";
import RestoreIdentityPrompt from "./RestoreIdentityPrompt";
import SaveFreshPhrasePrompt from "./SaveFreshPhrasePrompt";

/** Gate every route inside the AppShell behind a real session. */
export function RequireSession(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  return (
    <Show
      when={auth.state().kind === "session"}
      fallback={<UnauthenticatedFallback />}
    >
      <IdentitySecurityBanner />
      {props.children}
    </Show>
  );
}

/**
 * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
 * dismissible security banner shown when the account's published identity
 * did not match this device's at login (someone replaced it; the
 * ACCESS-hijack signature). Renders nothing in the normal case.
 */
function IdentitySecurityBanner(): JSX.Element {
  const auth = useAuth();
  return (
    <Show when={auth.identityAlert() !== null}>
      <div
        role="alert"
        style={{
          background: "var(--danger-bg, #3a1414)",
          color: "var(--danger-fg, #ffb4b4)",
          border: "1px solid var(--danger-border, #c0392b)",
          "border-radius": "8px",
          padding: "0.75rem 1rem",
          margin: "0.5rem",
          display: "flex",
          gap: "0.75rem",
          "align-items": "flex-start",
        }}
      >
        <div style={{ flex: "1" }}>
          <strong>Your account's secure identity was changed.</strong>{" "}
          Someone published a different identity key for this account. If
          this was you (you reset your identity on another device), you can
          dismiss this. If it was NOT you, your account may be compromised —
          re-verify your safety number with your contacts out of band, and
          rotate your password / passkeys.
        </div>
        <button type="button" onClick={() => auth.dismissIdentityAlert()}>
          Dismiss
        </button>
      </div>
    </Show>
  );
}

/** Mirror of `RequireSession` for the public auth pages. */
export function RequireAnonymous(props: {
  children: JSX.Element;
}): JSX.Element {
  const auth = useAuth();
  return (
    <Show
      when={auth.state().kind === "anon"}
      fallback={<AlreadyAuthenticatedFallback />}
    >
      {props.children}
    </Show>
  );
}

function UnauthenticatedFallback(): JSX.Element {
  const auth = useAuth();
  // ADR-0017: an authenticated-but-identity-pending user (fresh
  // device, no cached phrase seed) must NOT be bounced to /login —
  // that would loop (login thinks they're already authenticated).
  // Render the recovery-phrase prompt in place instead. The
  // lost-everything fallback (`save-fresh-phrase`) lands here too —
  // render the phrase-save screen rather than redirect.
  return (
    <Show
      when={
        auth.state().kind !== "needs-recovery-phrase" &&
        auth.state().kind !== "save-fresh-phrase"
      }
      fallback={
        <Show
          when={auth.state().kind === "save-fresh-phrase"}
          fallback={<RestoreIdentityPrompt />}
        >
          <SaveFreshPhrasePrompt />
        </Show>
      }
    >
      {/* While the context is still resolving (it starts in the
          "loading" state on mount while it reads localStorage + IDB)
          we render a placeholder instead of an instant redirect —
          bouncing the user to /login during a half-second
          rehydration is jarring. */}
      <Show
        when={auth.state().kind !== "loading"}
        fallback={<p style={{ padding: "1.5rem" }}>Restoring session…</p>}
      >
        <Navigate href={paths.login} />
      </Show>
    </Show>
  );
}

function AlreadyAuthenticatedFallback(): JSX.Element {
  const auth = useAuth();
  // ADR-0017: the identity-pending state lands here too (the user
  // just submitted the login form, which is wrapped in
  // RequireAnonymous). Render the recovery-phrase prompt rather than
  // an indefinite "Restoring session…". The lost-everything fallback
  // (`save-fresh-phrase`) lands here too (the user clicked
  // "Create a new identity" from within RestoreIdentityPrompt) —
  // render the phrase-save screen.
  return (
    <Show
      when={
        auth.state().kind !== "needs-recovery-phrase" &&
        auth.state().kind !== "save-fresh-phrase"
      }
      fallback={
        <Show
          when={auth.state().kind === "save-fresh-phrase"}
          fallback={<RestoreIdentityPrompt />}
        >
          <SaveFreshPhrasePrompt />
        </Show>
      }
    >
      {/* If the user is "loading", let it resolve before bouncing. */}
      <Show
        when={auth.state().kind === "session"}
        fallback={<p style={{ padding: "1.5rem" }}>Restoring session…</p>}
      >
        <Navigate href={paths.home} />
      </Show>
    </Show>
  );
}
