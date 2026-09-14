// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signup route — drives a real OPAQUE-PAKE registration through
 * `AuthContext.signup` (1.16c3b). Server: `lakko-svc-auth` on
 * `127.0.0.1:8081` via Vite's `/api/auth/*` dev proxy.
 *
 * 1.16c3c1 adds the BIP39 recovery passphrase. After a successful
 * signup the 24 words are shown once and the user must confirm they
 * have written them down before going anywhere. That screen is NOT
 * rendered here: `auth.signup` lands the session, and this page sits
 * inside `<RequireAnonymous>`, which used to navigate home on that
 * transition before a stage of this page could show the phrase. The
 * route guards render `auth/SignupPhrasePrompt.tsx` for as long as
 * `auth.signupPhrase()` is unacknowledged (`auth/phrase-gate.ts`).
 *
 * Generates a fresh UUIDv7 `account_id` client-side, persists
 * `(accountId, username)` in `localStorage` so subsequent logins
 * on this device work.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { A } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
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
  type DisplayNameCheck,
  type UsernameCheck,
} from "../profile/rules";
import { AuthLayout } from "../ui/AuthLayout";

export default function Signup(): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();

  const [username, setUsername] = createSignal("");
  // ADR-0048 — optional profile name, saved once the account exists.
  const [displayName, setDisplayName] = createSignal("");
  // ADR-0048 §4 — validated as the user types; nothing is registered for a
  // name that fails the rule.
  const usernameCheck = createUsernameCheck(username);
  const displayNameCheck = createDisplayNameCheck(displayName);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const checked = usernameCheck();
    if (!mayLookUp(checked)) {
      setError(usernameReason(checked) ?? "Enter a username");
      return;
    }
    if (!maySaveDisplayName(displayNameCheck())) {
      setError("Fix your name before continuing.");
      return;
    }
    if (password() !== confirm()) {
      setError("Passwords do not match.");
      return;
    }
    if (password().length < 8) {
      // Floor only — a real password-strength heuristic + Argon2id
      // KSF cost feedback is a separate sprint.
      setError("Password must be at least 8 characters.");
      return;
    }
    setBusy(true);
    try {
      // Resolving lands the session with the phrase pending; the guard around
      // this route switches to the phrase screen on that transition, so there
      // is nothing to render from the result here.
      await auth.signup(checked.normalised, password());
      // ADR-0048 — the name is local until there is something to publish it
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
      // Clear the password fields immediately — we don't need them
      // any more, and we don't want them sitting in DOM state
      // longer than necessary.
      setPassword("");
      setConfirm("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
        <SignupForm
          username={username}
          setUsername={setUsername}
          usernameCheck={usernameCheck}
          displayName={displayName}
          setDisplayName={setDisplayName}
          displayNameCheck={displayNameCheck}
          password={password}
          setPassword={setPassword}
          confirm={confirm}
          setConfirm={setConfirm}
          busy={busy}
          error={error}
          onSubmit={onSubmit}
        />
    </AuthLayout>
  );
}

// ----------------------------------------------------------------------
// Stage 1: signup form
// ----------------------------------------------------------------------

interface SignupFormProps {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly usernameCheck: () => UsernameCheck;
  readonly displayName: () => string;
  readonly setDisplayName: (v: string) => void;
  readonly displayNameCheck: () => DisplayNameCheck;
  readonly password: () => string;
  readonly setPassword: (v: string) => void;
  readonly confirm: () => string;
  readonly setConfirm: (v: string) => void;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onSubmit: (e: SubmitEvent) => void;
}

function SignupForm(props: SignupFormProps): JSX.Element {
  return (
    <>
      <h1>Create your account</h1>
      <p class="auth-lede">
        Your password never leaves your device. You will get a 24-word recovery
        phrase to write down.
      </p>
      <form onSubmit={props.onSubmit}>
        <label>
          Username
          <input
            type="text"
            autocomplete="username"
            required
            value={props.username()}
            onInput={(e) => props.setUsername(e.currentTarget.value)}
            disabled={props.busy()}
            aria-invalid={props.usernameCheck().kind === "invalid"}
            aria-describedby="signup-username-hint"
          />
        </label>
        {/* ADR-0048 UX §4 — the reason as the user types; uppercase is
            accepted and shown lowercased. */}
        <div id="signup-username-hint" role="status" style={{ "font-size": "0.85rem" }}>
          <Show
            when={usernameReason(props.usernameCheck())}
            fallback={
              <Show when={props.usernameCheck().kind === "valid"}>
                <span style={{ color: "var(--surface-muted)" }}>
                  @{(props.usernameCheck() as { normalised: string }).normalised}
                </span>
              </Show>
            }
          >
            {(reason) => <span class="form-error">{reason()}</span>}
          </Show>
        </div>
        <label>
          Your name (optional)
          <input
            type="text"
            autocomplete="name"
            placeholder={
              props.usernameCheck().kind === "valid"
                ? (props.usernameCheck() as { normalised: string }).normalised
                : ""
            }
            value={props.displayName()}
            onInput={(e) => props.setDisplayName(e.currentTarget.value)}
            disabled={props.busy()}
            aria-invalid={props.displayNameCheck().kind === "invalid"}
          />
        </label>
        <Show
          when={(() => {
            const c = props.displayNameCheck();
            return c.kind === "invalid" ? c.reason : null;
          })()}
        >
          {(reason) => (
            <div class="form-error" role="status" style={{ "font-size": "0.85rem" }}>
              {reason()}
            </div>
          )}
        </Show>
        <label>
          Password
          <input
            type="password"
            autocomplete="new-password"
            required
            value={props.password()}
            onInput={(e) => props.setPassword(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <label>
          Confirm password
          <input
            type="password"
            autocomplete="new-password"
            required
            value={props.confirm()}
            onInput={(e) => props.setConfirm(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        {props.error() !== null && <div class="form-error">{props.error()}</div>}
        <button
          class="primary"
          type="submit"
          disabled={
            props.busy() ||
            !mayLookUp(props.usernameCheck()) ||
            !maySaveDisplayName(props.displayNameCheck())
          }
        >
          {props.busy() ? "Creating account…" : "Create account"}
        </button>
      </form>
      <p class="form-footer">
        Already have an account? <A href={paths.login}>Log in</A>
      </p>
    </>
  );
}
