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
import { createSignal, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { signupErrorMessage } from "../auth/auth-error-copy";
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

  const normalised = (): string | null => {
    const c = usernameCheck();
    return c.kind === "valid" ? c.normalised : null;
  };
  const nameReason = (): string | null => {
    const c = displayNameCheck();
    return c.kind === "invalid" ? c.reason : null;
  };

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
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
    try {
      // Resolving lands the session with the phrase pending; the guard around
      // this route switches to the phrase screen on that transition.
      await auth.signup(checked.normalised, password());
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
      setError(signupErrorMessage(err));
    } finally {
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
        </Show>
        <button
          class="primary"
          type="submit"
          disabled={
            busy() ||
            !mayLookUp(usernameCheck()) ||
            !maySaveDisplayName(displayNameCheck())
          }
        >
          {busy() ? "Creating account..." : "Create account"}
        </button>
      </form>
      <p class="lk-auth-foot">
        Already have an account? <A href={paths.login}>Log in</A>
      </p>
      <PasswordExplainer />
    </AuthLayout>
  );
}
