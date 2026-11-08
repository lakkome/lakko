// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a publisher invitation card in a one-to-one offers (UX
 * `docs/ux/group-and-channel-privacy.md` 15.2 and 15.2.2): `Accept` sends
 * the request from the card itself ("it applies with no further step"); the
 * waiting lives on the card, never in `Requests sent`. Pure, so the rules
 * are asserted without a component.
 */

/** UX 15.2.2: after 7 days a request reads "No answer yet." with
 *  `Ask again`; a device asks again at most once per channel per 7 days. */
export const PUBLISHER_ASK_AGAIN_AFTER_MS = 7 * 24 * 60 * 60 * 1000;

/** The card's control. */
export type PublisherCardControl =
  /** This account already posts here: `Open`. */
  | "open"
  /** Sending (the first `Accept` or `Ask again`): the button, disabled,
   *  reads the waiting line. */
  | "busy"
  /** Sent, under 7 days: the waiting line, no button (never withdrawn). */
  | "waiting"
  /** Sent 7 days ago or more: "No answer yet." and `Ask again`. */
  | "no-answer"
  /** `Ask again` found the link no longer works: the invalid-link line, for
   *  good. The earlier request still lands if the owner approves it. */
  | "dead"
  /** `Accept`. */
  | "accept";

/** The part of a pending request the card reads. */
export interface PendingRequestView {
  /** When the request was last sent (the first `Accept` or `Ask again`). */
  readonly requestedAtMs: number;
  /** `Ask again` found the link dead. */
  readonly linkDead?: boolean;
}

export function publisherCardControl(input: {
  readonly role: "owner" | "publisher" | "subscriber" | null;
  readonly busy: boolean;
  readonly pending: PendingRequestView | null;
  readonly nowMs: number;
}): PublisherCardControl {
  if (input.role === "owner" || input.role === "publisher") return "open";
  if (input.busy) return "busy";
  const p = input.pending;
  if (p === null) return "accept";
  if (p.linkDead === true) return "dead";
  return input.nowMs - p.requestedAtMs >= PUBLISHER_ASK_AGAIN_AFTER_MS ? "no-answer" : "waiting";
}

/** What `Ask again` came to, and what the card does with it. */
export type AskAgainOutcome =
  /** The link opened and the request went out again: the record is new and
   *  the 7 days start again. */
  | { readonly kind: "sent" }
  /** Approved meanwhile: open the channel. */
  | { readonly kind: "joined" }
  /** The link no longer works: the earlier record stays, marked dead. */
  | { readonly kind: "dead" }
  /** No network: the line under the card; `Ask again` stays. */
  | { readonly kind: "offline" }
  /** Anything else: the generic line; `Ask again` stays. */
  | { readonly kind: "failed" };

/** Map a refusal code of the link open or the request to the outcome. */
export function askAgainFailure(code: string | null): AskAgainOutcome {
  if (code === "link-invalid" || code === "link-damaged") return { kind: "dead" };
  if (code === "offline") return { kind: "offline" };
  return { kind: "failed" };
}
