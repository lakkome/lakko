// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The post-signup "Save your recovery phrase" screen, as a guided flow
 * (docs/ux/client-redesign.md section 5.6):
 *
 *   1. the 12 numbered words in two columns, Copy, a short explanation and
 *      the three-word note (ADR-0049) when the vault enrolled;
 *   2. a check: three words at random positions, typed back from the notes,
 *      plus the acknowledgement, before Done.
 *
 * It is rendered by the route GUARDS, from `auth.signupPhrase()`, for as long
 * as that phrase is unacknowledged, whichever route the user is on and again
 * after a reload; the decision lives in `auth/phrase-gate.ts`. The picker and
 * the checker live in `phrase-confirm.ts`.
 */
import { createSignal, Show, type JSX } from "solid-js";
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
