// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 (client contract) - which reach grants a receiver keeps.
 *
 * A grant is a secret that lets its holder write to someone's mailbox, so a
 * receiver must never keep one that did not come from the person it names,
 * through the carrier that person uses for it:
 *
 * - `contact`: only from the OTHER participant of a one-to-one conversation;
 * - `group` / `member`: only from a member of the group whose profile carried
 *   it (and a `group` key only in a group, never in a one-to-one);
 * - `channel`: only from that channel's member cards (never a profile);
 * - `link`, `reply`, `fof`: never from a profile - they travel in a
 *   capability, a request or an introduction record.
 *
 * "From" is the sender's ACCOUNT key resolved from the MLS sender leaf at
 * receipt (C-68), never a leaf index and never a claim inside the envelope.
 * The kept grant is filed under that key, so a later lookup ("can I write to
 * this person?") asks for the key it resolved, not for whoever last held the
 * leaf.
 */
import type { ReachClassName } from "./reach-model";

/** Where a grant arrived. */
export type GrantCarrier =
  | { readonly kind: "profile"; readonly conversation: "one-to-one" | "group" }
  | { readonly kind: "member-card" };

/** The receipt facts a decision needs, all resolved by the receiver. */
export interface GrantReceipt {
  readonly carrier: GrantCarrier;
  /** The sender's account key resolved at receipt; `null` when it could not
   *  be resolved (the grant is then dropped). */
  readonly senderAccountKeyHex: string | null;
  /** This device's own account key. A grant from ourselves is never kept. */
  readonly ownAccountKeyHex: string;
  /** The sender is a current member of the group the carrier belongs to. */
  readonly senderIsMember: boolean;
}

/** Whether to keep a grant of class `cls` received as `receipt` says. */
export function acceptReceivedGrant(cls: ReachClassName, receipt: GrantReceipt): boolean {
  const sender = receipt.senderAccountKeyHex;
  if (sender === null || sender === receipt.ownAccountKeyHex) return false;
  const carrier = receipt.carrier;
  switch (cls) {
    case "contact":
      return (
        carrier.kind === "profile" &&
        carrier.conversation === "one-to-one" &&
        receipt.senderIsMember
      );
    case "member":
      return carrier.kind === "profile" && receipt.senderIsMember;
    case "group":
      return (
        carrier.kind === "profile" && carrier.conversation === "group" && receipt.senderIsMember
      );
    case "channel":
      return carrier.kind === "member-card";
    case "link":
    case "reply":
    case "fof":
      return false;
  }
}

/**
 * The grant a writer uses to reach a person, in order of preference: a
 * contact grant always works; otherwise the key of the scope the writer met
 * them in. `null` when nothing kept authorises the write (the caller then
 * writes unsigned, which an Everyone mailbox accepts).
 */
export function preferredGrantClass(
  held: ReadonlySet<ReachClassName>,
): ReachClassName | null {
  for (const cls of ["contact", "group", "channel", "fof", "member"] as const) {
    if (held.has(cls)) return cls;
  }
  return null;
}
