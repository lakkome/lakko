// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A found person's row after **Add contact** (`docs/ux/who-can-find-me.md`
 * section 6.1): the tap sends the request at once, then the row reads
 * **Request sent** and the button goes away; a refusal says they may have
 * changed who can find them (no button: they cannot be reached this way);
 * any other failure says so and gives the button back.
 *
 * No Solid: the component keeps one of these per person and renders
 * {@link rowView}.
 */
import { REQUEST_FAILED, REQUEST_REFUSED } from "./search-state";
import type { SendOutcome } from "./send-request";

export type RowSendState = "idle" | "sending" | "sent" | "refused" | "failed";

/** The row state a send ended in. */
export function rowStateAfter(outcome: SendOutcome): RowSendState {
  switch (outcome.kind) {
    case "sent":
      return "sent";
    case "refused":
      return "refused";
    case "failed":
      return "failed";
  }
}

export interface RowView {
  /** The Add contact button: shown, shown but disabled (sending), or gone. */
  readonly add: "shown" | "disabled" | "hidden";
  /** The row reads "Request sent" in the button's place. */
  readonly sent: boolean;
  /** The line under the row, or `null`. */
  readonly message: { readonly error: boolean; readonly text: () => string } | null;
}

/** What a row shows for its state; `canAdd` is whether it offers Add contact
 *  at all (`canAddContact`). A refused row keeps its sentence after the
 *  refusal took the offer away (`reach-verdicts.ts`). */
export function rowView(state: RowSendState, canAdd: boolean): RowView {
  if (state === "refused") {
    return { add: "hidden", sent: false, message: { error: true, text: REQUEST_REFUSED } };
  }
  if (!canAdd) return { add: "hidden", sent: false, message: null };
  switch (state) {
    case "idle":
      return { add: "shown", sent: false, message: null };
    case "sending":
      return { add: "disabled", sent: false, message: null };
    case "sent":
      return { add: "hidden", sent: true, message: null };
    case "failed":
      return { add: "shown", sent: false, message: { error: true, text: REQUEST_FAILED } };
  }
}
