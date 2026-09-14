// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0017 lost-everything fallback — phrase-save screen.
 *
 * Rendered whenever the auth state is `save-fresh-phrase`: the user
 * recovered ACCESS via an ADR-0018 factor, had NO recovery phrase and
 * NO other device, and explicitly chose to create a brand-new secure
 * identity from the `RestoreIdentityPrompt`. `mintFreshIdentity` has
 * already minted + published the new AccountIdentity (WITHOUT a
 * supersession proof — none is mintable, since there is no old seed)
 * and parked the completion context. This screen shows the freshly
 * minted 24-word recovery phrase so the user can save it, gated behind
 * an explicit "I have written this down" acknowledgement, then lands
 * the session via `confirmFreshPhraseSaved`.
 *
 * The grid + acknowledge gate mirror the signup-time
 * `RecoveryPhrasePanel` (`routes/Signup.tsx`) — same shape, so the two
 * phrase-display surfaces stay visually consistent. The phrase is read
 * back from `pendingFreshPhrase` (exposed via the `save-fresh-phrase`
 * state on the auth API); it lives in memory only and is gone once the
 * session lands.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";

export default function SaveFreshPhrasePrompt(): JSX.Element {
  const auth = useAuth();
  const [acknowledged, setAcknowledged] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const phrase = (): string => {
    const s = auth.state();
    return s.kind === "save-fresh-phrase" ? auth.freshPhrase() ?? "" : "";
  };
  const username = (): string => {
    const s = auth.state();
    return s.kind === "save-fresh-phrase" ? s.account.username : "";
  };
  const words = (): string[] =>
    phrase()
      .split(/\s+/)
      .filter((w) => w !== "");

  async function onDone(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.confirmFreshPhraseSaved();
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
        {/* ADR-0032 guest signup lands here too (no username, and nothing was
            replaced) — the "new identity / contacts re-verify" copy below is
            only true for the ADR-0017 lost-everything path. */}
        <Show
          when={username() !== ""}
          fallback={
            <>
              <h1>Save your recovery phrase</h1>
              <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
                Your account was created without a password. These 24 words are
                the <strong>only way</strong> to get back into it on another
                device or after clearing this browser. Write them down on paper,
                in order. Do not screenshot them. Do not store them in plain
                text. We cannot show them to you again.
              </p>
            </>
          }
        >
          <h1>Save your new recovery phrase</h1>
          <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
            You created a brand-new secure identity for{" "}
            <strong>@{username()}</strong>. These 24 words are the{" "}
            <strong>only way</strong> to recover this new identity if you
            lose access again. Write them down on paper, in order. Do not
            screenshot them. Do not store them in plain text. We cannot
            show them to you again.
          </p>
          <p
            style={{
              color: "var(--surface-muted)",
              "font-size": "0.9rem",
              "margin-top": 0,
            }}
          >
            Your existing contacts will see a changed identity key with no
            proof and will need to verify you again before you can message
            securely.
          </p>
        </Show>

        <ol
          style={{
            display: "grid",
            "grid-template-columns": "repeat(2, minmax(0, 1fr))",
            gap: "0.25rem 0.75rem",
            padding: "1rem 1rem 1rem 2.5rem",
            margin: "1rem 0",
            background: "var(--surface-bg)",
            border: "1px solid var(--surface-border)",
            "border-radius": "var(--radius-sm)",
            "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
            "font-size": "0.95rem",
          }}
        >
          <For each={words()}>{(w) => <li>{w}</li>}</For>
        </ol>

        <label
          style={{
            display: "flex",
            "align-items": "flex-start",
            gap: "0.5rem",
            "margin-bottom": "1rem",
          }}
        >
          <input
            type="checkbox"
            checked={acknowledged()}
            onChange={(e) => setAcknowledged(e.currentTarget.checked)}
            disabled={busy()}
            style={{ "margin-top": "0.25rem", width: "auto" }}
          />
          <span>
            I have written down my new 24-word recovery phrase. I
            understand that losing it means losing access to this
            identity.
          </span>
        </label>

        <Show when={error() !== null}>
          <div class="form-error">{error()}</div>
        </Show>

        <button
          class="primary"
          type="button"
          disabled={!acknowledged() || busy()}
          onClick={() => void onDone()}
        >
          {busy() ? "Finishing…" : "Done — take me to Lakko"}
        </button>
      </section>
    </div>
  );
}
