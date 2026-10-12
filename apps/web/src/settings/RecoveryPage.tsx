// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Security > Recovery (docs/ux/identity-recovery.md 10.1), as
 * far as it still applies after the maintainer decision of 2026-10-02 (the
 * three words are checked on the device; there is no three-word setting).
 *
 * Two groups, the same split the recovery screen uses:
 *
 *   - what brings the chats back on a new device: the recovery phrase (with
 *     "Show recovery phrase"), passkeys, other devices;
 *   - what signs the person in when the password is forgotten: three words
 *     from the phrase (a statement, nothing to configure) and the
 *     authenticator app.
 *
 * The phrase is read from this device (`AuthContext.devicePhrase`), never
 * from a server. On web "Show recovery phrase" sits behind the account
 * password, which can only be checked with the server; an account without a
 * password (a guest) gets a plain confirmation instead. The gate keeps a
 * passer-by at an unlocked browser from reading the words off the screen;
 * it is not what protects the stored phrase (the device key is).
 */
import { createResource, createSignal, onCleanup, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { showPhraseOffered, threeWordsShown } from "../auth/protect/account-protection";
import { phraseProtectedHere } from "../auth/protect/protection-state";
import { phraseRevealStep } from "../auth/key-reset-owed";
import { AuthClient } from "../auth/client";
import { authLimitMessage, isOffline, screenText } from "../auth/auth-error-copy";
import { phraseWords } from "../auth/phrase-confirm";
import { TotpEnrollView } from "../auth/RecoveryFactors";
import { WaitNotice } from "../auth/recover/FormNotices";
import { isThreeWordsSuspended, threeWordsSettingsLine as threeWordsLine } from "../auth/recover/three-words";
import { attemptsRetryAfterSecs, createRetryCountdown } from "../auth/recover/retry-countdown";
import { useWebauthnClient } from "../auth/use-webauthn-client";
import { Icon } from "../ui/icons";
import { PasswordField } from "../ui/PasswordField";
import { PhraseGrid } from "../ui/PhraseGrid";
import { paths } from "../routes/paths";
import { t } from "../l10n";

export function RecoveryPage(): JSX.Element {
  const auth = useAuth();
  // Whether this device holds the phrase. Only the fact is kept here; the
  // words are read again when the person asks to see them.
  const [phraseSaved] = createResource(async () => (await auth.devicePhrase()) !== null);
  // Whether the account is phrase-protected, as this device knows it
  // (`GET /recovery-email` at the landing, or a phrase saved here).
  const phraseProtected = (): boolean => {
    const st = auth.state();
    return phraseProtectedHere(st.kind === "session" ? st.account.accountId : null);
  };
  // Paused after the saved phrase was refused at log in (three-words.ts).
  const wordsSuspended = (): boolean => {
    const st = auth.state();
    if (st.kind !== "session") return false;
    const storage = typeof localStorage === "undefined" ? undefined : localStorage;
    return isThreeWordsSuspended(storage, st.account.accountId);
  };

  return (
    <>
      <A
        href={paths.settingsSection("security")}
        class="ghost small lk-wide-only"
        style={{ "align-self": "flex-start" }}
      >
        <Icon name="back" size={16} />
        {t("settings.section.security")}
      </A>
      <div class="lk-page-head">
        <h1>{t("settings.recovery.title")}</h1>
      </div>

      <section class="lk-card">
        <h2>{t("settings.recovery.group.keys")}</h2>
        {/* ADR-0067: the words were never shown to the owner of an account
            without a saved phrase, so there is nothing to show again; saving
            one is in Settings > Account. */}
        <Show when={showPhraseOffered(phraseProtected(), phraseSaved() === true)}>
          <PhraseRow saved={() => phraseSaved() === true} known={() => phraseSaved() !== undefined} />
        </Show>
        <PasskeysRow />
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="devices" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("settings.recovery.devices.title")}</span>
            <span class="lk-card-row-sub">{t("settings.security.devices.sub")}</span>
          </div>
          <A href={paths.devices} class="lk-card-row-action">
            {t("common.manage")}
          </A>
        </div>
      </section>

      {/* ADR-0067 section 11: three words exist only for a phrase-protected
          account. */}
      <Show when={threeWordsShown(phraseProtected())}>
      <section class="lk-card">
        <h2>{t("settings.recovery.group.signin")}</h2>
        <div class="lk-card-row">
          <span class="lk-card-row-icon">
            <Icon name="lines" size={18} />
          </span>
          <div class="lk-card-row-body">
            <span class="lk-card-row-title">{t("recovery.method.words.title")}</span>
            <span class="lk-card-row-sub">
              {t(threeWordsLine(phraseSaved() === true, wordsSuspended()))}
            </span>
          </div>
        </div>
      </section>
      </Show>

      <section class="lk-card">
        <h2>{t("settings.recovery.totp.title")}</h2>
        <TotpEnrollView />
      </section>
    </>
  );
}

/** "Passkeys: 2. 1 of them restores keys." */
function PasskeysRow(): JSX.Element {
  const auth = useAuth();
  const { client } = useWebauthnClient();
  const [counts] = createResource(
    () => auth.identitySecrets()?.sessionToken ?? null,
    async (token): Promise<{ total: number; restoring: number } | null> => {
      const c = client();
      if (c === null) return null;
      try {
        const total = (await c.listCredentials()).credentials.length;
        let restoring = 0;
        try {
          restoring = (await new AuthClient().passkeyVaultList(token)).length;
        } catch {
          // The server cannot say which passkeys restore keys: none counted.
        }
        return { total, restoring: Math.min(restoring, total) };
      } catch {
        return null;
      }
    },
  );
  const sub = (): string => {
    const c = counts();
    if (c === undefined || c === null) return t("settings.recovery.passkeys.sub");
    if (c.total === 0) return t("settings.recovery.passkeys.none");
    return `${t("settings.recovery.passkeys.count", { count: c.total })} ${t(
      "settings.recovery.passkeys.restoring",
      { count: c.restoring },
    )}`;
  };
  return (
    <div class="lk-card-row">
      <span class="lk-card-row-icon">
        <Icon name="lock" size={18} />
      </span>
      <div class="lk-card-row-body">
        <span class="lk-card-row-title">{t("settings.passkeys.title")}</span>
        <span class="lk-card-row-sub">{sub()}</span>
      </div>
      <A href={paths.settingsSection("security")} class="lk-card-row-action">
        {t("common.manage")}
      </A>
    </div>
  );
}

/** The recovery phrase row and "Show recovery phrase". */
function PhraseRow(props: {
  readonly saved: () => boolean;
  readonly known: () => boolean;
}): JSX.Element {
  const auth = useAuth();
  // "pending": a recovery-credential replace is owed, so the words are
  // withheld (as on the automatic phrase screen) until "Try again" succeeds.
  type Step = "closed" | "pending" | "asking" | "shown";
  const [step, setStep] = createSignal<Step>("closed");
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [words, setWords] = createSignal<string[]>([]);
  const wait = createRetryCountdown();

  // An account without a username has no password to ask for.
  const hasPassword = (): boolean => {
    const s = auth.state();
    return s.kind === "session" && s.account.username.trim().length > 0;
  };

  function hide(): void {
    setWords([]);
    setPassword("");
    setError(null);
    setStep("closed");
  }
  // The words never outlive the page they were shown on.
  onCleanup(() => setWords([]));

  /** "Show recovery phrase": the pending explanation while the publish of
   *  a reset is owed (UX 15.4), else the re-authentication step. */
  function open(): void {
    setError(null);
    setStep(
      phraseRevealStep({ phraseOnDevice: props.saved(), publishOwed: auth.publishOwed() }) ===
        "publish-pending"
        ? "pending"
        : "asking",
    );
  }

  async function onRetryCredential(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await auth.retryOwedPublish();
      // Nothing is owed any more: on to the re-authentication step.
      setStep("asking");
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else {
        setError(
          isOffline(err)
            ? t("recovery_phrase.fresh.publish_error")
            : (screenText(err) ?? authLimitMessage(err) ?? t("recovery_phrase.fresh.error.generic")),
        );
      }
    } finally {
      setBusy(false);
    }
  }

  async function reveal(): Promise<void> {
    const phrase = await auth.devicePhrase();
    // Decided again at the moment of showing: the debt may have appeared or
    // been settled since the row was opened.
    const decision = phraseRevealStep({
      phraseOnDevice: phrase !== null,
      publishOwed: auth.publishOwed(),
    });
    if (decision === "not-saved" || phrase === null) {
      setError(t("settings.recovery.phrase.not_saved_here"));
      return;
    }
    if (decision === "publish-pending") {
      setPassword("");
      setStep("pending");
      return;
    }
    setWords(phraseWords(phrase));
    setPassword("");
    setStep("shown");
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      if (hasPassword() && !(await auth.verifyPassword(password()))) {
        setError(t("settings.recovery.phrase.wrong_password"));
        return;
      }
      await reveal();
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else {
        // eslint-disable-next-line no-console
        console.warn("[recovery] password check failed", err);
        setError(
          isOffline(err)
            ? t("common.error.offline")
            : (authLimitMessage(err) ?? t("settings.recovery.phrase.error.generic")),
        );
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div class="lk-card-row">
        <span class="lk-card-row-icon">
          <Icon name="key" size={18} />
        </span>
        <div class="lk-card-row-body">
          <span class="lk-card-row-title">{t("recovery.method.phrase.title")}</span>
          <span class="lk-card-row-sub">{t("settings.recovery.phrase.sub")}</span>
          <Show when={props.known()}>
            <span class="lk-card-row-sub">
              <strong>
                {props.saved()
                  ? t("settings.recovery.phrase.saved_here")
                  : t("settings.recovery.phrase.not_saved_here")}
              </strong>
            </span>
          </Show>
        </div>
        <Show when={props.saved() && step() === "closed"}>
          <button type="button" class="lk-card-row-action" onClick={open}>
            {t("settings.recovery.phrase.show")}
          </button>
        </Show>
      </div>
      <Show when={step() === "pending"}>
        <div class="lk-alert danger" role="alert">
          {t("recovery_phrase.fresh.publish_owed")}
        </div>
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
        </Show>
        <WaitNotice remainingMs={wait.remainingMs} />
        <div class="lk-actions">
          <button
            class="primary"
            type="button"
            disabled={busy() || wait.waiting()}
            onClick={() => void onRetryCredential()}
          >
            {busy() ? t("common.checking") : t("common.try_again")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={hide}>
            {t("common.cancel")}
          </button>
        </div>
      </Show>
      <Show when={step() === "asking"}>
        <form onSubmit={(e) => void onSubmit(e)}>
          <Show
            when={hasPassword()}
            fallback={<p class="lk-muted">{t("settings.recovery.phrase.confirm_show")}</p>}
          >
            <p class="lk-muted">{t("settings.recovery.phrase.password_prompt")}</p>
            <PasswordField
              label={t("auth.password.label")}
              autocomplete="current-password"
              value={password}
              setValue={setPassword}
              disabled={busy}
            />
          </Show>
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <WaitNotice remainingMs={wait.remainingMs} />
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={busy() || wait.waiting()}>
              {busy() ? t("common.checking") : t("settings.recovery.phrase.show")}
            </button>
            <button type="button" class="ghost" disabled={busy()} onClick={hide}>
              {t("common.cancel")}
            </button>
          </div>
        </form>
      </Show>
      <Show when={step() === "shown"}>
        <PhraseGrid words={words} />
        <p class="form-info">{t("recovery_phrase.save.anyone_with_words")}</p>
        <div class="lk-actions">
          <button type="button" onClick={hide}>
            {t("settings.recovery.phrase.hide")}
          </button>
        </div>
      </Show>
    </>
  );
}
