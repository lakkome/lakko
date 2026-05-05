// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Login route — shows a username + password form and drives the
 * stub `auth.login(...)` from `AuthContext`.
 *
 * Phase 1.16b: the form is real (controlled inputs, error
 * surfacing, basic disabled-while-busy state) but the auth call
 * itself is a placeholder that always succeeds. Phase 1.16c
 * replaces the placeholder with the real OPAQUE-PAKE round-trip
 * through `lakko-ffi-wasm`.
 */
import { createSignal, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";

export default function Login(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [username, setUsername] = createSignal("");
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
          Phase 1.16b skeleton — credentials are <em>not</em> verified yet;
          1.16c wires the real OPAQUE-PAKE flow.
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
      </section>
    </div>
  );
}
