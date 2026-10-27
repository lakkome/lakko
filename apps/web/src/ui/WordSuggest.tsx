// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The BIP39 word suggestions under a recovery-phrase field (`PhraseInput`,
 * `WordSlots`): the app's own short list in place of the browser's
 * `<datalist>`, which each browser draws differently and a phone draws as a
 * system strip over the keyboard (design system section 6).
 *
 * The field is a combobox: the person types, up to five words starting with
 * what is typed appear under it, ArrowDown / ArrowUp move through them, Enter
 * takes the highlighted one and Escape hides the list until the next
 * keystroke. A click on a word takes it. Which words appear is
 * `suggestWords` in `picker.ts`, which has tests.
 */

import { type JSX, For, Show, createMemo, createSignal, createUniqueId } from "solid-js";

import { BIP39_ENGLISH } from "../recovery/bip39-words";
import { suggestWords } from "./picker";

/** One controller per group of slots; only the focused slot shows a list. */
export interface WordSuggest {
  /** The combobox ARIA state of slot `i`'s input (each read is reactive;
   *  the input also carries `role="combobox"` and
   *  `aria-autocomplete="list"`). */
  readonly expanded: (i: number) => boolean;
  readonly controls: (i: number) => string | undefined;
  readonly activeDescendant: (i: number) => string | undefined;
  /** Call from the slot's focus / blur / input handlers. */
  readonly onFocus: (i: number) => void;
  readonly onBlur: (i: number) => void;
  readonly onInput: () => void;
  /** Handles the key if it belongs to the list; returns `true` when it did. */
  readonly onKeyDown: (i: number, e: KeyboardEvent) => boolean;
  /** The list for slot `i`, rendered right after its input. */
  readonly List: (props: { slot: number }) => JSX.Element;
}

export function createWordSuggest(opts: {
  /** The current word in slot `i`. */
  readonly word: (i: number) => string;
  /** Put `word` into slot `i` (and move on, as the caller decides). */
  readonly accept: (i: number, word: string) => void;
}): WordSuggest {
  const id = createUniqueId();
  const listId = (i: number): string => `lk-word-suggest-${id}-${i}`;
  const optionId = (i: number, k: number): string => `lk-word-suggest-${id}-${i}-${k}`;

  const [focused, setFocused] = createSignal(-1);
  const [active, setActive] = createSignal(-1);
  const [hidden, setHidden] = createSignal(false);

  const words = createMemo((): string[] => {
    const i = focused();
    if (i < 0 || hidden()) return [];
    return suggestWords(opts.word(i), BIP39_ENGLISH);
  });
  const shownFor = (i: number): boolean => focused() === i && words().length > 0;

  const take = (i: number, word: string): void => {
    setActive(-1);
    setHidden(true);
    opts.accept(i, word);
  };

  return {
    expanded: (i) => shownFor(i),
    controls: (i) => (shownFor(i) ? listId(i) : undefined),
    activeDescendant: (i) => (shownFor(i) && active() >= 0 ? optionId(i, active()) : undefined),
    onFocus: (i) => {
      setFocused(i);
      setActive(-1);
      setHidden(false);
    },
    onBlur: (i) => {
      if (focused() === i) setFocused(-1);
    },
    onInput: () => {
      setActive(-1);
      setHidden(false);
    },
    onKeyDown: (i, e) => {
      const list = shownFor(i) ? words() : [];
      if (list.length === 0) return false;
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setActive((a) => Math.min(list.length - 1, a + 1));
          return true;
        case "ArrowUp":
          e.preventDefault();
          setActive((a) => Math.max(0, a - 1));
          return true;
        case "Enter": {
          const w = list[active()];
          if (w === undefined) return false;
          e.preventDefault();
          take(i, w);
          return true;
        }
        case "Escape":
          e.preventDefault();
          e.stopPropagation();
          setHidden(true);
          return true;
        default:
          return false;
      }
    },
    List: (props) => (
      <Show when={shownFor(props.slot)}>
        <div id={listId(props.slot)} class="lk-word-suggest" role="listbox">
          <For each={words()}>
            {(w, k) => (
              <div
                id={optionId(props.slot, k())}
                role="option"
                aria-selected={active() === k()}
                classList={{ active: active() === k() }}
                // Keep focus in the field, so the click does not blur it first.
                onPointerDown={(e) => e.preventDefault()}
                onClick={() => take(props.slot, w)}
              >
                {w}
              </div>
            )}
          </For>
        </div>
      </Show>
    ),
  };
}
