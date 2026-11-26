// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - the RE-SEND MARKER.
 *
 * The founder of a successor one-to-one sends again, into it, its own
 * account's messages that reached only removed devices (`reset-repair.ts`
 * `messagesToResend`). Each re-sent message carries, inside the sealed
 * envelope, where it was originally sent: the ORIGINAL group id and the
 * original DS message id there (`attachments.ts` `composeResendEnvelope`).
 *
 * The person who reset holds no history, so for them the re-sent message is
 * simply a message. A device that DOES hold the original (another device of
 * the founder's own account) would otherwise show it twice: once in the old
 * segment, once in the successor. This rule is what makes it show once: the
 * successor row is not drawn, not stored, not counted and not notified, and
 * the history row stays where it is and is not replaced.
 *
 * The marker is a claim by the sender, so it is believed only against what
 * this device already holds: the named row must exist, in an OLDER segment
 * of the SAME conversation, and be by the SAME author account as the sender
 * the MLS layer authenticated. A marker that matches nothing is ignored and
 * the message renders as any other. Nothing is ever drawn for the marker
 * itself.
 *
 * Pure. iOS and Android mirror the table exactly (R.12).
 */

/** Where a re-sent message was originally sent. */
export interface ResendOrigin {
  /** The original conversation group's id, standard base64. */
  readonly groupIdB64: string;
  /** The original's DS `messageId` in that group. */
  readonly messageId: number;
}

export type ResendVerdict =
  /** An ordinary message: stored, shown, counted. */
  | "draw"
  /** This device already holds the original: the successor row is dropped
   *  before storage. */
  | "skip";

/**
 * | marker | origin group is an older segment of the receiving group's conversation | this device holds the named row | its author is the sender's account | verdict |
 * |--------|-------------------------------------------------------------------------|---------------------------------|------------------------------------|---------|
 * | none   | -                                                                       | -                               | -                                  | `draw`  |
 * | set    | no (another conversation, an unknown group, the same or a later group)  | -                               | -                                  | `draw`  |
 * | set    | yes                                                                     | no                              | -                                  | `draw`  |
 * | set    | yes                                                                     | yes                             | no                                 | `draw`  |
 * | set    | yes                                                                     | yes                             | yes                                | `skip`  |
 *
 * ADR-0101 walk 2026-11-26 (D3): the person who reset gets the history of a
 * returned conversation back from its contact, filed into the SUCCESSOR
 * under the original DS ids (DS ids are global, so the successor never
 * holds an id of the old group otherwise). A re-sent message whose named
 * row the RECEIVING group already holds that way, by the same author, is
 * the same message once more: `skip` (`heldInReceivedAuthor`). The reverse
 * order is the filer's: a history row whose id a held re-send names is not
 * filed ({@link resentOriginIds}).
 *
 * `heldAuthor` and `senderAuthor` are the same kind of token for one
 * account (the caller resolves a key through its aliases first); `null`
 * means "not held" / "sender not attributable" and never matches.
 */
export function resendVerdict(input: {
  readonly marker: ResendOrigin | undefined;
  /** The conversation the message was RECEIVED in. */
  readonly receivedConversation: string;
  /** The conversation the marker's group belongs to, or `null` when this
   *  device holds no such group. */
  readonly originConversation: string | null;
  /** Whether the marker's group comes BEFORE the receiving group in that
   *  conversation's segments. */
  readonly originIsOlderSegment: boolean;
  /** The author account of the row the marker names, or `null` when this
   *  device does not hold that row. */
  readonly heldAuthor: string | null;
  /** The account of the MLS-authenticated sender of the received message. */
  readonly senderAuthor: string | null;
  /** The author account of the row the marker names when the RECEIVING
   *  group holds it (a returned history's copy), or `null`. */
  readonly heldInReceivedAuthor?: string | null;
}): ResendVerdict {
  if (input.marker === undefined) return "draw";
  const copy = input.heldInReceivedAuthor ?? null;
  if (copy !== null && input.senderAuthor !== null && copy === input.senderAuthor) return "skip";
  if (input.originConversation === null) return "draw";
  if (input.originConversation !== input.receivedConversation) return "draw";
  if (!input.originIsOlderSegment) return "draw";
  if (input.heldAuthor === null || input.senderAuthor === null) return "draw";
  return input.heldAuthor === input.senderAuthor ? "skip" : "draw";
}

/** The slice of a stored row the rule reads. */
export interface HeldRowAuthor {
  readonly direction: "outgoing" | "incoming" | "system";
  readonly authorKeyB64?: string | null;
}

/**
 * The author account token of a stored row: this account's own key for a
 * row this device sent, the stored author key (through `canonical`) for a
 * received one, `null` for a system row or a row with no author.
 */
export function heldRowAuthor(
  row: HeldRowAuthor | undefined,
  ownPersonKeyB64: string | null,
  canonical: (keyB64: string) => string,
): string | null {
  if (row === undefined || row.direction === "system") return null;
  if (row.direction === "outgoing") return ownPersonKeyB64 === null ? null : canonical(ownPersonKeyB64);
  const key = row.authorKeyB64;
  return key === undefined || key === null || key.length === 0 ? null : canonical(key);
}

/** The slice of a stored row {@link resentOriginIds} reads. */
export interface ResentRow {
  readonly resendOf?: { readonly messageId: string };
}

/**
 * ADR-0101 walk 2026-11-26 (D3) - the original DS ids the held re-sends of a
 * slot name: a history row carrying one of them is the same message as a
 * re-send already shown, and is not filed again. Pure.
 */
export function resentOriginIds(rows: readonly ResentRow[]): Set<string> {
  const out = new Set<string>();
  for (const r of rows) {
    const id = r.resendOf?.messageId;
    if (typeof id === "string" && /^[0-9]+$/.test(id)) out.add(id);
  }
  return out;
}
