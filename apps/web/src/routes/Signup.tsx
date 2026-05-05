// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signup route — drives a real OPAQUE-PAKE registration through
 * `AuthContext.signup` (1.16c3b). Server: `lakko-svc-auth` on
 * `127.0.0.1:8081` via Vite's `/api/auth/*` dev proxy.
 *
 * Generates a fresh UUIDv7 `account_id` client-side, persists
 * `(accountId, username)` in `localStorage` so subsequent logins
 * on this device work. BIP39 recovery passphrase generation lands
 * in 1.16c3c.
 */
import { createSignal, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";

export default function Signup(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    if (password() !== confirm()) {
      setError("Passwords do not match.");
      return;
    }
    if (password().length < 8) {
      // Floor only — 1.16c upgrades this with the real password
      // strength heuristic + Argon2id KSF cost feedback.
      setError("Password must be at least 8 characters.");
      return;
    }
    setBusy(true);
    try {
      await auth.signup(username().trim(), password());
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
        <h1>Create a Lakko account</h1>
        <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
          OPAQUE-PAKE registration. The password never leaves your
          browser — the server only sees an OPRF-blinded envelope.
          BIP39 recovery passphrase generation arrives in 1.16c3c.
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
              autocomplete="new-password"
              required
              value={password()}
              onInput={(e) => setPassword(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          <label>
            Confirm password
            <input
              type="password"
              autocomplete="new-password"
              required
              value={confirm()}
              onInput={(e) => setConfirm(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          {error() !== null && <div class="form-error">{error()}</div>}
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Creating account…" : "Sign up"}
          </button>
        </form>
        <p class="form-footer">
          Already have an account? <A href={paths.login}>Log in</A>.
        </p>
      </section>
    </div>
  );
}
