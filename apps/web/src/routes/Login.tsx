// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Login route — drives a real OPAQUE-PAKE round-trip through
 * `AuthContext.login`. Servers: `lakko-svc-auth` (OPAQUE) +
 * `lakko-svc-discovery` (username → account_id resolution),
 * via Vite's `/api/auth/*` and `/api/discovery/*` dev proxies.
 *
 * 1.16d2 lifted the single-device limitation: the form now
 * resolves the typed username via `/discovery/lookup` when the
 * local cache misses, so a fresh browser can log in to any
 * account whose username is registered. Cache hits (re-login on
 * the same device) skip the lookup round-trip.
 */
import { createSignal, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";

export default function Login(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  // Pre-fill from the device-cached username (1.16c3b). When 1.16d
  // brings cross-device discovery the form will accept arbitrary
  // usernames again; for now the only login candidate on this
  // device is whoever signed up here last.
  const cached = (): string => {
    const s = auth.state();
    return s.kind === "anon" && s.cachedUsername !== undefined
      ? s.cachedUsername
      : "";
  };
  const [username, setUsername] = createSignal(cached());
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await auth.login(username().trim(), password());
      navigate(paths.home, { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="auth-shell">
      <section class="auth-card">
        <h1>Log in to Lakko</h1>
        <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
          OPAQUE-PAKE login + OPRF username lookup. Wrong
          username and wrong password produce the same error —
          by design.
        </p>
        <form onSubmit={onSubmit}>
          <label>
            Username
            <input
              type="text"
              autocomplete="username"
              required
              value={username()}
              onInput={(e) => setUsername(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          <label>
            Password
            <input
              type="password"
              autocomplete="current-password"
              required
              value={password()}
              onInput={(e) => setPassword(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          {error() !== null && <div class="form-error">{error()}</div>}
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Signing in…" : "Log in"}
          </button>
        </form>
        <p class="form-footer">
          New to Lakko? <A href={paths.signup}>Create an account</A>.
        </p>
        <p class="form-footer">
          Lost your password? <A href={paths.recover}>Recover with your 24-word phrase</A>.
        </p>
      </section>
    </div>
  );
}
