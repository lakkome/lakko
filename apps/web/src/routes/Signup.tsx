// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signup route (docs/ux/client-redesign.md section 5.6): username with the
 * live availability hint, name, password, confirm password, Create account.
 *
 * No recovery phrase is shown at sign-up (ADR-0067 D2): the phrase is
 * generated and stored on the device, and the person may save one later.
 * What follows the form is the "Protect your account" step
 * (docs/ux/identity-recovery.md section 19.1a), which is NOT rendered here:
 * `auth.signup` lands the session before it returns, and this page sits
 * inside `<RequireAnonymous>`, which would navigate away on that
 * transition; the route guards render `auth/protect/ProtectAccountPrompt.tsx`
 * for as long as `auth.protectStepOwed()` (`auth/phrase-gate.ts`).
 */
import { createSignal, onCleanup, Show, type JSX } from "solid-js";
import { A, useSearchParams } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { isSignupRateLimited, signupErrorMessage } from "../auth/auth-error-copy";
import { isAbortError } from "../auth/pow";
import { signupWorkingLine, signupWorkingTitle } from "../auth/signup-working";
import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { PasswordExplainer } from "../auth/PasswordExplainer";
import { useProfiles } from "../profile/ProfileContext";
import {
  createDisplayNameCheck,
  createUsernameCheck,
} from "../profile/username-field";
import {
  mayLookUp,
  maySaveDisplayName,
  savedDisplayName,
  usernameReason,
} from "../profile/rules";
import { MIN_PASSWORD_LENGTH, newPasswordProblem } from "../auth/password-rules";
import { AuthLayout } from "../ui/AuthLayout";
import { PasswordField } from "../ui/PasswordField";
import { COPY as FIND_ME } from "../settings/who-can-find-me";

export default function Signup(): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();
  // docs/ux/who-can-find-me.md section 7: `Change` opens Settings > Privacy.
  // The setting exists only once the account does, so the link asks for the
  // screen to open as soon as signup (and its protect step) is done,
  // through the `?return=` the auth guards already honour.
  const [search, setSearch] = useSearchParams<{ return?: string }>();
  const privacyAfterSignup = (): boolean => search.return === paths.settingsSection("privacy");
  // An invite landing's `?return=` is not overwritten: losing the invite the
  // person came for would be worse than one link fewer.
  const otherReturn = (): boolean =>
    typeof search.return === "string" && search.return.length > 0 && !privacyAfterSignup();

  const [username, setUsername] = createSignal("");
  // ADR-0048: an optional profile name, saved once the account exists.
  const [displayName, setDisplayName] = createSignal("");
  // ADR-0048 section 4: validated as the user types; nothing is registered
  // for a name that fails the rule.
  const usernameCheck = createUsernameCheck(username);
  const displayNameCheck = createDisplayNameCheck(displayName);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  /** ADR-0054 S4: the signup's network limit, which offers `Try again`. */
  const [rateLimited, setRateLimited] = createSignal(false);
  /** Cancels the account-creation proof of work (docs/ux/anti-abuse.md
   *  section 1): `Cancel` returns to the filled-in form, and leaving the
   *  screen cancels too. The server holds nothing until the proof is spent. */
  let running: AbortController | null = null;
  onCleanup(() => running?.abort());

  const normalised = (): string | null => {
    const c = usernameCheck();
    return c.kind === "valid" ? c.normalised : null;
  };
  const nameReason = (): string | null => {
    const c = displayNameCheck();
    return c.kind === "invalid" ? c.reason : null;
  };

  function onCancel(): void {
    running?.abort();
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    await submit();
  }

  async function submit(): Promise<void> {
    if (busy()) return;
    setError(null);
    setRateLimited(false);
    const checked = usernameCheck();
    if (!mayLookUp(checked)) {
      setError(usernameReason(checked) ?? t("auth.signup.error.enter_username"));
      return;
    }
    if (!maySaveDisplayName(displayNameCheck())) {
      setError(t("auth.signup.error.fix_name"));
      return;
    }
    const problem = newPasswordProblem(password(), confirm());
    if (problem !== null) {
      setError(problem);
      return;
    }
    setBusy(true);
    const controller = new AbortController();
    running = controller;
    try {
      // Resolving lands the session with the protect step owed; the guard
      // around this route switches to that step on the transition.
      await auth.signup(checked.normalised, password(), { signal: controller.signal });
      // ADR-0048: the name is local until there is something to publish it
      // into; a failure to save it never fails the signup.
      const name = savedDisplayName(displayNameCheck());
      if (name !== null) {
        try {
          await profiles.saveOwnDisplayName(name);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("saving the profile name after signup failed", err);
        }
      }
      // The password is not needed any more; do not keep it in DOM state.
      setPassword("");
      setConfirm("");
    } catch (err) {
      // Cancelled: back to the filled-in form, nothing to say.
      if (isAbortError(err)) return;
      setRateLimited(isSignupRateLimited(err));
      setError(signupErrorMessage(err));
    } finally {
      if (running === controller) running = null;
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <h1>{t("auth.signup.title")}</h1>
      <p class="lk-auth-lede">{t("auth.signup.lede")}</p>
      <form onSubmit={onSubmit}>
        <label>
          {t("auth.username.label")}
          <input
            type="text"
            autocomplete="username"
            autocapitalize="none"
            spellcheck={false}
            required
            value={username()}
            onInput={(e) => setUsername(e.currentTarget.value)}
            disabled={busy()}
            aria-invalid={usernameCheck().kind === "invalid"}
            aria-describedby="signup-username-hint"
          />
        </label>
        {/* ADR-0048 UX section 4: the reason as the user types; uppercase is
            accepted and shown lowercased. */}
        <div id="signup-username-hint" role="status" class="form-info">
          <Show
            when={usernameReason(usernameCheck())}
            fallback={
              <Show when={normalised()}>{(n) => <span>@{n()}</span>}</Show>
            }
          >
            {(reason) => <span class="form-error">{reason()}</span>}
          </Show>
        </div>
        {/* docs/ux/who-can-find-me.md section 7: one line under the
            username's success state. Outside the live region above so it is
            not re-read on every keystroke. */}
        <Show when={usernameReason(usernameCheck()) === null && normalised() !== null}>
          <p class="form-info" style={{ margin: 0 }}>
            {FIND_ME.signupLine()}{" "}
            <Show
              when={!privacyAfterSignup()}
              fallback={<span>{t("who_can_find_me.signup_after_create")}</span>}
            >
              <Show when={!otherReturn()}>
                <a
                  href={paths.settingsSection("privacy")}
                  onClick={(e) => {
                    e.preventDefault();
                    setSearch({ return: paths.settingsSection("privacy") }, { replace: true });
                  }}
                >
                  {FIND_ME.signupChange()}
                </a>
              </Show>
            </Show>
          </p>
        </Show>
        <label>
          {t("auth.signup.name.label")}
          <input
            type="text"
            autocomplete="name"
            placeholder={normalised() ?? ""}
            value={displayName()}
            onInput={(e) => setDisplayName(e.currentTarget.value)}
            disabled={busy()}
            aria-invalid={nameReason() !== null}
          />
        </label>
        <Show when={nameReason()}>
          {(reason) => (
            <div class="form-error" role="status">
              {reason()}
            </div>
          )}
        </Show>
        <PasswordField
          label={t("auth.password.label")}
          autocomplete="new-password"
          value={password}
          setValue={setPassword}
          disabled={busy}
          minLength={MIN_PASSWORD_LENGTH}
        />
        <PasswordField
          label={t("auth.password.confirm.label")}
          autocomplete="new-password"
          value={confirm}
          setValue={setConfirm}
          disabled={busy}
          minLength={MIN_PASSWORD_LENGTH}
        />
        <Show when={error() !== null}>
          <div class="form-error" role="alert">
            {error()}
          </div>
          <Show when={rateLimited()}>
            <button type="button" onClick={() => void submit()}>
              {t("common.try_again")}
            </button>
          </Show>
        </Show>
        <Show
          when={busy()}
          fallback={
            <button
              class="primary"
              type="submit"
              disabled={!mayLookUp(usernameCheck()) || !maySaveDisplayName(displayNameCheck())}
            >
              {t("auth.signup.submit")}
            </button>
          }
        >
          {/* docs/ux/anti-abuse.md section 1: a disabled row with a spinner,
              one line below, and Cancel. No percentage, no time estimate. */}
          <div role="status" aria-live="polite" class="lk-stack" style={{ gap: "8px" }}>
            <button class="primary" type="button" disabled aria-busy="true">
              <span class="lakko-spinner" aria-hidden="true" /> {signupWorkingTitle()}
            </button>
            <p class="form-info" style={{ margin: 0 }}>
              {signupWorkingLine()}
            </p>
            <button type="button" onClick={onCancel}>
              {t("common.cancel")}
            </button>
          </div>
        </Show>
      </form>
      <p class="lk-auth-foot">
        {tRich("auth.signup.have_account", {
          link: <A href={paths.login}>{t("auth.login.submit")}</A>,
        })}
      </p>
      <PasswordExplainer />
    </AuthLayout>
  );
}
