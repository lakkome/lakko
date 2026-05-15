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
 *
 * ## Phase 1.21c — social recovery (Shamir 3-of-5)
 *
 * The form now offers two input modes:
 *
 *   * **Phrase** — paste the original 24-word mnemonic.
 *     Unchanged from 1.16c3c3.
 *   * **Shares** — paste 3+ Shamir shares, each on its own
 *     line, of the form `<index>:<base64>`. The
 *     [`combineSharesToMnemonic`] helper reconstructs the
 *     entropy + rebuilds the canonical phrase, then drives
 *     the same `AuthContext.recover(phrase, …)` path. A
 *     wrong / insufficient share count produces a
 *     syntactically-valid phrase that fails to OPAQUE-
 *     authenticate; the auth-service rejection surfaces here
 *     as the standard "invalid recovery" error (which is the
 *     right collapse — distinguishing "wrong shares" from
 *     "wrong phrase" would leak the social-recovery layer to
 *     the server).
 */
import { createSignal, Show, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { useAuth } from "../auth/AuthContext";
import { combineSharesToMnemonic } from "../recovery/shamir";

/** Which input mode the user is filling. */
type RecoverMode = "phrase" | "shares";

export default function Recover(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [mode, setMode] = createSignal<RecoverMode>("phrase");
  const [phrase, setPhrase] = createSignal("");
  const [sharesInput, setSharesInput] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

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
      // Resolve the phrase. Phrase mode passes it through
      // verbatim; shares mode reconstructs the entropy and
      // re-derives the phrase via BIP39's checksum logic.
      let resolved: string;
      if (mode() === "shares") {
        const lines = sharesInput()
          .split(/\r?\n/)
          .map((l) => l.trim())
          .filter((l) => l.length > 0);
        if (lines.length < 2) {
          throw new Error(
            "At least 2 shares required (default Lakko config is 3-of-5).",
          );
        }
        try {
          resolved = await combineSharesToMnemonic(lines);
        } catch (err) {
          throw new Error(
            `Couldn't combine shares: ${
              err instanceof Error ? err.message : String(err)
            }`,
          );
        }
      } else {
        resolved = phrase();
      }
      await auth.recover(resolved, password());
      // Drop the phrase / shares + password fields immediately
      // — the mnemonic is a long-term secret the user has to
      // keep on paper; we don't need it any more after the
      // server has accepted the rotation.
      setPhrase("");
      setSharesInput("");
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
        <Show when={cachedUsername() !== null} fallback={<NoCachedAccount />}>
          <RecoverForm
            cachedUsername={cachedUsername() ?? ""}
            mode={mode}
            setMode={setMode}
            phrase={phrase}
            setPhrase={setPhrase}
            sharesInput={sharesInput}
            setSharesInput={setSharesInput}
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

interface RecoverFormProps {
  readonly cachedUsername: string;
  readonly mode: () => RecoverMode;
  readonly setMode: (v: RecoverMode) => void;
  readonly phrase: () => string;
  readonly setPhrase: (v: string) => void;
  readonly sharesInput: () => string;
  readonly setSharesInput: (v: string) => void;
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
        Recover your account using either the 24-word phrase or 3+
        social-recovery shares. We'll prove ownership to the server
        and rotate the password envelope. Account on this device:{" "}
        <strong>@{props.cachedUsername}</strong>.
      </p>
      <ModeTabs mode={props.mode} setMode={props.setMode} busy={props.busy} />
      <form onSubmit={props.onSubmit}>
        <Show
          when={props.mode() === "phrase"}
          fallback={
            <SharesField
              value={props.sharesInput}
              setValue={props.setSharesInput}
              busy={props.busy}
            />
          }
        >
          <PhraseField
            value={props.phrase}
            setValue={props.setPhrase}
            busy={props.busy}
          />
        </Show>
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
        <Show when={props.error() !== null}>
          <div class="form-error">{props.error()}</div>
        </Show>
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

function ModeTabs(props: {
  mode: () => RecoverMode;
  setMode: (v: RecoverMode) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <div
      role="tablist"
      style={{
        display: "flex",
        gap: "0.4rem",
        "margin-bottom": "0.75rem",
      }}
    >
      <TabButton
        active={props.mode() === "phrase"}
        disabled={props.busy()}
        onClick={() => props.setMode("phrase")}
        label="24-word phrase"
      />
      <TabButton
        active={props.mode() === "shares"}
        disabled={props.busy()}
        onClick={() => props.setMode("shares")}
        label="Recovery shares"
      />
    </div>
  );
}

function TabButton(props: {
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  label: string;
}): JSX.Element {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={props.active}
      disabled={props.disabled}
      onClick={props.onClick}
      style={{
        padding: "0.4rem 0.75rem",
        "border-radius": "var(--radius-md)",
        border: "1px solid var(--surface-border)",
        background: props.active
          ? "var(--accent)"
          : "var(--surface-elevated)",
        color: props.active ? "var(--accent-fg)" : "inherit",
        cursor: props.disabled ? "wait" : "pointer",
        "font-size": "0.85rem",
      }}
    >
      {props.label}
    </button>
  );
}

function PhraseField(props: {
  value: () => string;
  setValue: (v: string) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <label>
      Recovery phrase (24 words)
      <textarea
        rows={4}
        placeholder="abandon abandon abandon … art"
        required
        value={props.value()}
        onInput={(e) => props.setValue(e.currentTarget.value)}
        disabled={props.busy()}
        autocomplete="off"
        spellcheck={false}
        style={{
          "font-family":
            "ui-monospace, SFMono-Regular, Menlo, monospace",
        }}
      />
    </label>
  );
}

function SharesField(props: {
  value: () => string;
  setValue: (v: string) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <label>
      Recovery shares (one per line)
      <textarea
        rows={6}
        placeholder={
          "3:abcdEFGH...\n" +
          "1:zzzzABCD...\n" +
          "5:mmmm0123..."
        }
        required
        value={props.value()}
        onInput={(e) => props.setValue(e.currentTarget.value)}
        disabled={props.busy()}
        autocomplete="off"
        spellcheck={false}
        style={{
          "font-family":
            "ui-monospace, SFMono-Regular, Menlo, monospace",
        }}
      />
      <small style={{ color: "var(--surface-muted)" }}>
        Paste at least the threshold number of shares (default
        Lakko config is 3-of-5). Each share is one line of the
        form <code>&lt;index&gt;:&lt;base64&gt;</code>.
      </small>
    </label>
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
