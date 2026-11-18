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
 * from nothing else: dismissed or accepted, the row goes back to what the
 * check says. A decline is silent, so a person asked earlier keeps the line;
 * once that request has stood unanswered for a day (the one ask-again rule,
 * `../chat/ask-again.ts`), the row also offers **Ask again**, which sends a
 * new request in one tap. Right after Add contact it does not: the button goes
 * away. A refused or failed line lasts until the text changes.
 *
 * No Solid: the component keeps one of these per person and renders
 * {@link rowView}.
 */
import { askAgainState } from "../chat/ask-again";
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
  /** The people the Sent list itself holds, by account id, with when the
   *  latest request to them went out: a request that can be sent again once
   *  it is a day old. Someone added through their link is not among them:
   *  that request was the link's. */
  readonly askedAtMsByAccountIdHex: ReadonlyMap<string, number>;
}

/**
 * `invited` are the people added through their invite link who have not
 * answered yet: the request went out when **Add** was pressed, so a row for
 * them reads "Request sent" like any other and offers no second request.
 */
export function outgoingOf(
  sent: readonly SentRequestRecord[],
  invited: readonly { readonly accountIdHex: string; readonly signaturePubkeyB64: string }[] = [],
): OutgoingRequests {
  const keysB64 = new Set<string>();
  const accountIdsHex = new Set<string>();
  const askedAtMsByAccountIdHex = new Map<string, number>();
  for (const r of sent) {
    accountIdsHex.add(r.recipientAccountIdHex);
    const held = askedAtMsByAccountIdHex.get(r.recipientAccountIdHex);
    if (held === undefined || r.sentAtMs > held) askedAtMsByAccountIdHex.set(r.recipientAccountIdHex, r.sentAtMs);
    if (r.recipientKeyB64 !== undefined) keysB64.add(r.recipientKeyB64);
  }
  for (const p of invited) {
    accountIdsHex.add(p.accountIdHex);
    keysB64.add(p.signaturePubkeyB64);
    askedAtMsByAccountIdHex.delete(p.accountIdHex);
  }
  return { keysB64, accountIdsHex, askedAtMsByAccountIdHex };
}

/** Whether a request to this person is waiting. By account id too: their key
 *  may have rotated since it went out. */
export function isOutgoing(person: PersonResult, outgoing: OutgoingRequests): boolean {
  return (
    outgoing.keysB64.has(person.keyB64) ||
    (person.accountIdHex !== null && outgoing.accountIdsHex.has(person.accountIdHex))
  );
}

/** How a row's request stands: `sent` while the person is in the Sent list,
 *  except that a new request of this session speaks for itself until it
 *  settled: a repeat on its way, or one that failed. */
export function rowSendState(
  person: PersonResult,
  held: RowSendState,
  outgoing: OutgoingRequests,
): RowSendState {
  if (held === "sending" || held === "failed" || held === "refused") return held;
  return isOutgoing(person, outgoing) ? "sent" : "idle";
}

/** Whether a row reading Request sent offers to ask again: the request in
 *  the Sent list has stood unanswered for a day (`docs/ux/who-can-find-me.md`
 *  section 6.1: right after Add contact the row reads Request sent and the
 *  button goes away). */
export function mayAskAgain(person: PersonResult, outgoing: OutgoingRequests, nowMs: number): boolean {
  if (person.accountIdHex === null) return false;
  const sentAtMs = outgoing.askedAtMsByAccountIdHex.get(person.accountIdHex);
  return sentAtMs !== undefined && askAgainState(sentAtMs, nowMs) === "no-answer";
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
  /** Beside Request sent: Ask again. */
  readonly askAgain: boolean;
  /** The line under the row, or `null`. */
  readonly message: { readonly error: boolean; readonly text: () => string } | null;
}

/** What a row shows for its state; `canAdd` is whether it offers Add contact
 *  at all (`canAddContact`), `asked` whether the request standing is one that
 *  can be sent again ({@link mayAskAgain}). A refused row keeps its sentence
 *  after the refusal took the offer away (`reach-verdicts.ts`). */
export function rowView(state: RowSendState, canAdd: boolean, asked = false): RowView {
  if (state === "refused") {
    return { add: "hidden", sent: false, askAgain: false, message: { error: true, text: REQUEST_REFUSED } };
  }
  // Before `canAdd`: whoever the Sent list holds reads Request sent, whatever
  // the last check said about reaching them again. Asking again needs a way
  // to reach them.
  if (state === "sent") return { add: "hidden", sent: true, askAgain: canAdd && asked, message: null };
  if (!canAdd) return { add: "hidden", sent: false, askAgain: false, message: null };
  switch (state) {
    case "idle":
      return { add: "shown", sent: false, askAgain: false, message: null };
    case "sending":
      return { add: "disabled", sent: false, askAgain: false, message: null };
    case "failed":
      return { add: "shown", sent: false, askAgain: false, message: { error: true, text: REQUEST_FAILED } };
  }
}
