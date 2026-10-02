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
 * What either guard renders is decided by `phrase-gate.ts`, which also holds
 * the rule that an unacknowledged signup recovery phrase is shown before ANY
 * route — the anonymous guard used to navigate home on the very transition
 * that produced the phrase, so it was never seen.
 *
 * The guards read the real auth state behind `useAuth()`, which rehydrates
 * a persisted session on mount, so they see a genuine `loading` / `anon` /
 * `session` value.
 */
import { Match, Show, Switch, type JSX } from "solid-js";
import { Navigate, useLocation } from "@solidjs/router";
import { useAuth } from "./AuthContext";
import { paths } from "../routes/paths";
import { t } from "../l10n";
import RestoreIdentityPrompt from "./RestoreIdentityPrompt";
import SaveFreshPhrasePrompt from "./SaveFreshPhrasePrompt";
import SignupPhrasePrompt from "./SignupPhrasePrompt";
import { GuestAccountScreen } from "./GuestAccountOffer";
import { readReturnUrl } from "./return-url";
import {
  anonymousGuardView,
  sessionGuardView,
  type GuardView,
} from "./phrase-gate";

/** Gate every route inside the AppShell behind a real session. */
export function RequireSession(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const view = (): GuardView =>
    sessionGuardView(auth.state().kind, auth.signupPhrase() !== null, auth.isGuest());
  return (
    <Show when={view() === "children"} fallback={<GuardFallback view={view()} />}>
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
      <div class="lk-alert danger lk-identity-alert" role="alert">
        <div class="lk-fill">
          <strong>{t("security.identity_changed.title")}</strong>{" "}
          {t("security.identity_changed.body")}
        </div>
        <button type="button" class="small" onClick={() => auth.dismissIdentityAlert()}>
          {t("common.dismiss")}
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
  const view = (): GuardView =>
    anonymousGuardView(auth.state().kind, auth.signupPhrase() !== null);
  return (
    <Show when={view() === "children"} fallback={<GuardFallback view={view()} />}>
      {props.children}
    </Show>
  );
}

/**
 * Everything a guard renders instead of its route.
 *
 * - `loading`: a placeholder rather than an instant redirect — bouncing the
 *   user to /login during a half-second rehydration is jarring.
 * - `restore-identity` / `save-fresh-phrase`: ADR-0017 — an authenticated
 *   user whose identity is not yet on this device, or who just minted a new
 *   one, must not be bounced (that would loop).
 * - `save-signup-phrase`: the signup recovery phrase, until acknowledged.
 *   Clearing it re-runs the decision: on a public auth page that is
 *   `redirect-home`, which honours `?return=`; inside the app the route
 *   underneath simply renders.
 * - `guest`: a guest account (`guest-account.ts`) never lands in the app; it
 *   is sent back to its call, or asked whether to keep the account.
 */
function GuardFallback(props: { view: GuardView }): JSX.Element {
  const location = useLocation();
  return (
    <Switch>
      <Match when={props.view === "loading"}>
        <p class="lk-muted" style={{ padding: "24px" }}>{t("common.loading")}</p>
      </Match>
      <Match when={props.view === "restore-identity"}>
        <RestoreIdentityPrompt />
      </Match>
      <Match when={props.view === "save-fresh-phrase"}>
        <SaveFreshPhrasePrompt />
      </Match>
      <Match when={props.view === "save-signup-phrase"}>
        <SignupPhrasePrompt />
      </Match>
      <Match when={props.view === "guest"}>
        <GuestAccountScreen />
      </Match>
      <Match when={props.view === "redirect-login"}>
        <Navigate href={paths.login} />
      </Match>
      <Match when={props.view === "redirect-home"}>
        {/* A public auth page reached with `?return=<path>` (1.18e — an invite
            landing's "Log in" / "Sign up") continues there once signed in —
            including after the signup phrase is acknowledged. Validated
            path-only by `readReturnUrl`; `/` when absent or unsafe. */}
        <Navigate href={readReturnUrl(location)} />
      </Match>
    </Switch>
  );
}
