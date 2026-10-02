// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Three numbered word fields for the ADR-0049 three-word recovery.
 *
 * Each field is labelled with the position the server asked for (say 2, 7
 * and 11), autocompletes from the BIP39 English list, refuses a word that is
 * not in it (red outline, and the caller's Continue stays disabled), and a
 * paste of the whole 12-word phrase into any field fills the three positions
 * from it, unless `allowPhrasePaste` is false (the signup check, where the
 * words must come from the user's notes). The value is the three words in
 * field order.
 */
import { Index, type JSX } from "solid-js";
import { BIP39_ENGLISH, isBip39Word } from "../recovery/bip39-words";
import { RECOVERY_PHRASE_WORDS } from "../recovery/phrase-shape";
import { t } from "../l10n";

export function WordSlots(props: {
  positions: () => readonly number[];
  value: () => readonly string[];
  setValue: (v: string[]) => void;
  disabled?: () => boolean;
  /** Fill the slots from a pasted whole phrase (default true). */
  allowPhrasePaste?: boolean;
}): JSX.Element {
  const inputs: HTMLInputElement[] = [];
  const words = (): string[] => {
    const w = [...props.value()];
    while (w.length < props.positions().length) w.push("");
    return w.slice(0, props.positions().length);
  };
  const setWord = (i: number, raw: string): void => {
    const next = words();
    next[i] = raw.toLowerCase().replace(/[^a-z]/g, "");
    props.setValue(next);
  };
  const focusSlot = (i: number): void => {
    const el = inputs[Math.max(0, Math.min(inputs.length - 1, i))];
    el?.focus();
    el?.select();
  };
  const onPaste = (e: ClipboardEvent): void => {
    if (props.allowPhrasePaste === false) return;
    const parts = (e.clipboardData?.getData("text") ?? "")
      .toLowerCase()
      .split(/[^a-z]+/)
      .filter((p) => p.length > 0);
    if (parts.length !== RECOVERY_PHRASE_WORDS) return;
    e.preventDefault();
    props.setValue(props.positions().map((p) => parts[p - 1] ?? ""));
  };
  const onKeyDown = (i: number, e: KeyboardEvent & { currentTarget: HTMLInputElement }): void => {
    if ((e.key === " " || e.key === "Enter") && i < inputs.length - 1 && e.currentTarget.value.length > 0) {
      e.preventDefault();
      focusSlot(i + 1);
    } else if (e.key === " ") {
      e.preventDefault();
    } else if (e.key === "Backspace" && e.currentTarget.value.length === 0 && i > 0) {
      e.preventDefault();
      focusSlot(i - 1);
    }
  };

  return (
    <div class="lk-word-slots">
      <Index each={props.positions()}>
        {(pos, i) => (
          <label class="lk-word-slot">
            <span class="lk-word-slot-num">{pos()}</span>
            <input
              ref={(el) => {
                inputs[i] = el;
              }}
              type="text"
              list="lk-bip39-words-3"
              value={words()[i] ?? ""}
              classList={{
                invalid: (words()[i] ?? "").length > 0 && !isBip39Word(words()[i] ?? ""),
              }}
              autocomplete="off"
              autocapitalize="none"
              autocorrect="off"
              spellcheck={false}
              disabled={props.disabled?.() === true}
              aria-label={t("recovery_phrase.word_label", { position: String(pos()) })}
              placeholder={t("recovery_phrase.word_placeholder", { position: String(pos()) })}
              onInput={(e) => setWord(i, e.currentTarget.value)}
              onPaste={onPaste}
              onKeyDown={(e) => onKeyDown(i, e)}
            />
          </label>
        )}
      </Index>
      <datalist id="lk-bip39-words-3">
        <Index each={BIP39_ENGLISH}>{(w) => <option value={w()} />}</Index>
      </datalist>
    </div>
  );
}
