// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4.1 "The sheet" — the two-row choice
 * behind the `Pin` / `Unpin` row: `Pin for me` and, when this device may set
 * a shared pin, `Pin for everyone` (`Pin for both` in a one-to-one), each
 * with one line under it saying what it does. Which rows appear is
 * `pin-sheet.ts`'s decision; this draws what it is handed, as a layer over
 * the chat (`ModalLayer`, the same idiom the conversation info uses).
 */

import { For, type JSX } from "solid-js";

import { ModalLayer } from "../ui/ModalLayer";
import type { PinOption } from "./pin-sheet";

export function PinSheet(props: {
  readonly open: boolean;
  readonly title: string;
  readonly options: readonly PinOption[];
  /** A row was chosen. Called BEFORE `onClose`, so a caller that keeps the
   *  sheet's subject in the same signal it clears on close still has it. */
  readonly onPick: (option: PinOption) => void;
  readonly onClose: () => void;
}): JSX.Element {
  return (
    <ModalLayer open={props.open} title={props.title} onClose={props.onClose}>
      <ul class="lk-list tight">
        <For each={props.options}>
          {(option) => (
            <li>
              <button
                type="button"
                class="lk-row lk-pin-option"
                onClick={() => {
                  props.onPick(option);
                  props.onClose();
                }}
              >
                <span class="lk-row-body">
                  <span class="lk-row-title">{option.label}</span>
                  <span class="lk-row-sub">{option.detail}</span>
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </ModalLayer>
  );
}
