// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The post-signup "Save your recovery phrase" screen, as a guided flow
 * (docs/ux/client-redesign.md section 5.6):
 *
 *   1. the 24 numbered words in two columns, Copy, a short explanation, the
 *      three-word note (ADR-0049) when the vault enrolled, and the social
 *      recovery shares behind "More options";
 *   2. a check: three words at random positions, typed back from the notes,
 *      plus the acknowledgement, before Done.
 *
 * It is rendered by the route GUARDS, from `auth.signupPhrase()`, for as long
 * as that phrase is unacknowledged, whichever route the user is on and again
 * after a reload; the decision lives in `auth/phrase-gate.ts`. The picker and
 * the checker live in `phrase-confirm.ts`.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { useAuth } from "./AuthContext";
import { AuthLayout } from "../ui/AuthLayout";
import { PhraseGrid } from "../ui/PhraseGrid";
import { WordSlots } from "../ui/WordSlots";
import {
  confirmationComplete,
  confirmationErrorMessage,
  confirmationMismatches,
  listPositions,
  phraseWords,
  pickConfirmationPositions,
} from "./phrase-confirm";
import { t } from "../l10n";
import {
  DEFAULT_SHARE_COUNT,
  DEFAULT_THRESHOLD,
  splitMnemonic,
} from "../recovery/shamir";

type Step = "show" | "confirm";

/**
 * Render the pending signup phrase. Acknowledging it clears the phrase; the
 * guard that rendered this then re-decides what to show (the route it wraps,
 * or the `?return=` target on a public auth page).
 */
export default function SignupPhrasePrompt(): JSX.Element {
  const auth = useAuth();
  const words = (): string[] => phraseWords(auth.signupPhrase() ?? "");
  const [step, setStep] = createSignal<Step>("show");
  const [positions, setPositions] = createSignal<number[]>([]);
  const [typed, setTyped] = createSignal<string[]>(["", "", ""]);
  const [acknowledged, setAcknowledged] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  function toConfirm(): void {
    setPositions(pickConfirmationPositions(words().length));
    setTyped(["", "", ""]);
    setError(null);
    setStep("confirm");
  }

  function toWords(): void {
    setError(null);
    setStep("show");
  }

  function onDone(e: SubmitEvent): void {
    e.preventDefault();
    const misses = confirmationMismatches(words(), positions(), typed());
    if (misses.length > 0) {
      setError(confirmationErrorMessage(misses));
      setTyped(["", "", ""]);
      return;
    }
    auth.acknowledgeSignupPhrase();
  }

  return (
    <AuthLayout>
      <Show
        when={step() === "confirm"}
        fallback={
          <>
            <h1>{t("recovery_phrase.save.title")}</h1>
            <p class="lk-auth-step">{t("recovery_phrase.step", { current: 1, total: 2 })}</p>
            <p class="lk-auth-lede">{t("recovery_phrase.save.lede")}</p>
            <PhraseGrid words={words} highlight={() => auth.signupVaultPositions() ?? []} />
            <p class="form-info">{t("recovery_phrase.save.no_screenshot")}</p>
            <Show when={auth.signupVaultPositions()}>
              {(picked) => (
                <p class="form-info">
                  {t("recovery_phrase.save.vault_note", { positions: listPositions(picked()) })}
                </p>
              )}
            </Show>
            <details class="lk-disclosure">
              <summary>{t("common.more_options")}</summary>
              <div class="lk-disclosure-body">
                <SocialRecoveryPanel phrase={auth.signupPhrase() ?? ""} />
              </div>
            </details>
            <button class="primary" type="button" onClick={toConfirm}>
              {t("recovery_phrase.save.written")}
            </button>
          </>
        }
      >
        <h1>{t("recovery_phrase.check.title")}</h1>
        <p class="lk-auth-step">{t("recovery_phrase.step", { current: 2, total: 2 })}</p>
        <p class="lk-auth-lede">
          {t("recovery_phrase.check.lede", { positions: listPositions(positions()) })}
        </p>
        <form onSubmit={onDone}>
          <WordSlots
            positions={positions}
            value={typed}
            setValue={setTyped}
            allowPhrasePaste={false}
          />
          <Show when={error() !== null}>
            <div class="form-error" role="alert">
              {error()}
            </div>
          </Show>
          <label class="lk-check">
            <input
              type="checkbox"
              checked={acknowledged()}
              onChange={(e) => setAcknowledged(e.currentTarget.checked)}
            />
            <span>{t("recovery_phrase.check.acknowledge")}</span>
          </label>
          <button
            class="primary"
            type="submit"
            disabled={!acknowledged() || !confirmationComplete(positions(), typed())}
          >
            {t("common.done")}
          </button>
          <button type="button" class="ghost" onClick={toWords}>
            {t("recovery_phrase.check.back")}
          </button>
        </form>
      </Show>
    </AuthLayout>
  );
}

// ----------------------------------------------------------------------
// Social recovery shares (3 of 5)
// ----------------------------------------------------------------------

/**
 * Splits the phrase into 5 shares, any 3 of which rebuild it, for people the
 * user trusts. Behind "More options": the paper phrase stays the main path,
 * and the shares complement it rather than replace it.
 */
function SocialRecoveryPanel(props: { phrase: string }): JSX.Element {
  const [busy, setBusy] = createSignal(false);
  const [shares, setShares] = createSignal<string[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [copiedIndex, setCopiedIndex] = createSignal<number | null>(null);

  async function onGenerate(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      setShares(await splitMnemonic(props.phrase, DEFAULT_THRESHOLD, DEFAULT_SHARE_COUNT));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("social recovery: splitting the phrase failed", err);
      setError(t("recovery.shares.error.generic"));
    } finally {
      setBusy(false);
    }
  }

  async function onCopy(idx: number, value: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(value);
      setCopiedIndex(idx);
      setTimeout(() => setCopiedIndex((cur) => (cur === idx ? null : cur)), 1500);
    } catch {
      setError(t("recovery.shares.copy_failed"));
    }
  }

  return (
    <>
      <h3>{t("recovery.method.shares.title")}</h3>
      <p>
        {t("recovery.shares.panel_body", {
          count: DEFAULT_SHARE_COUNT,
          threshold: DEFAULT_THRESHOLD,
        })}
      </p>
      <Show
        when={shares().length > 0}
        fallback={
          <div class="lk-actions">
            <button type="button" class="small" onClick={() => void onGenerate()} disabled={busy()}>
              {busy()
                ? t("recovery.shares.generating")
                : t("recovery.shares.generate", { count: DEFAULT_SHARE_COUNT })}
            </button>
          </div>
        }
      >
        <ol class="lk-list tight">
          <For each={shares()}>
            {(share, i) => (
              <li class="lk-share-row">
                <span class="lk-share-num">{i() + 1}</span>
                <code class="lk-ellipsis" title={share}>
                  {share}
                </code>
                <button type="button" class="small" onClick={() => void onCopy(i(), share)}>
                  {copiedIndex() === i() ? t("common.copied") : t("common.copy")}
                </button>
              </li>
            )}
          </For>
        </ol>
        <p>
          {t("recovery.shares.usage", {
            method: t("recovery.method.shares.title"),
            threshold: DEFAULT_THRESHOLD,
          })}
        </p>
      </Show>
      <Show when={error() !== null}>
        <div class="form-error">{error()}</div>
      </Show>
    </>
  );
}
