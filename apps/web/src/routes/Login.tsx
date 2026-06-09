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
import { createSignal, Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { readReturnUrl } from "../auth/return-url";
import { WebauthnClient } from "../auth/webauthn-client";

export default function Login(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const location = useLocation();

  // Pre-fill from the device-cached username (1.16c3b) as a
  // convenience. The form accepts arbitrary usernames — 1.16d2
  // wired cross-device discovery, so `AuthContext.login` resolves
  // any registered username via `/discovery/lookup` on a cache
  // miss. The prefill is just the last account seen on this device.
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
      // 1.18e — honour `?return=<path>` from the URL so an anon
      // visitor who clicked "Log in" from the invite landing
      // (`/i/:slug`) lands back there after auth. Validated
      // path-only by `readReturnUrl`; falls back to `/` when
      // the param is absent, malformed, or an open-redirect
      // attempt.
      navigate(readReturnUrl(location), { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onPasskeyLogin(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.loginWithPasskey();
      navigate(readReturnUrl(location), { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  // 2a2c2 — show the passkey affordance only when the browser
  // supports WebAuthn AND the user has logged in here before
  // (the export-key cache is populated). The "logged in here
  // before" check is a coarse heuristic — we use the cached
  // (accountId, username) marker that the prior signup / login
  // / recover would have written to localStorage. A real cache
  // miss (key store cleared, ciphertext tampered) surfaces
  // later as a clean error message from `loginWithPasskey`.
  const passkeyAvailable = (): boolean => {
    if (!WebauthnClient.isWebauthnAvailable()) return false;
    const s = auth.state();
    return s.kind === "anon" && s.cachedUsername !== undefined;
  };

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
        <Show when={passkeyAvailable()}>
          <div style={{ margin: "0.75rem 0", "text-align": "center" }}>
            <button
              type="button"
              disabled={busy()}
              onClick={() => void onPasskeyLogin()}
              style={{ "min-width": "60%" }}
            >
              Sign in with passkey
            </button>
            <p
              class="form-footer"
              style={{ "font-size": "0.85em", "margin-top": "0.25rem" }}
            >
              Uses the passkey you registered earlier on this device.
            </p>
          </div>
        </Show>
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
