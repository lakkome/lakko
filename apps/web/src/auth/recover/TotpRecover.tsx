// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with an authenticator code (ADR-0018 factor (c)): username plus
 * the current six-digit code. This grants access only; the identity then
 * comes from a seed cached on this device, or the guard shows the restore
 * prompt (phrase, three words, another device). No navigation here: the
 * route's guard reacts to the state flip.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { AuthHttpError } from "../client";
import { authLimitMessage } from "../auth-error-copy";
import { normalizeTotpCode } from "../totp-code";
import { requireValidUsername } from "../../profile/username-field";
import { UsernameField } from "./UsernameField";

/** The server is enumeration-safe: 401 covers an unknown account, an
 *  unconfirmed enrolment, a wrong code and a replayed code alike. */
export function totpRecoverErrorMessage(err: unknown): string {
  const limit = authLimitMessage(err);
  if (limit !== null) return limit;
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return "That code did not work. Check the app and try the next one.";
    }
    if (err.status === 503) {
      return "Authenticator recovery is not available here. Use another way in.";
    }
  }
  if (err instanceof Error && /username not found/.test(err.message)) {
    return "That code did not work. Check the username and the app, then try again.";
  }
  return err instanceof Error ? err.message : String(err);
}

export function TotpRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
}): JSX.Element {
  const auth = useAuth();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    let user = props.username().trim();
    if (user.length === 0) {
      setError("Enter the username of the account.");
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    try {
      user = await requireValidUsername(user);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setError("Enter the six-digit code from your authenticator app.");
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithTotp(user, normalized);
      setCode("");
    } catch (err) {
      setError(totpRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={(e) => void onSubmit(e)}>
      <p class="lk-muted">
        Use the six-digit code from the authenticator app you set up in Settings. It gets you
        back in; your chats then need your recovery phrase or another device.
      </p>
      <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
      <label>
        Code from the app
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
      <button class="primary" type="submit" disabled={busy()}>
        {busy() ? "Checking..." : "Continue"}
      </button>
    </form>
  );
}
