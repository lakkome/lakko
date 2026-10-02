// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with the 12-word phrase. It ends in `AuthContext.recover(phrase,
 * newPassword, username)`, which rotates the password on the server and
 * lands a session on this device.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { recoverErrorMessage, screenText } from "../auth-error-copy";
import { t } from "../../l10n";
import { newPasswordProblem } from "../password-rules";
import { requireValidUsername } from "../../profile/username-field";
import { PasswordField } from "../../ui/PasswordField";
import { PhraseInput } from "../../ui/PhraseInput";
import { UsernameField } from "./UsernameField";
import { WaitNotice } from "./FormNotices";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./retry-countdown";

export function PhraseRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [phrase, setPhrase] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const wait = createRetryCountdown();

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setError(problem);
      return;
    }
    if (props.username().trim().length === 0) {
      setError(t("recovery.username_required"));
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    let canonicalUsername: string;
    try {
      canonicalUsername = await requireValidUsername(props.username());
    } catch (err) {
      setError(screenText(err) ?? t("username.error.unknown"));
      return;
    }
    setBusy(true);
    try {
      await auth.recover(phrase(), password(), canonicalUsername);
      // The phrase is a long-term secret the user keeps on paper; drop it
      // from DOM state as soon as the server has accepted the rotation.
      setPhrase("");
      setPassword("");
      setConfirm("");
      props.onDone();
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else setError(recoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={onSubmit}>
      <p class="lk-muted">
        {t("recovery.phrase.intro")}
      </p>
      <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
      <PhraseInput value={phrase} setValue={setPhrase} disabled={busy} />
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
      <WaitNotice remainingMs={wait.remainingMs} />
      <button class="primary" type="submit" disabled={busy() || wait.waiting()}>
        {busy() ? t("common.checking") : t("recovery.submit")}
      </button>
    </form>
  );
}
