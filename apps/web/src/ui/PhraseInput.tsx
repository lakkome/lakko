// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery phrase as numbered slots, one per word (12, ADR-0072).
 *
 * Every slot suggests words from the BIP39 English list (`WordSuggest`, the app's own list) (the first four
 * letters of a word are unique, so four keystrokes usually settle it), space /
 * Enter move to the next slot, a paste of the whole phrase fills the slots
 * from the current one, and a word that is not in the list is outlined at
 * once. The value handed back is the space-joined phrase, so the callers'
 * parse (`RecoveryMnemonic.fromPhrase`, which also checks the checksum) is
 * unchanged. Only the whole phrase can restore anything: the words ARE the
 * identity seed, so there is nothing to check a subset against.
 *
 * The slots read across the rows (3 columns x 4 rows at phone width, 4 x 3
 * on a wide screen), the same order `PhraseGrid` shows the words in at
 * signup; the rule is in `phrase-grid.ts`.
 */
import { Index, type JSX } from "solid-js";
import { isBip39Word } from "../recovery/bip39-words";
import { RECOVERY_PHRASE_WORDS } from "../recovery/phrase-shape";
import { t } from "../l10n";
import { createWordSuggest } from "./WordSuggest";

export const PHRASE_WORDS = RECOVERY_PHRASE_WORDS;

export function PhraseInput(props: {
  value: () => string;
  setValue: (v: string) => void;
  disabled?: () => boolean;
  label?: string;
}): JSX.Element {
  const words = (): string[] => {
    const w = props
      .value()
      .split(/\s+/)
      .filter((x) => x.length > 0);
    while (w.length < PHRASE_WORDS) w.push("");
    return w.slice(0, PHRASE_WORDS);
  };
  const inputs: HTMLInputElement[] = [];
  const commit = (next: string[]): void => props.setValue(next.join(" ").trim());
  const focusSlot = (i: number): void => {
    const el = inputs[Math.max(0, Math.min(PHRASE_WORDS - 1, i))];
    el?.focus();
    el?.select();
  };
  const setWord = (i: number, raw: string): void => {
    const next = words();
    next[i] = raw.toLowerCase().replace(/[^a-z]/g, "");
    commit(next);
  };
  const onPaste = (i: number, e: ClipboardEvent): void => {
    const parts = (e.clipboardData?.getData("text") ?? "")
      .toLowerCase()
      .split(/[^a-z]+/)
      .filter((p) => p.length > 0);
    if (parts.length <= 1) return;
    e.preventDefault();
    const next = words();
    parts.forEach((p, k) => {
      if (i + k < PHRASE_WORDS) next[i + k] = p;
    });
    commit(next);
    focusSlot(i + parts.length);
  };
  const suggest = createWordSuggest({
    word: (i) => words()[i] ?? "",
    accept: (i, word) => {
      setWord(i, word);
      if (i < PHRASE_WORDS - 1) focusSlot(i + 1);
    },
  });
  const onKeyDown = (i: number, e: KeyboardEvent & { currentTarget: HTMLInputElement }): void => {
    if (suggest.onKeyDown(i, e)) return;
    if (e.key === " " || e.key === "Enter") {
      if (e.currentTarget.value.length > 0) {
        e.preventDefault();
        focusSlot(i + 1);
      } else if (e.key === " ") {
        e.preventDefault();
      }
    } else if (e.key === "Backspace" && e.currentTarget.value.length === 0 && i > 0) {
      e.preventDefault();
      focusSlot(i - 1);
    }
  };
  const filled = (): number => words().filter((w) => w.length > 0).length;

  return (
    <div class="lk-phrase-input">
      <div class="lk-phrase-head">
        <span>{props.label ?? t("recovery_phrase.label")}</span>
        <span class="form-info">
          {filled()} / {PHRASE_WORDS}
        </span>
      </div>
      <ol class="lk-phrase-grid">
        <Index each={words()}>
          {(w, i) => (
            <li>
              <input
                ref={(el) => {
                  inputs[i] = el;
                }}
                type="text"
                role="combobox"
                aria-autocomplete="list"
                aria-expanded={suggest.expanded(i)}
                aria-controls={suggest.controls(i)}
                aria-activedescendant={suggest.activeDescendant(i)}
                value={w()}
                classList={{ invalid: w().length > 0 && !isBip39Word(w()) }}
                autocomplete="off"
                autocapitalize="none"
                autocorrect="off"
                spellcheck={false}
                disabled={props.disabled?.() === true}
                aria-label={t("recovery_phrase.word_label", { position: String(i + 1) })}
                onInput={(e) => {
                  setWord(i, e.currentTarget.value);
                  suggest.onInput();
                }}
                onFocus={() => suggest.onFocus(i)}
                onBlur={() => suggest.onBlur(i)}
                onPaste={(e) => onPaste(i, e)}
                onKeyDown={(e) => onKeyDown(i, e)}
              />
              <suggest.List slot={i} />
            </li>
          )}
        </Index>
      </ol>
    </div>
  );
}
