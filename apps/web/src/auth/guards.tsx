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

/** Gate every route inside the AppShell behind a real session. */
export function RequireSession(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  return (
    <Show
      when={auth.state().kind === "session"}
      fallback={<UnauthenticatedFallback />}
    >
      {props.children}
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
  // While the context is still resolving (it starts in the
  // "loading" state on mount while it reads localStorage + IDB) we
  // render a placeholder instead of an instant redirect — bouncing
  // the user to /login during a half-second rehydration is jarring.
  return (
    <Show
      when={auth.state().kind !== "loading"}
      fallback={<p style={{ padding: "1.5rem" }}>Restoring session…</p>}
    >
      <Navigate href={paths.login} />
    </Show>
  );
}

function AlreadyAuthenticatedFallback(): JSX.Element {
  // If the user is "loading", let it resolve before bouncing them.
  const auth = useAuth();
  return (
    <Show
      when={auth.state().kind === "session"}
      fallback={<p style={{ padding: "1.5rem" }}>Restoring session…</p>}
    >
      <Navigate href={paths.home} />
    </Show>
  );
}
