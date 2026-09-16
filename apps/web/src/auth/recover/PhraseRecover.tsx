// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery with the 24-word phrase, or with social recovery shares that
 * rebuild it. Both end in `AuthContext.recover(phrase, newPassword,
 * username)`, which rotates the password on the server and lands a session
 * on this device.
 *
 * In `shares` mode the pasted lines are combined into the phrase first. A
 * wrong or short set of shares yields a phrase that fails to authenticate,
 * and that surfaces as the same sentence as a wrong phrase: telling the two
 * apart would reveal the social-recovery layer to the server.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { recoverErrorMessage } from "../auth-error-copy";
import { newPasswordProblem } from "../password-rules";
import { requireValidUsername } from "../../profile/username-field";
import { combineSharesToMnemonic } from "../../recovery/shamir";
import { PasswordField } from "../../ui/PasswordField";
import { PhraseInput } from "../../ui/PhraseInput";
import { UsernameField } from "./UsernameField";

export function PhraseRecover(props: {
  readonly mode: "phrase" | "shares";
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [phrase, setPhrase] = createSignal("");
  const [sharesInput, setSharesInput] = createSignal("");
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
    if (props.username().trim().length === 0) {
      setError("Enter the username of the account.");
      return;
    }
    // ADR-0048 section 4: no lookup for a name that fails the username rule.
    let canonicalUsername: string;
    try {
      canonicalUsername = await requireValidUsername(props.username());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    setBusy(true);
    try {
      let resolved: string;
      if (props.mode === "shares") {
        const lines = sharesInput()
          .split(/\r?\n/)
          .map((l) => l.trim())
          .filter((l) => l.length > 0);
        if (lines.length < 2) {
          setError("Paste at least two shares, one per line.");
          return;
        }
        try {
          resolved = await combineSharesToMnemonic(lines);
        } catch {
          setError("Could not combine the shares. Check that each one is complete and try again.");
          return;
        }
      } else {
        resolved = phrase();
      }
      await auth.recover(resolved, password(), canonicalUsername);
      // The phrase is a long-term secret the user keeps on paper; drop it
      // from DOM state as soon as the server has accepted the rotation.
      setPhrase("");
      setSharesInput("");
      setPassword("");
      setConfirm("");
      props.onDone();
    } catch (err) {
      setError(recoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={onSubmit}>
      <p class="lk-muted">
        {props.mode === "shares"
          ? "Paste the shares you were given, one per line, then choose a new password."
          : "Type your 24 words, then choose a new password."}
      </p>
      <UsernameField value={props.username} setValue={props.setUsername} disabled={busy} />
      <Show
        when={props.mode === "phrase"}
        fallback={
          <label>
            Recovery shares, one per line
            <textarea
              rows={6}
              placeholder={"3:abcdEFGH...\n1:zzzzABCD...\n5:mmmm0123..."}
              required
              value={sharesInput()}
              onInput={(e) => setSharesInput(e.currentTarget.value)}
              disabled={busy()}
              autocomplete="off"
              spellcheck={false}
              class="lk-mono"
            />
            <span class="form-info">
              Paste them exactly as they were sent. You need as many as were agreed when
              they were made, usually 3 of 5.
            </span>
          </label>
        }
      >
        <PhraseInput value={phrase} setValue={setPhrase} disabled={busy} />
      </Show>
      <PasswordField
        label="New password"
        autocomplete="new-password"
        value={password}
        setValue={setPassword}
        disabled={busy}
      />
      <PasswordField
        label="Confirm new password"
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
      <button class="primary" type="submit" disabled={busy()}>
        {busy() ? "Checking..." : "Recover account"}
      </button>
    </form>
  );
}
