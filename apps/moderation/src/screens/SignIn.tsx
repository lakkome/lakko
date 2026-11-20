// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Sign-in (UX section 5.1): one touch of the moderator's security key. The
 * same touch gives the report secret, which stays in this tab's memory.
 */

import { type JSX, Show, createSignal } from "solid-js";

import logoUrl from "../../../web/public/icon-192.png";
import { errorMessage } from "../api/errors";
import { setSession } from "../session";
import { BusyButton } from "../ui";
import { signInWithKey } from "../webauthn";

export function SignInScreen(): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const signIn = async (): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      setSession(await signInWithKey());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth-shell">
      <section class="auth-brand">
        <div class="auth-brand-logo">
          <img src={logoUrl} alt="" />
          Lakko
        </div>
        <div>
          <h2>Moderation</h2>
          <p>Review reports, decide cases and answer appeals. Every decision is published in the moderation log.</p>
        </div>
        <div class="auth-brand-foot">For Lakko's moderators only.</div>
      </section>
      <section class="auth-side">
        <div class="auth-card">
          <h1>Sign in</h1>
          <p class="lk-auth-lede">Use your security key. You'll touch it once.</p>
          <Show when={error()}>
            {(msg) => (
              <p class="form-error" role="alert">
                {msg()}
              </p>
            )}
          </Show>
          <BusyButton class="primary" busy={busy()} onClick={() => void signIn()}>
            Sign in with security key
          </BusyButton>
          <p class="lk-auth-foot">Reloading the page signs you out. Reports open only while you are signed in.</p>
        </div>
      </section>
    </div>
  );
}
