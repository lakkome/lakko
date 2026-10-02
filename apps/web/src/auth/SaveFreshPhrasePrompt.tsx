// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The phrase-save screen for a freshly minted identity, rendered whenever the
 * auth state is `save-fresh-phrase`: after the ADR-0017 lost-everything
 * fallback (`mintFreshIdentity` has published the new identity and parked
 * the completion context), and after an ADR-0032 guest signup (no username,
 * nothing replaced). It shows the new 12 words, takes the acknowledgement,
 * then lands the session via `confirmFreshPhraseSaved`.
 *
 * One state comes before the words after a fresh identity: the new identity
 * is published, but replacing the account's recovery credential with one
 * made from the new phrase failed (offline, a limit). The new 12 words
 * would then not sign in anywhere, so they are NOT shown. The screen says
 * what happened and offers "Try again", which repeats only that step (it
 * never mints again), plus "Log out". The debt is also recorded on the
 * device and retried when a session next lands there.
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
import { authLimitMessage, isOffline, screenText } from "./auth-error-copy";
import { WaitNotice } from "./recover/FormNotices";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./recover/retry-countdown";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";

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

  const wait = createRetryCountdown();
  const [retryBusy, setRetryBusy] = createSignal(false);
  const [retryError, setRetryError] = createSignal<string | null>(null);

  async function onRetryCredential(): Promise<void> {
    setRetryError(null);
    setRetryBusy(true);
    try {
      await auth.retryFreshRecoveryCredential();
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else {
        setRetryError(
          isOffline(err)
            ? t("common.error.offline")
            : (authLimitMessage(err) ?? t("recovery_phrase.fresh.credential_error")),
        );
      }
    } finally {
      setRetryBusy(false);
    }
  }

  async function onDone(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.confirmFreshPhraseSaved();
      // No navigation: the state flips to `session` and the guard re-renders.
    } catch (err) {
      setError(
        isOffline(err)
          ? t("common.error.offline")
          : (screenText(err) ?? t("recovery_phrase.fresh.error.generic")),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <Show
        when={!auth.freshRecoveryCredentialPending()}
        fallback={
          <>
            <h1>{t("recovery_phrase.fresh.title")}</h1>
            <div class="lk-alert danger" role="alert">
              {t("recovery_phrase.fresh.credential_pending")}
            </div>
            <Show when={retryError() !== null}>
              <div class="form-error" role="alert">
                {retryError()}
              </div>
            </Show>
            <WaitNotice remainingMs={wait.remainingMs} />
            <button
              class="primary"
              type="button"
              disabled={retryBusy() || wait.waiting()}
              onClick={() => void onRetryCredential()}
            >
              {retryBusy() ? t("common.checking") : t("common.try_again")}
            </button>
            <button type="button" class="ghost" disabled={retryBusy()} onClick={() => void auth.logout()}>
              {t("restore_identity.log_out")}
            </button>
          </>
        }
      >
      <Show
        when={username() !== ""}
        fallback={
          <>
            <h1>{t("recovery_phrase.save.title")}</h1>
            <p class="lk-auth-lede">{t("recovery_phrase.guest.lede")}</p>
          </>
        }
      >
        <h1>{t("recovery_phrase.fresh.title")}</h1>
        <p class="lk-auth-lede">
          {tRich("recovery_phrase.fresh.lede", { username: <strong>@{username()}</strong> })}
        </p>
        <p class="form-info">{t("recovery_phrase.fresh.safety_note")}</p>
      </Show>

      <PhraseGrid words={words} />
      <p class="form-info">{t("recovery_phrase.save.no_screenshot")}</p>

      <label class="lk-check">
        <input
          type="checkbox"
          checked={acknowledged()}
          onChange={(e) => setAcknowledged(e.currentTarget.checked)}
          disabled={busy()}
        />
        <span>{t("recovery_phrase.fresh.acknowledge")}</span>
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
        {busy() ? t("recovery_phrase.fresh.finishing") : t("common.done")}
      </button>
      </Show>
    </AuthLayout>
  );
}
