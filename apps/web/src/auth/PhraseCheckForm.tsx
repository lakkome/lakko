// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery-phrase check: three words at random positions, typed back
 * from the person's notes, and the acknowledgement. One form for the screen
 * after new words (`SignupPhrasePrompt`) and for "Save a recovery phrase" on
 * the words a device already holds (`SavePhrasePanel`). The decisions live in
 * `phrase-confirm.ts`.
 */
import { createSignal, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { WordSlots } from "../ui/WordSlots";
import {
  confirmationComplete,
  confirmationErrorMessage,
  confirmationMismatches,
  listPositions,
  pickConfirmationPositions,
} from "./phrase-confirm";

export function PhraseCheckForm(props: {
  readonly words: readonly string[];
  /** The step line above the lede, when the surface has one. */
  readonly stepLabel?: string;
  readonly submitLabel: string;
  /** Runs when the three words match. */
  readonly onConfirmed: () => void;
  readonly onBack: () => void;
  /** A failure of what `onConfirmed` started. */
  readonly failure?: string | null;
  /** What `onConfirmed` started is running: the buttons wait. */
  readonly busy?: boolean;
  /** Drawn above the buttons (the proof block). */
  readonly children?: JSX.Element;
}): JSX.Element {
  const [positions] = createSignal<number[]>(pickConfirmationPositions(props.words.length));
  const [typed, setTyped] = createSignal<string[]>(["", "", ""]);
  const [acknowledged, setAcknowledged] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  function onSubmit(e: SubmitEvent): void {
    e.preventDefault();
    const misses = confirmationMismatches(props.words, positions(), typed());
    if (misses.length > 0) {
      setError(confirmationErrorMessage(misses));
      setTyped(["", "", ""]);
      return;
    }
    setError(null);
    props.onConfirmed();
  }

  return (
    <>
      <Show when={props.stepLabel}>{(label) => <p class="lk-auth-step">{label()}</p>}</Show>
      <p class="lk-auth-lede">
        {t("recovery_phrase.check.lede", { positions: listPositions(positions()) })}
      </p>
      <form onSubmit={onSubmit}>
        <WordSlots positions={positions} value={typed} setValue={setTyped} allowPhrasePaste={false} />
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
        {props.children}
        <Show when={(props.failure ?? null) !== null}>
          <div class="form-error" role="alert">
            {props.failure}
          </div>
        </Show>
        <button
          class="primary"
          type="submit"
          disabled={props.busy === true || !acknowledged() || !confirmationComplete(positions(), typed())}
        >
          {props.submitLabel}
        </button>
        <button type="button" class="ghost" disabled={props.busy === true} onClick={() => props.onBack()}>
          {t("recovery_phrase.check.back")}
        </button>
      </form>
    </>
  );
}
