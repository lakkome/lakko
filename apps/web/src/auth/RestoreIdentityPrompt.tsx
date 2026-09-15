// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0017 fresh-device fail-safe screen.
 *
 * Rendered whenever the auth state is `needs-recovery-phrase`: the
 * user authenticated successfully (password or passkey ACCESS) on a
 * device that has NO cached AccountIdentity (phrase) seed, so the
 * canonical identity cannot be rebuilt. Per ADR-0017 we MUST NOT
 * derive the identity from the OPAQUE export key (that is the wrong
 * identity now that the identity is phrase-seeded) nor silently mint
 * a fresh one (that would break every contact's pin). Two completion
 * routes:
 *
 *   * **Recovery phrase** — the user types the 24 words;
 *     `provideRecoveryPhrase` derives the BIP39 seed, verifies it
 *     against the published bundle (fail-closed on mismatch), caches
 *     it, and completes the session.
 *   * **Another device** (ADR-0018 factor (a), path (a)) — the user
 *     shows a pairing QR to an already-signed-in device, which
 *     transfers the seed (plus a fresh access grant) over the E2EE
 *     rendezvous channel; `completeFromPairing` runs the SAME
 *     fail-closed verification before caching anything.
 */
import { createSignal, Match, onMount, Show, Switch, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { PhraseInput } from "../ui/PhraseInput";
import { WordSlots } from "../ui/WordSlots";
import {
  WORD_VAULT_DESTRUCTION_CAP,
  wordVaultErrorMessage,
  wordsAreAnswerable,
  type WordVaultChallenge,
} from "../recovery/word-vault";
import { AuthHttpError } from "./client";
import { base64Decode } from "./account-id";
import { getDeviceKey } from "./key-store";
import {
  prepareAnonymousJoinFlow,
  prepareJoinFlow,
} from "../devices/orchestrator";
import RestoreViaDevicePanel, {
  type DeviceRestoreFlowHandle,
} from "../devices/RestoreViaDevicePanel";

type Mode = "words" | "phrase" | "device";

export default function RestoreIdentityPrompt(): JSX.Element {
  const auth = useAuth();
  const [mode, setMode] = createSignal<Mode>("words");
  const [phrase, setPhrase] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // ADR-0049 three-word route.
  const [challenge, setChallenge] = createSignal<WordVaultChallenge | null>(null);
  const [challengeError, setChallengeError] = createSignal<string | null>(null);
  const [words, setWords] = createSignal<string[]>(["", "", ""]);
  const [misses, setMisses] = createSignal(0);
  const triesLeft = (): number => WORD_VAULT_DESTRUCTION_CAP - misses();
  const wordsUnavailable = (): boolean => misses() >= WORD_VAULT_DESTRUCTION_CAP;

  const account = (): { username: string; accountIdB64: string } | null => {
    const s = auth.state();
    return s.kind === "needs-recovery-phrase"
      ? {
          username: s.account.username,
          accountIdB64: s.account.accountId,
        }
      : null;
  };

  onMount(() => {
    const acct = account();
    if (acct === null) return;
    void auth
      .wordVaultStart(acct.accountIdB64)
      .then((c) => setChallenge(c))
      .catch((err: unknown) => {
        setChallengeError(wordVaultErrorMessage(err, null));
        if (err instanceof AuthHttpError && err.status === 503) setMode("phrase");
      });
  });

  async function onSubmitWords(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const acct = account();
    const c = challenge();
    if (acct === null || c === null) return;
    setError(null);
    setBusy(true);
    try {
      const fullPhrase = await auth.wordVaultOpen(acct.accountIdB64, c, words());
      await auth.provideRecoveryPhrase(fullPhrase);
      setWords(["", "", ""]);
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        const next = misses() + 1;
        setMisses(next);
        setWords(["", "", ""]);
        setError(
          next >= WORD_VAULT_DESTRUCTION_CAP
            ? "The three-word shortcut is no longer available for this account. Use another device or your full 24-word phrase."
            : wordVaultErrorMessage(err, WORD_VAULT_DESTRUCTION_CAP - next),
        );
        if (next >= WORD_VAULT_DESTRUCTION_CAP) setMode("phrase");
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await auth.provideRecoveryPhrase(phrase());
      setPhrase("");
      // No navigation needed — the state flips to `session` and the
      // surrounding guard re-renders the protected content.
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  // ADR-0018 factor (a), path (a) — restore the identity from another
  // signed-in device. When this browser already has a device key (a
  // password/passkey session ran here before), use the standard
  // authenticated join flow. On a FRESH browser with no device key yet
  // (e.g. ACCESS came via the ADR-0018 TOTP / phone-OTP factor, which
  // does not derive one until the identity is acquired), fall back to the
  // ANONYMOUS join flow: it mints an ephemeral per-device seed in memory,
  // so it needs no pre-existing device key. Either way the transferred
  // phrase seed + session land via `completeFromPairing`, which caches the
  // seed and bootstraps a device key from it.
  async function prepareDeviceFlow(): Promise<DeviceRestoreFlowHandle> {
    const acct = account();
    if (acct === null) {
      throw new Error("no pending identity recovery — sign in first");
    }
    const accountIdBytes = base64Decode(acct.accountIdB64);
    const deviceKey = await getDeviceKey(acct.accountIdB64);
    if (deviceKey === null) {
      const anon = await prepareAnonymousJoinFlow({
        accountIdBytes,
        accountIdB64: acct.accountIdB64,
      });
      return {
        qrPayload: anon.qrPayload,
        awaitWelcome: (signal) => anon.awaitWelcome(signal),
        accept: async (welcome, welcomeCursor) => {
          const outcome = await anon.accept(welcome, { welcomeCursor });
          return outcome.recovery;
        },
        dispose: () => anon.dispose(),
      };
    }
    const session = await prepareJoinFlow({
      accountIdBytes,
      accountIdB64: acct.accountIdB64,
      deviceKey,
    });
    return {
      qrPayload: session.qrPayload,
      awaitWelcome: (signal) => session.awaitWelcome(signal),
      accept: async (welcome, welcomeCursor) => {
        const outcome = await session.accept(welcome, {
          welcomeCursor,
          recovery: true,
        });
        return outcome.recovery;
      },
      dispose: () => session.dispose(),
    };
  }

  return (
    <AuthLayout>
        <h1>Restore your identity</h1>
        <p class="auth-lede">
          You are signed in as <strong>@{account()?.username ?? ""}</strong> on
          a new device. The key your contacts trust is restored from three
          words of your recovery phrase, from a device that is still signed
          in, or from the full phrase.
        </p>
        <div role="tablist" class="lk-actions">
          <Show when={!wordsUnavailable()}>
            <button
              type="button"
              role="tab"
              aria-selected={mode() === "words"}
              class={mode() === "words" ? "primary" : ""}
              disabled={busy()}
              onClick={() => {
                setError(null);
                setMode("words");
              }}
            >
              Three words
            </button>
          </Show>
          <button
            type="button"
            role="tab"
            aria-selected={mode() === "phrase"}
            class={mode() === "phrase" ? "primary" : ""}
            disabled={busy()}
            onClick={() => {
              setError(null);
              setMode("phrase");
            }}
          >
            All 24 words
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode() === "device"}
            class={mode() === "device" ? "primary" : ""}
            disabled={busy()}
            onClick={() => {
              setError(null);
              setMode("device");
            }}
          >
            Another device
          </button>
        </div>
        <Switch>
          <Match when={mode() === "words"}>
            <form onSubmit={onSubmitWords}>
              <Show
                when={challenge() !== null}
                fallback={
                  <p class="form-info">
                    {challengeError() ?? "Fetching which words to type…"}
                  </p>
                }
              >
                <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
                  Type words <strong>{challenge()?.positions.join(", ")}</strong>{" "}
                  of your recovery phrase.
                </p>
                <WordSlots
                  positions={() => challenge()?.positions ?? []}
                  value={words}
                  setValue={setWords}
                  disabled={busy}
                />
              </Show>
              <Show when={error() !== null}>
                <div class="form-error">{error()}</div>
              </Show>
              <button
                class="primary"
                type="submit"
                disabled={busy() || challenge() === null || !wordsAreAnswerable(words())}
              >
                {busy() ? "Checking…" : "Continue"}
              </button>
              <Show when={misses() > 0 && !wordsUnavailable()}>
                <p class="form-info">{triesLeft()} tries left.</p>
              </Show>
            </form>
          </Match>
          <Match when={mode() === "device"}>
            <div>
              <p style={{ color: "var(--surface-muted)" }}>
                Restore from a device that is still signed in: it shows a
                confirmation prompt, you compare the code, and it transfers
                your identity end-to-end encrypted. The server never sees
                it. No camera needed; a QR code is there if you prefer.
              </p>
              <RestoreViaDevicePanel
                prepare={prepareDeviceFlow}
                requestAccount={() => {
                  const acct = account();
                  return acct === null ? null : base64Decode(acct.accountIdB64);
                }}
                onRestored={async (outcome) => {
                  await auth.completeFromPairing(
                    outcome.phraseSeed,
                    outcome.sessionToken ?? undefined,
                    outcome.sessionExpiresAtMs ?? undefined,
                  );
                }}
                disabled={busy()}
              />
            </div>
          </Match>
          <Match when={mode() === "phrase"}>
            <form onSubmit={onSubmit}>
              <PhraseInput value={phrase} setValue={setPhrase} disabled={busy} />
              <Show when={error() !== null}>
                <div class="form-error">{error()}</div>
              </Show>
              <button class="primary" type="submit" disabled={busy()}>
                {busy() ? "Restoring…" : "Restore identity"}
              </button>
            </form>
          </Match>
        </Switch>
        <LostEverythingPanel busy={busy()} />
        <p class="form-footer">
          <button
            type="button"
            disabled={busy()}
            onClick={() => void auth.logout()}
          >
            Sign out
          </button>
        </p>
    </AuthLayout>
  );
}

/**
 * ADR-0017 lost-everything fallback entry point.
 *
 * The THIRD restore option, below recovery-phrase + another-device:
 * for a user who recovered ACCESS via a factor (TOTP / phone-OTP) but
 * has NEITHER their recovery phrase NOR another signed-in device.
 * Without this they are stuck. This is the ONLY sanctioned path that
 * publishes a CHANGED AccountIdentity key WITHOUT a supersession proof
 * — so it is deliberately NOT the default action: collapsed by default
 * behind a clear warning, and the "Create a new identity" button is a
 * plain (non-primary) destructive-ish action the user must opt into.
 */
function LostEverythingPanel(props: { busy: boolean }): JSX.Element {
  const auth = useAuth();
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onCreate(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      // Mints + publishes the new identity (without a proof — none is
      // mintable) and flips the state to `save-fresh-phrase`, which
      // the guard renders as the phrase-save screen. We discard the
      // returned phrase here — the save screen reads it back from
      // `auth.freshPhrase()`.
      await auth.mintFreshIdentity();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <details
      open={open()}
      onToggle={(e) => setOpen(e.currentTarget.open)}
      style={{
        "margin-top": "1rem",
        padding: "0.5rem 0.75rem",
        border: "1px solid var(--surface-border)",
        "border-radius": "var(--radius-sm)",
        background: "var(--surface-elevated)",
      }}
    >
      <summary style={{ cursor: "pointer", "font-size": "0.9rem" }}>
        I don't have my recovery phrase or another device
      </summary>
      <div
        role="alert"
        style={{
          margin: "0.75rem 0",
          padding: "0.6rem 0.75rem",
          border: "1px solid var(--danger, #b00020)",
          "border-radius": "var(--radius-sm)",
          background: "var(--surface-bg)",
          "font-size": "0.85rem",
        }}
      >
        <strong>This creates a brand-new secure identity for your
        account.</strong>{" "}
        Your existing contacts will have to verify you again, any
        encrypted history on your lost device stays inaccessible, and
        you'll get a new recovery phrase to save. Only do this if you
        have genuinely lost both your recovery phrase and every other
        device.
      </div>
      <Show when={error() !== null}>
        <div class="form-error">{error()}</div>
      </Show>
      <button
        type="button"
        disabled={busy() || props.busy}
        onClick={() => void onCreate()}
      >
        {busy() ? "Creating new identity…" : "Create a new identity"}
      </button>
    </details>
  );
}
