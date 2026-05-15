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
import { createSignal, For, Show, type JSX } from "solid-js";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { readReturnUrl } from "../auth/return-url";
import {
  DEFAULT_SHARE_COUNT,
  DEFAULT_THRESHOLD,
  splitMnemonic,
} from "../recovery/shamir";

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

      <SocialRecoveryPanel phrase={props.phrase} />

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

// ----------------------------------------------------------------------
// Phase 1.21c — optional Shamir 3-of-5 social-recovery shares
// ----------------------------------------------------------------------

/**
 * Optional sub-panel inside the post-signup recovery-phrase
 * screen. Lets the user split the 24-word mnemonic into 5
 * shares (any 3 reconstruct) for distribution to guardians.
 *
 * Collapsed by default — the canonical "write 24 words on
 * paper" flow stays the prominent path. Power users who want
 * a social fallback expand the panel, copy each share, and
 * give one to each guardian. The user acknowledgement above
 * (the "I have written down" checkbox) gates the Done button
 * regardless; social shares are a complement to the
 * paper-phrase backup, not a replacement.
 */
function SocialRecoveryPanel(props: { phrase: string }): JSX.Element {
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [shares, setShares] = createSignal<string[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [copiedIndex, setCopiedIndex] = createSignal<number | null>(null);

  async function onGenerate(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const out = await splitMnemonic(
        props.phrase,
        DEFAULT_THRESHOLD,
        DEFAULT_SHARE_COUNT,
      );
      setShares(out);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onCopy(idx: number, value: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(value);
      setCopiedIndex(idx);
      // Reset the "copied" flag after a short delay so a
      // subsequent copy on the same chip surfaces a fresh
      // confirmation.
      setTimeout(() => setCopiedIndex((cur) => (cur === idx ? null : cur)), 1500);
    } catch {
      setError("Clipboard write blocked — copy manually.");
    }
  }

  return (
    <details
      open={open()}
      onToggle={(e) => setOpen(e.currentTarget.open)}
      style={{
        "margin-bottom": "1rem",
        padding: "0.5rem 0.75rem",
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-sm)",
        background: "var(--surface-elevated)",
      }}
    >
      <summary
        style={{
          cursor: "pointer",
          "font-size": "0.9rem",
        }}
      >
        Optional: split into 3-of-5 social-recovery shares
      </summary>
      <p
        style={{
          color: "var(--surface-muted)",
          "font-size": "0.85rem",
          margin: "0.5rem 0",
        }}
      >
        Generate 5 share strings; any 3 reconstruct your
        recovery phrase. Distribute one to each trusted
        guardian (family, friends, a safe deposit box). No
        single guardian alone — or any two colluding — can
        recover your account; you can lose contact with two
        guardians and still recover.
      </p>
      <Show
        when={shares().length > 0}
        fallback={
          <button
            type="button"
            onClick={() => {
              void onGenerate();
            }}
            disabled={busy()}
            style={{ "font-size": "0.85rem" }}
          >
            {busy() ? "Generating…" : `Generate ${DEFAULT_SHARE_COUNT} shares`}
          </button>
        }
      >
        <ol
          style={{
            display: "flex",
            "flex-direction": "column",
            gap: "0.4rem",
            padding: 0,
            margin: "0 0 0.5rem 0",
            "list-style": "none",
          }}
        >
          <For each={shares()}>
            {(share, i) => (
              <li
                style={{
                  display: "flex",
                  "align-items": "center",
                  gap: "0.5rem",
                  padding: "0.4rem 0.5rem",
                  background: "var(--surface-bg)",
                  "border-radius": "var(--radius-sm)",
                }}
              >
                <strong style={{ "min-width": "1.5rem", "font-family": "ui-monospace" }}>
                  #{i() + 1}
                </strong>
                <code
                  style={{
                    flex: 1,
                    "font-size": "0.75rem",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                    "white-space": "nowrap",
                  }}
                  title={share}
                >
                  {share}
                </code>
                <button
                  type="button"
                  onClick={() => {
                    void onCopy(i(), share);
                  }}
                  style={{
                    "font-size": "0.75rem",
                    padding: "0.2rem 0.5rem",
                  }}
                >
                  {copiedIndex() === i() ? "Copied" : "Copy"}
                </button>
              </li>
            )}
          </For>
        </ol>
        <p
          style={{
            color: "var(--surface-muted)",
            "font-size": "0.8rem",
            margin: "0.25rem 0 0",
          }}
        >
          Distribute one share to each of {DEFAULT_SHARE_COUNT}{" "}
          trusted guardians. Recover via /recover → Recovery
          shares with any {DEFAULT_THRESHOLD}.
        </p>
      </Show>
      <Show when={error() !== null}>
        <div
          class="form-error"
          style={{ "margin-top": "0.5rem", "font-size": "0.85rem" }}
        >
          {error()}
        </div>
      </Show>
    </details>
  );
}
