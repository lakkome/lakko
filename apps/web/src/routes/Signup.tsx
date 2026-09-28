// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signup route (docs/ux/client-redesign.md section 5.6): username with the
 * live availability hint, name, password, confirm password, Create account.
 *
 * The recovery-phrase step is NOT rendered here. `auth.signup` lands the
 * session before it returns the phrase, and this page sits inside
 * `<RequireAnonymous>`, which would navigate away on that transition; the
 * route guards render `auth/SignupPhrasePrompt.tsx` for as long as
 * `auth.signupPhrase()` is unacknowledged (`auth/phrase-gate.ts`).
 */
import { createSignal, onCleanup, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { isSignupRateLimited, signupErrorMessage } from "../auth/auth-error-copy";
import { isAbortError } from "../auth/pow";
import { SIGNUP_WORKING_LINE, SIGNUP_WORKING_TITLE } from "../auth/signup-working";
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

export default function Signup(): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();

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
      setError(usernameReason(checked) ?? "Enter a username.");
      return;
    }
    if (!maySaveDisplayName(displayNameCheck())) {
      setError("Fix your name before continuing.");
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
      // Resolving lands the session with the phrase pending; the guard around
      // this route switches to the phrase screen on that transition.
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
      <h1>Create your account</h1>
      <p class="lk-auth-lede">
        Your password never leaves your device. You will get a recovery phrase
        to write down next.
      </p>
      <form onSubmit={onSubmit}>
        <label>
          Username
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
        <label>
          Name (optional)
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
          label="Password"
          autocomplete="new-password"
          value={password}
          setValue={setPassword}
          disabled={busy}
          minLength={MIN_PASSWORD_LENGTH}
        />
        <PasswordField
          label="Confirm password"
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
              Try again
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
              Create account
            </button>
          }
        >
          {/* docs/ux/anti-abuse.md section 1: a disabled row with a spinner,
              one line below, and Cancel. No percentage, no time estimate. */}
          <div role="status" aria-live="polite" class="lk-stack" style={{ gap: "8px" }}>
            <button class="primary" type="button" disabled aria-busy="true">
              <span class="lakko-spinner" aria-hidden="true" /> {SIGNUP_WORKING_TITLE}
            </button>
            <p class="form-info" style={{ margin: 0 }}>
              {SIGNUP_WORKING_LINE}
            </p>
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
          </div>
        </Show>
      </form>
      <p class="lk-auth-foot">
        Already have an account? <A href={paths.login}>Log in</A>
      </p>
      <PasswordExplainer />
    </AuthLayout>
  );
}
