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
import { listPositions } from "../phrase-confirm";
import { t } from "../../l10n";
import { tRich } from "../../l10n/rich";

export function WordsRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
  /** The shortcut is off for this account or this server: the caller shows
   *  the catalog message `noticeKey` over the other ways in. */
  readonly onUnavailable: (noticeKey: string) => void;
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
      setError(t("recovery.username_required"));
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
        props.onUnavailable("recovery.words.unavailable");
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
          props.onUnavailable("recovery.words.switched_off");
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
          <p class="lk-muted">{t("recovery.words.intro")}</p>
          <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <button class="primary" type="submit" disabled={busy()}>
            {busy() ? t("common.looking_up") : t("common.continue")}
          </button>
        </form>
      }
    >
      {(c) => (
        <form onSubmit={onAnswer}>
          <p class="lk-muted">
            {tRich("recovery.words.prompt", {
              username: <strong>@{canonicalUsername()}</strong>,
              positions: <strong>{listPositions(c().positions)}</strong>,
            })}
          </p>
          <WordSlots positions={() => c().positions} value={words} setValue={setWords} disabled={busy} />
          <PasswordField
            label={t("auth.password.new.label")}
            autocomplete="new-password"
            value={password}
            setValue={setPassword}
            disabled={busy}
          />
          <PasswordField
            label={t("auth.password.new_confirm.label")}
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
            {busy() ? t("common.checking") : t("recovery.submit")}
          </button>
          <Show when={misses() > 0}>
            <p class="form-info">
              {t("recovery.words.tries_left", { count: WORD_VAULT_DESTRUCTION_CAP - misses() })}
            </p>
          </Show>
        </form>
      )}
    </Show>
  );
}
