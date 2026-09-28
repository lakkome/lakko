// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Forgot-password recovery with three words (ADR-0049). Step 1 asks the
 * server, by the username's login handle (ADR-0054 S5), which words to type; step 2
 * takes the three words plus a new password, opens the vault into the full
 * phrase, and runs the SAME `auth.recover` the phrase form runs, so the
 * published-identity check and the password rotation are unchanged.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { newPasswordProblem } from "../password-rules";
import { requireValidUsername } from "../../profile/username-field";
import {
  WORD_VAULT_DESTRUCTION_CAP,
  wordVaultErrorMessage,
  wordsAreAnswerable,
  type WordVaultChallenge,
} from "../../recovery/word-vault";
import { PasswordField } from "../../ui/PasswordField";
import { WordSlots } from "../../ui/WordSlots";
import { UsernameField } from "./UsernameField";

/** "5, 14 and 21" */
export function listPositions(positions: readonly number[]): string {
  if (positions.length <= 1) return positions.join("");
  return `${positions.slice(0, -1).join(", ")} and ${positions[positions.length - 1]}`;
}

export function WordsRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
  /** The shortcut is off for this account or this server: the caller shows
   *  `notice` over the other ways in. */
  readonly onUnavailable: (notice: string) => void;
}): JSX.Element {
  const auth = useAuth();
  const [canonicalUsername, setCanonicalUsername] = createSignal("");
  const [challenge, setChallenge] = createSignal<WordVaultChallenge | null>(null);
  const [words, setWords] = createSignal<string[]>(["", "", ""]);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [misses, setMisses] = createSignal(0);

  async function onStart(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    if (props.username().trim().length === 0) {
      setError("Enter the username of the account.");
      return;
    }
    setBusy(true);
    try {
      const canonical = await requireValidUsername(props.username());
      // ADR-0054 S5: addressed by the name's login handle, no lookup.
      const c = await auth.wordVaultStart(canonical);
      setCanonicalUsername(canonical);
      setChallenge(c);
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 503) {
        props.onUnavailable("Three-word recovery is not available here. Use another way in.");
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  async function onAnswer(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const name = canonicalUsername();
    const c = challenge();
    if (name.length === 0 || c === null) return;
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setError(problem);
      return;
    }
    setBusy(true);
    try {
      const fullPhrase = await auth.wordVaultOpen(name, c, words());
      await auth.recover(fullPhrase, password(), canonicalUsername());
      setWords(["", "", ""]);
      setPassword("");
      setConfirm("");
      props.onDone();
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        const next = misses() + 1;
        setMisses(next);
        setWords(["", "", ""]);
        if (next >= WORD_VAULT_DESTRUCTION_CAP) {
          props.onUnavailable(
            "The three-word shortcut is now switched off for this account. Use your full recovery phrase or another device.",
          );
        } else {
          setError(wordVaultErrorMessage(err, WORD_VAULT_DESTRUCTION_CAP - next));
        }
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show
      when={challenge()}
      fallback={
        <form onSubmit={onStart}>
          <p class="lk-muted">
            Type three words from your recovery phrase and choose a new password. First,
            which account?
          </p>
          <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? "Looking up..." : "Continue"}
          </button>
        </form>
      }
    >
      {(c) => (
        <form onSubmit={onAnswer}>
          <p class="lk-muted">
            <strong>@{canonicalUsername()}</strong>: type words{" "}
            <strong>{listPositions(c().positions)}</strong> of your recovery phrase.
          </p>
          <WordSlots positions={() => c().positions} value={words} setValue={setWords} disabled={busy} />
          <PasswordField
            label="New password"
            autocomplete="new-password"
            value={password}
            setValue={setPassword}
            disabled={busy}
          />
          <PasswordField
            label="Confirm new password"
            autocomplete="new-password"
            value={confirm}
            setValue={setConfirm}
            disabled={busy}
          />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <button class="primary" type="submit" disabled={busy() || !wordsAreAnswerable(words())}>
            {busy() ? "Checking..." : "Recover account"}
          </button>
          <Show when={misses() > 0}>
            <p class="form-info">{WORD_VAULT_DESTRUCTION_CAP - misses()} tries left.</p>
          </Show>
        </form>
      )}
    </Show>
  );
}
