// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A found person's row after **Add contact** (`docs/ux/who-can-find-me.md`
 * section 6.1): the tap sends the request at once, then the row reads
 * **Request sent** and the button goes away; a refusal says they may have
 * changed who can find them (no button: they cannot be reached this way);
 * any other failure says so and gives the button back.
 *
 * A row says what is true now. **Request sent** comes from the Sent list and
 * from nothing else: declined and dismissed, or accepted, the row goes back to
 * what the check says. A refused or failed line lasts until the text changes.
 *
 * No Solid: the component keeps one of these per person and renders
 * {@link rowView}.
 */
import type { SentRequestRecord } from "../contacts/contact-list-model";
import type { PersonResult } from "./people";
import { REQUEST_FAILED, REQUEST_REFUSED } from "./search-state";
import type { SendOutcome } from "./send-request";

export type RowSendState = "idle" | "sending" | "sent" | "refused" | "failed";

/** What a row keeps in memory after a send. `sent` is not kept: the row reads
 *  that from the Sent list ({@link rowSendState}). */
export function heldStateAfter(outcome: SendOutcome): RowSendState {
  switch (outcome.kind) {
    case "sent":
      return "idle";
    case "refused":
      return "refused";
    case "failed":
      return "failed";
  }
}

/** Who the Sent list (Contacts > Sent) holds. */
export interface OutgoingRequests {
  readonly keysB64: ReadonlySet<string>;
  readonly accountIdsHex: ReadonlySet<string>;
}

export function outgoingOf(sent: readonly SentRequestRecord[]): OutgoingRequests {
  const keysB64 = new Set<string>();
  const accountIdsHex = new Set<string>();
  for (const r of sent) {
    accountIdsHex.add(r.recipientAccountIdHex);
    if (r.recipientKeyB64 !== undefined) keysB64.add(r.recipientKeyB64);
  }
  return { keysB64, accountIdsHex };
}

/** Whether a request to this person is waiting. By account id too: their key
 *  may have rotated since it went out. */
export function isOutgoing(person: PersonResult, outgoing: OutgoingRequests): boolean {
  return (
    outgoing.keysB64.has(person.keyB64) ||
    (person.accountIdHex !== null && outgoing.accountIdsHex.has(person.accountIdHex))
  );
}

/** How a row's request stands: `sent` exactly while the person is in the Sent
 *  list. */
export function rowSendState(
  person: PersonResult,
  held: RowSendState,
  outgoing: OutgoingRequests,
): RowSendState {
  if (isOutgoing(person, outgoing)) return "sent";
  return held === "sent" ? "idle" : held;
}

/** The text changed: a refused or failed line goes, so the row reflects the
 *  new check. A request on its way stays claimed. */
export function withoutRowLines(
  held: ReadonlyMap<string, RowSendState>,
): ReadonlyMap<string, RowSendState> {
  const kept = [...held].filter(([, state]) => state === "sending");
  return kept.length === held.size ? held : new Map(kept);
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
  // Before `canAdd`: whoever the Sent list holds reads Request sent, whatever
  // the last check said about reaching them again.
  if (state === "sent") return { add: "hidden", sent: true, message: null };
  if (!canAdd) return { add: "hidden", sent: false, message: null };
  switch (state) {
    case "idle":
      return { add: "shown", sent: false, message: null };
    case "sending":
      return { add: "disabled", sent: false, message: null };
    case "failed":
      return { add: "shown", sent: false, message: { error: true, text: REQUEST_FAILED } };
  }
}
