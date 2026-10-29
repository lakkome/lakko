// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with the 12-word phrase. It ends in `AuthContext.recover(phrase,
 * newPassword, username)`, which rotates the password on the server and
 * lands a session on this device.
 *
 * ADR-0075 section 8 (UX 19.7): when the words are refused as not the
 * account's, the form asks whether they are the account's phrase from
 * before a key reset in the last month (`auth.recall.checkByPhrase`, signed
 * by the key the phrase derives). If so it continues with "This account's
 * keys were changed on <date>. Was that you?" and `Take the account back`,
 * which does what the old device's form does (section 18.4) with the phrase
 * as the proof and the new password already typed here.
 */
import { isAbandoned } from "../binding-epoch";
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError, AuthNetworkError } from "../client";
import { asksPhraseRecall, type RecallAttemptFailure } from "../recall";
import { isOffline, recoverErrorMessage, screenText } from "../auth-error-copy";
import { t } from "../../l10n";
import { newPasswordProblem } from "../password-rules";
import { requireValidUsername } from "../../profile/username-field";
import { PasswordField } from "../../ui/PasswordField";
import { PhraseInput } from "../../ui/PhraseInput";
import { UsernameField } from "./UsernameField";
import { WaitNotice } from "./FormNotices";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./retry-countdown";
import { displayLocale } from "../../l10n/display-locale";

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
  // UX 19.7: the typed words are the account's phrase from before a reset.
  const [recall, setRecall] = createSignal<{ readonly username: string; readonly changedAtMs: number } | null>(
    null,
  );

  function recallFailureLine(failure: RecallAttemptFailure): string | null {
    switch (failure.kind) {
      case "wrong-password":
        return t("recall.phrase.wrong");
      case "closed":
        return t("devices.recall.error.closed");
      case "offline":
        return t("common.error.offline");
      case "rate-limited":
        return failure.retryAfterSecs === null ? t("common.error.too_many_attempts") : null;
      case "error":
        return t("recovery.error.generic");
    }
  }

  /** `Take the account back`: the phrase proves it; the account gets the
   *  new password typed above, new keys and a new phrase. */
  async function onTakeBack(): Promise<void> {
    const current = recall();
    if (current === null || busy() || wait.waiting()) return;
    setError(null);
    setBusy(true);
    try {
      const failure = await auth.recall.takeBackByPhrase(current.username, phrase(), password());
      if (failure === null) {
        // The guard shows the new phrase screen on the state change.
        setPhrase("");
        setPassword("");
        setConfirm("");
        return;
      }
      if (failure.kind === "rate-limited" && failure.retryAfterSecs !== null) {
        wait.startFor(failure.retryAfterSecs);
      }
      if (failure.kind === "closed" || failure.kind === "wrong-password") setRecall(null);
      setError(recallFailureLine(failure));
    } finally {
      setBusy(false);
    }
  }

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
      // Typed by the person: an owed phrase screen is not shown back.
      await auth.recover(phrase(), password(), canonicalUsername, "typed");
      // The phrase is a long-term secret the user keeps on paper; drop it
      // from DOM state as soon as the server has accepted the rotation.
      setPhrase("");
      setPassword("");
      setConfirm("");
      props.onDone();
    } catch (err) {
      // Overtaken by another session on this device: nothing to report.
      if (isAbandoned(err)) return;
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) {
        wait.startFor(secs);
        return;
      }
      // The words are not the account's now. Were they before a reset?
      const answered = err instanceof AuthHttpError;
      const offline = err instanceof AuthNetworkError || isOffline(err);
      if (
        asksPhraseRecall({
          status: answered ? err.status : null,
          offline,
          localFailure: !answered && !offline && err instanceof Error,
        })
      ) {
        const check = await auth.recall.checkByPhrase(canonicalUsername, phrase());
        if (check.kind === "open") {
          setRecall({ username: canonicalUsername, changedAtMs: check.status.changedAtMs });
          return;
        }
      }
      setError(recoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show
      when={recall()}
      fallback={<>{form()}</>}
    >
      {(current) => (
        <div class="lk-stack" role="group" aria-live="polite">
          <p>
            <strong>
              {t("recall.phrase.changed", {
                date: new Date(current().changedAtMs).toLocaleDateString(displayLocale(), {
                  dateStyle: "medium",
                }),
              })}
            </strong>
          </p>
          <ul>
            <li>{t("devices.recall.form.effect_keys")}</li>
            <li>{t("devices.recall.form.effect_factors")}</li>
            <li>{t("devices.recall.form.effect_messages")}</li>
          </ul>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <WaitNotice remainingMs={wait.remainingMs} />
          <div class="lk-actions">
            <button
              type="button"
              disabled={busy()}
              onClick={() => {
                // The person changed the keys themselves: these words are
                // simply not the account's phrase any more.
                setRecall(null);
                setError(t("recall.phrase.wrong"));
              }}
            >
              {t("notice.was_me")}
            </button>
            <button
              type="button"
              class="primary"
              disabled={busy() || wait.waiting()}
              onClick={() => void onTakeBack()}
            >
              {busy() ? t("devices.recall.form.working") : t("recall.phrase.take_back")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );

  function form(): JSX.Element {
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
}
