// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The "new password" form of docs/ux/identity-recovery.md 7.1: two password
 * fields and Save. The person types nothing but the new password; the
 * device proves it holds the account's keys (`AuthContext.replacePassword`
 * runs the password reset from the cached identity seed).
 *
 * Used by the step after a forgot-password route (`NewPasswordPrompt`) and
 * by Settings > Security > Password.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { authLimitMessage, isOffline, screenText } from "./auth-error-copy";
import { MIN_PASSWORD_LENGTH, newPasswordProblem } from "./password-rules";
import { PasswordField } from "../ui/PasswordField";
import { t } from "../l10n";

/** The sentence a failed save shows (ADR-0061 section 6: from the kind of
 *  failure, never the server's prose, which goes to the log). */
export function newPasswordErrorMessage(err: unknown): string {
  if (isOffline(err)) return t("common.error.offline");
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  // eslint-disable-next-line no-console
  console.warn("[new-password] save failed", err);
  return screenText(err) ?? t("recovery.new_password.error.generic");
}

export function NewPasswordForm(props: {
  /** Called once the password is replaced. */
  readonly onSaved: () => void;
  /** The second button beside Save, when given: "Not now" on the step
   *  after a forgot-password route, Cancel in Settings. */
  readonly onSkip?: () => void;
  /** The second button's label (default: "Not now"). */
  readonly skipLabel?: string;
  /** ADR-0075 (UX 19.6): the form is on the "It was not me" screen of an
   *  account notice, which asks nothing but the new password. */
  readonly afterAccountNotice?: boolean;
}): JSX.Element {
  const auth = useAuth();
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setError(problem);
      return;
    }
    setBusy(true);
    try {
      await auth.replacePassword(
        password(),
        props.afterAccountNotice === true ? { afterAccountNotice: true } : undefined,
      );
      setPassword("");
      setConfirm("");
      props.onSaved();
    } catch (err) {
      setError(newPasswordErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={onSubmit}>
      <PasswordField
        label={t("auth.password.new.label")}
        autocomplete="new-password"
        value={password}
        setValue={setPassword}
        disabled={busy}
        minLength={MIN_PASSWORD_LENGTH}
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
      <div class="lk-actions">
        <button class="primary" type="submit" disabled={busy()}>
          {busy() ? t("common.saving") : t("recovery.new_password.save")}
        </button>
        <Show when={props.onSkip}>
          {(skip) => (
            <button type="button" class="ghost" disabled={busy()} onClick={() => skip()()}>
              {props.skipLabel ?? t("recovery.new_password.not_now")}
            </button>
          )}
        </Show>
      </div>
    </form>
  );
}
