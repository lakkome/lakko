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
 * One state comes before the words after a reset (ADR-0065, UX 15.4): the
 * keys ARE reset (the server step is one operation), but publishing the
 * new identity to the directory and the device list did not finish
 * (offline, a limit, a refusal). Until that is done the contacts and the
 * other devices cannot find the new keys, so the words are NOT shown. The
 * screen says what happened and offers "Try again", which repeats only the
 * publishing steps (it never resets again), plus "Log out". The debt is
 * also recorded on the device and retried when a session next lands there;
 * a reset older than 30 days asks for a sign-in first.
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

  async function onRetryPublish(): Promise<void> {
    setRetryError(null);
    setRetryBusy(true);
    try {
      await auth.retryFreshPublish();
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else {
        setRetryError(
          isOffline(err)
            ? t("recovery_phrase.fresh.publish_error")
            : (screenText(err) ?? authLimitMessage(err) ?? t("recovery_phrase.fresh.error.generic")),
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
        when={!auth.freshPublishOwed()}
        fallback={
          <>
            <h1>{t("recovery_phrase.fresh.title")}</h1>
            <div class="lk-alert danger" role="alert">
              {auth.freshPublishSignInNeeded()
                ? t("recovery_phrase.fresh.sign_in_to_finish")
                : t("recovery_phrase.fresh.publish_owed")}
            </div>
            <Show when={retryError() !== null}>
              <div class="form-error" role="alert">
                {retryError()}
              </div>
            </Show>
            <WaitNotice remainingMs={wait.remainingMs} />
            <Show when={!auth.freshPublishSignInNeeded()}>
              <button
                class="primary"
                type="button"
                disabled={retryBusy() || wait.waiting()}
                onClick={() => void onRetryPublish()}
              >
                {retryBusy() ? t("common.checking") : t("common.try_again")}
              </button>
            </Show>
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
        <p class="form-info">{t("recovery_phrase.fresh.devices_signed_out")}</p>
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
