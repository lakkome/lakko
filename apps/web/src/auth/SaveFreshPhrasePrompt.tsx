// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The phrase-save screen for a freshly minted identity, rendered whenever the
 * auth state is `save-fresh-phrase`: after the ADR-0017 lost-everything
 * fallback (`mintFreshIdentity` has published the new identity and parked
 * the completion context), and after an ADR-0032 guest signup (no username,
 * nothing replaced). It shows the new 24 words, takes the acknowledgement,
 * then lands the session via `confirmFreshPhraseSaved`.
 *
 * Same grid and vocabulary as the signup phrase screen; the phrase is read
 * from `auth.freshPhrase()`, lives in memory only and is gone once the
 * session lands.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { PhraseGrid } from "../ui/PhraseGrid";
import { phraseWords } from "./phrase-confirm";

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
  const words = (): string[] => phraseWords(phrase());

  async function onDone(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.confirmFreshPhraseSaved();
      // No navigation: the state flips to `session` and the guard re-renders.
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <Show
        when={username() !== ""}
        fallback={
          <>
            <h1>Save your recovery phrase</h1>
            <p class="lk-auth-lede">
              Your account was created without a password. These 24 words are the only way
              back into it on another device or after clearing this browser. Write them
              down in order and keep them somewhere safe.
            </p>
          </>
        }
      >
        <h1>Save your new recovery phrase</h1>
        <p class="lk-auth-lede">
          You created a new identity for <strong>@{username()}</strong>. These 24 words are
          the only way to recover it if you lose access again. Write them down in order
          and keep them somewhere safe.
        </p>
        <p class="form-info">
          Your contacts will see that your safety number changed and will need to verify
          you again before messaging you securely.
        </p>
      </Show>

      <PhraseGrid words={words} />
      <p class="form-info">
        Do not keep them as a screenshot or in a plain text file. Lakko cannot show them
        again.
      </p>

      <label class="lk-check">
        <input
          type="checkbox"
          checked={acknowledged()}
          onChange={(e) => setAcknowledged(e.currentTarget.checked)}
          disabled={busy()}
        />
        <span>
          I have written down these 24 words and understand that losing them means losing
          access to this account.
        </span>
      </label>

      <Show when={error() !== null}>
        <div class="form-error" role="alert">
          {error()}
        </div>
      </Show>

      <button
        class="primary"
        type="button"
        disabled={!acknowledged() || busy()}
        onClick={() => void onDone()}
      >
        {busy() ? "Finishing..." : "Done"}
      </button>
    </AuthLayout>
  );
}
