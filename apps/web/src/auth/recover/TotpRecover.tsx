// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with an authenticator code (ADR-0018 factor (c)): username plus
 * the current six-digit code. This signs the person in and nothing more: on
 * a device that holds the account's keys the session lands (and the guard
 * offers a new password), otherwise the guard shows the restore prompt
 * (another device, a passkey, the phrase). No navigation here: the route's
 * guard reacts to the state flip.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { authLimitMessage, screenText } from "../auth-error-copy";
import { t } from "../../l10n";
import { normalizeTotpCode } from "../totp-code";
import { requireValidUsername } from "../../profile/username-field";
import { UsernameField } from "./UsernameField";
import { MethodUnavailable, WaitNotice } from "./FormNotices";
import { attemptsRetryAfterSecs, createRetryCountdown } from "./retry-countdown";

/** The server is enumeration-safe: 401 covers an unknown account, an
 *  unconfirmed enrolment, a wrong code and a replayed code alike. */
export function totpRecoverErrorMessage(err: unknown): string {
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return t("recovery.totp.error.wrong");
    }
  }
  if (err instanceof Error && /username not found/.test(err.message)) {
    return t("recovery.totp.error.wrong_username");
  }
  return screenText(err) ?? t("recovery.totp.error.generic");
}

/** The method is switched off on this server (`503`), as opposed to the
 *  limiter's own `503`, which reads as the generic failure. */
export function isMethodUnavailable(err: unknown): boolean {
  return (
    err instanceof AuthHttpError &&
    err.status === 503 &&
    err.errorCode !== "rate-limiter-unavailable"
  );
}

export function TotpRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  /** Back to the list of methods (the `503` state offers it). */
  readonly onBack: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [unavailable, setUnavailable] = createSignal(false);
  const wait = createRetryCountdown();

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    let user = props.username().trim();
    if (user.length === 0) {
      setError(t("recovery.username_required"));
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    try {
      user = await requireValidUsername(user);
    } catch (err) {
      setError(screenText(err) ?? t("username.error.unknown"));
      return;
    }
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setError(t("recovery.totp.error.enter_code"));
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithTotp(user, normalized);
      setCode("");
    } catch (err) {
      const secs = attemptsRetryAfterSecs(err);
      if (secs !== null) wait.startFor(secs);
      else if (isMethodUnavailable(err)) setUnavailable(true);
      else setError(totpRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={!unavailable()} fallback={<MethodUnavailable onBack={props.onBack} />}>
    <form onSubmit={(e) => void onSubmit(e)}>
      <p class="lk-muted">{t("recovery.totp.intro")}</p>
      <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
      <label>
        {t("recovery.totp.code_label")}
        <input
          type="text"
          inputmode="numeric"
          autocomplete="one-time-code"
          pattern="[0-9]{6}"
          maxlength="6"
          required
          value={code()}
          onInput={(e) => setCode(e.currentTarget.value)}
          disabled={busy()}
          placeholder="123456"
          class="lk-code-input"
        />
      </label>
      <Show when={error() !== null}>
        <div class="form-error" role="alert">
          {error()}
        </div>
      </Show>
      <WaitNotice remainingMs={wait.remainingMs} />
      <button class="primary" type="submit" disabled={busy() || wait.waiting()}>
        {busy() ? t("common.checking") : t("common.continue")}
      </button>
    </form>
    </Show>
  );
}
