// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The post-signup "Save your recovery phrase" screen.
 *
 * It lived inside `routes/Signup.tsx` as the form's second stage, and was
 * never seen: signup lands the session before it returns the phrase, and the
 * `<RequireAnonymous>` guard around `/signup` navigated home on that
 * transition, unmounting the page before the stage switched. The phrase is
 * shown once and cannot be shown again, so every new account was left without
 * the only thing that recovers it on another device (ADR-0017).
 *
 * It is now rendered by the route GUARDS, from `auth.signupPhrase()`, for as
 * long as that phrase is unacknowledged — whichever route the user is on, and
 * again after a reload. The decision lives in `auth/phrase-gate.ts`. The screen
 * itself is unchanged: the 24 words, the "I have written it down" gate, and the
 * optional 3-of-5 social-recovery split.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import {
  DEFAULT_SHARE_COUNT,
  DEFAULT_THRESHOLD,
  splitMnemonic,
} from "../recovery/shamir";

/**
 * Render the pending signup phrase. Acknowledging it clears the phrase; the
 * guard that rendered this then re-decides what to show (the route it wraps,
 * or the `?return=` target on a public auth page).
 */
export default function SignupPhrasePrompt(): JSX.Element {
  const auth = useAuth();
  const [acknowledged, setAcknowledged] = createSignal(false);
  return (
    <AuthLayout>
        <RecoveryPhrasePanel
          phrase={auth.signupPhrase() ?? ""}
          vaultPositions={auth.signupVaultPositions}
          acknowledged={acknowledged}
          setAcknowledged={setAcknowledged}
          onAcknowledge={() => auth.acknowledgeSignupPhrase()}
        />
    </AuthLayout>
  );
}

// ----------------------------------------------------------------------
// Recovery phrase display
// ----------------------------------------------------------------------

interface PhrasePanelProps {
  readonly phrase: string;
  /** ADR-0049: the three positions the word vault enrolled, once known. */
  readonly vaultPositions: () => number[] | null;
  readonly acknowledged: () => boolean;
  readonly setAcknowledged: (v: boolean) => void;
  readonly onAcknowledge: () => void;
}

function RecoveryPhrasePanel(props: PhrasePanelProps): JSX.Element {
  const words = (): string[] => props.phrase.split(/\s+/).filter((w) => w !== "");
  return (
    <>
      <h1>Save your recovery phrase</h1>
      <p class="auth-lede">
        These 24 words are the <strong>only way</strong> to recover
        your account if you forget your password. Write them down
        on paper, in order. Do not screenshot them. Do not store
        them in plain text. We cannot show them to you again.
      </p>

      <ol
        class="lk-phrase"
      >
        {words().map((w, i) => (
          <li classList={{ "lk-phrase-pick": props.vaultPositions()?.includes(i + 1) === true }}>
            <span>{w}</span>
          </li>
        ))}
      </ol>
      <Show when={props.vaultPositions() !== null}>
        <p class="form-info">
          On a new device you will normally be asked for just three of these:
          words {props.vaultPositions()?.join(", ")}. Keep the whole list safe
          anyway; it is the only way back if the shortcut is ever switched off.
        </p>
      </Show>

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
        Done
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
