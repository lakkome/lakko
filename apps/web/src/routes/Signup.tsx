// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Signup route — drives a real OPAQUE-PAKE registration through
 * `AuthContext.signup` (1.16c3b). Server: `lakko-svc-auth` on
 * `127.0.0.1:8081` via Vite's `/api/auth/*` dev proxy.
 *
 * 1.16c3c1 adds the BIP39 recovery passphrase. After a successful
 * signup the form switches to a "your recovery phrase" panel that
 * displays the 24 words once and requires the user to confirm
 * they have written them down before navigating home. Re-display
 * is impossible — the words live in memory only and drop the
 * moment the user hits "Done".
 *
 * Generates a fresh UUIDv7 `account_id` client-side, persists
 * `(accountId, username)` in `localStorage` so subsequent logins
 * on this device work.
 */
import { createSignal, Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { readReturnUrl } from "../auth/return-url";

/**
 * Two-stage form: `"form"` collects username + password, `"phrase"`
 * displays the BIP39 mnemonic post-signup. The local state machine
 * keeps the wiring contained — no need to lift state into a parent.
 */
type Stage = { kind: "form" } | { kind: "phrase"; phrase: string };

export default function Signup(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const location = useLocation();

  const [stage, setStage] = createSignal<Stage>({ kind: "form" });
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [acknowledged, setAcknowledged] = createSignal(false);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
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
      const out = await auth.signup(username().trim(), password());
      // Clear the password fields immediately — we don't need them
      // any more, and we don't want them sitting in DOM state
      // longer than necessary.
      setPassword("");
      setConfirm("");
      setStage({ kind: "phrase", phrase: out.recoveryPhrase });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  function onAcknowledge(): void {
    // Drop the phrase from local state before navigating — the
    // <RecoveryPhrasePanel> below has already finished rendering;
    // there is no path back to it. The same words live in
    // server-side ciphertext (the OPAQUE recovery envelope), but
    // are NOT recoverable from there without the words themselves.
    setStage({ kind: "form" });
    // 1.18e — honour `?return=<path>` from the URL so an anon
    // visitor who clicked "Sign up" from the invite landing
    // (`/i/:slug`) lands back there after auth + acknowledge.
    // Validated path-only by `readReturnUrl`; falls back to `/`
    // when the param is absent / malformed / open-redirect attempt.
    navigate(readReturnUrl(location), { replace: true });
  }

  return (
    <div class="auth-shell">
      <section class="auth-card">
        <Show
          when={stage().kind === "form"}
          fallback={
            <RecoveryPhrasePanel
              phrase={(stage() as { kind: "phrase"; phrase: string }).phrase}
              acknowledged={acknowledged}
              setAcknowledged={setAcknowledged}
              onAcknowledge={onAcknowledge}
            />
          }
        >
          <SignupForm
            username={username}
            setUsername={setUsername}
            password={password}
            setPassword={setPassword}
            confirm={confirm}
            setConfirm={setConfirm}
            busy={busy}
            error={error}
            onSubmit={onSubmit}
          />
        </Show>
      </section>
    </div>
  );
}

// ----------------------------------------------------------------------
// Stage 1: signup form
// ----------------------------------------------------------------------

interface SignupFormProps {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
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
      <h1>Create a Lakko account</h1>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        OPAQUE-PAKE registration. The password never leaves your
        browser — the server only sees an OPRF-blinded envelope.
        After signup you will be shown a 24-word recovery phrase
        once; write it down before continuing.
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
          />
        </label>
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
        <button class="primary" type="submit" disabled={props.busy()}>
          {props.busy() ? "Creating account…" : "Sign up"}
        </button>
      </form>
      <p class="form-footer">
        Already have an account? <A href={paths.login}>Log in</A>.
      </p>
    </>
  );
}

// ----------------------------------------------------------------------
// Stage 2: recovery phrase display
// ----------------------------------------------------------------------

interface PhrasePanelProps {
  readonly phrase: string;
  readonly acknowledged: () => boolean;
  readonly setAcknowledged: (v: boolean) => void;
  readonly onAcknowledge: () => void;
}

function RecoveryPhrasePanel(props: PhrasePanelProps): JSX.Element {
  const words = (): string[] => props.phrase.split(/\s+/).filter((w) => w !== "");
  return (
    <>
      <h1>Save your recovery phrase</h1>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        These 24 words are the <strong>only way</strong> to recover
        your account if you forget your password. Write them down
        on paper, in order. Do not screenshot them. Do not store
        them in plain text. We cannot show them to you again.
      </p>

      <ol
        style={{
          display: "grid",
          "grid-template-columns": "repeat(2, minmax(0, 1fr))",
          gap: "0.25rem 0.75rem",
          padding: "1rem 1rem 1rem 2.5rem",
          margin: "1rem 0",
          background: "var(--surface-bg)",
          border: "1px solid var(--surface-border)",
          "border-radius": "var(--radius-sm)",
          "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
          "font-size": "0.95rem",
        }}
      >
        {words().map((w) => (
          <li>{w}</li>
        ))}
      </ol>

      <label
        style={{
          display: "flex",
          "align-items": "flex-start",
          gap: "0.5rem",
          "margin-bottom": "1rem",
        }}
      >
        <input
          type="checkbox"
          checked={props.acknowledged()}
          onChange={(e) => props.setAcknowledged(e.currentTarget.checked)}
          style={{ "margin-top": "0.25rem", width: "auto" }}
        />
        <span>
          I have written down my 24-word recovery phrase. I
          understand that losing it means losing access to this
          account.
        </span>
      </label>

      <button
        class="primary"
        type="button"
        disabled={!props.acknowledged()}
        onClick={props.onAcknowledge}
      >
        Done — take me to Lakko
      </button>
    </>
  );
}
