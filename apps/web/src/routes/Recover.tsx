// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recover-from-mnemonic route — drives `AuthContext.recover` to
 * rotate the password envelope on the server (via 1.16c3c3a's
 * `/password/reset/{start,finish}` pair) and re-establish a
 * session on this device with a freshly-derived device root key.
 *
 * Single-device limitation, same as `/login` (1.16c3b): the
 * account id must already be cached on this device. Cross-device
 * recovery arrives with 1.16d's discovery service.
 *
 * The recovery flow only needs one PoW solve (gating `/start`),
 * so it's a few hundred ms faster than signup. The 24-word
 * textarea is forgiving about whitespace — `fromPhrase`
 * normalises newlines, tabs, and runs of spaces.
 */
import { createSignal, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";

export default function Recover(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [phrase, setPhrase] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // If there's no cached account on this device, the recovery
  // flow can't run at all (account_id is required). Show that
  // state up-front rather than letting the user fill the form
  // and fail at submit.
  const cachedUsername = (): string | null => {
    const s = auth.state();
    if (s.kind === "anon" && s.cachedUsername !== undefined) {
      return s.cachedUsername;
    }
    return null;
  };

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    if (password() !== confirm()) {
      setError("Passwords do not match.");
      return;
    }
    if (password().length < 8) {
      setError("Password must be at least 8 characters.");
      return;
    }
    setBusy(true);
    try {
      await auth.recover(phrase(), password());
      // Drop the phrase + password fields immediately — the
      // mnemonic is a long-term secret the user has to keep on
      // paper; we don't need it any more after the server has
      // accepted the rotation.
      setPhrase("");
      setPassword("");
      setConfirm("");
      navigate(paths.home, { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="auth-shell">
      <section class="auth-card">
        <h1>Recover account</h1>
        {cachedUsername() === null ? (
          <NoCachedAccount />
        ) : (
          <RecoverForm
            cachedUsername={cachedUsername() ?? ""}
            phrase={phrase}
            setPhrase={setPhrase}
            password={password}
            setPassword={setPassword}
            confirm={confirm}
            setConfirm={setConfirm}
            busy={busy}
            error={error}
            onSubmit={onSubmit}
          />
        )}
      </section>
    </div>
  );
}

interface RecoverFormProps {
  readonly cachedUsername: string;
  readonly phrase: () => string;
  readonly setPhrase: (v: string) => void;
  readonly password: () => string;
  readonly setPassword: (v: string) => void;
  readonly confirm: () => string;
  readonly setConfirm: (v: string) => void;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onSubmit: (e: SubmitEvent) => void;
}

function RecoverForm(props: RecoverFormProps): JSX.Element {
  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Paste your 24-word recovery phrase and pick a new password.
        We'll prove ownership to the server and rotate the password
        envelope. Account on this device:{" "}
        <strong>@{props.cachedUsername}</strong>.
      </p>
      <form onSubmit={props.onSubmit}>
        <label>
          Recovery phrase (24 words)
          <textarea
            rows={4}
            placeholder="abandon abandon abandon … art"
            required
            value={props.phrase()}
            onInput={(e) => props.setPhrase(e.currentTarget.value)}
            disabled={props.busy()}
            // Discourage browser autofill from saving these.
            autocomplete="off"
            spellcheck={false}
            style={{
              "font-family":
                "ui-monospace, SFMono-Regular, Menlo, monospace",
            }}
          />
        </label>
        <label>
          New password
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
          Confirm new password
          <input
            type="password"
            autocomplete="new-password"
            required
            value={props.confirm()}
            onInput={(e) => props.setConfirm(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        {props.error() !== null && (
          <div class="form-error">{props.error()}</div>
        )}
        <button class="primary" type="submit" disabled={props.busy()}>
          {props.busy() ? "Verifying…" : "Recover account"}
        </button>
      </form>
      <p class="form-footer">
        Remembered your password? <A href={paths.login}>Log in</A>.
      </p>
    </>
  );
}

function NoCachedAccount(): JSX.Element {
  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        This device has no cached account, so there's no
        <code> account_id </code>
        for the recovery flow to target. Cross-device recovery
        (using just the mnemonic from a fresh browser) arrives
        in Phase 1.16d together with the username discovery
        service.
      </p>
      <p>
        <A href={paths.signup}>Create a new account</A>, or{" "}
        <A href={paths.login}>log in</A> if you already have one
        elsewhere.
      </p>
    </>
  );
}
