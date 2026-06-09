// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0017 fresh-device fail-safe screen.
 *
 * Rendered whenever the auth state is `needs-recovery-phrase`: the
 * user authenticated successfully (password or passkey ACCESS) on a
 * device that has NO cached AccountIdentity (phrase) seed, so the
 * canonical identity cannot be rebuilt. Per ADR-0017 we MUST NOT
 * derive the identity from the OPAQUE export key (that is the wrong
 * identity now that the identity is phrase-seeded) nor silently mint
 * a fresh one (that would break every contact's pin). Instead the
 * user supplies their recovery phrase; `provideRecoveryPhrase`
 * derives the BIP39 seed, verifies it against the published bundle
 * (fail-closed on mismatch), caches it, and completes the session.
 *
 * Pairing an existing device over the E2EE channel (ADR-0018) is the
 * other acquisition route and is surfaced as a hint here, but the
 * phrase-entry path is what this screen wires.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";

export default function RestoreIdentityPrompt(): JSX.Element {
  const auth = useAuth();
  const [phrase, setPhrase] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const username = (): string => {
    const s = auth.state();
    return s.kind === "needs-recovery-phrase" ? s.account.username : "";
  };

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await auth.provideRecoveryPhrase(phrase());
      setPhrase("");
      // No navigation needed — the state flips to `session` and the
      // surrounding guard re-renders the protected content.
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="auth-shell">
      <section class="auth-card">
        <h1>Restore your secure identity</h1>
        <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
          New device — enter your recovery phrase or pair an existing
          device to restore your secure identity for{" "}
          <strong>@{username()}</strong>. Your password got you in, but
          your long-term identity (the key your contacts trust) lives in
          your 24-word recovery phrase.
        </p>
        <form onSubmit={onSubmit}>
          <label>
            Recovery phrase (24 words)
            <textarea
              rows={4}
              placeholder="abandon abandon abandon … art"
              required
              value={phrase()}
              onInput={(ev) => setPhrase(ev.currentTarget.value)}
              disabled={busy()}
              autocomplete="off"
              spellcheck={false}
              style={{
                "font-family":
                  "ui-monospace, SFMono-Regular, Menlo, monospace",
              }}
            />
          </label>
          <Show when={error() !== null}>
            <div class="form-error">{error()}</div>
          </Show>
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Restoring…" : "Restore identity"}
          </button>
        </form>
        <p class="form-footer" style={{ color: "var(--surface-muted)" }}>
          Don't have your phrase? Pair an existing signed-in device to
          carry your identity across (coming soon), or sign out and start
          a new account.
        </p>
        <p class="form-footer">
          <button
            type="button"
            disabled={busy()}
            onClick={() => void auth.logout()}
          >
            Sign out
          </button>
        </p>
      </section>
    </div>
  );
}
