// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One person, one one-to-one row.
 *
 * A device can hold two one-to-ones with the same person: it founded one
 * (A) whose Welcome the other person's device never opened, and later that
 * person founded another (B) in answer to a request this account sent (a link
 * answer, an Add). Both showed as rows in Chats (seen live 2026-11-07,
 * desktop and Pixel).
 *
 * The other person founding B settles it: a device founds a one-to-one in
 * answer to a request only when it holds none live with the requester
 * (ADR-0079 section 5, the restore step), so A is dead on their side and B
 * is where they read. B is therefore recorded as A's successor in the
 * conversation lineage (ADR-0073 R.5): one row, whose head is B, with A's
 * history above it. Unlike dropping A, this cannot split the two people when
 * both founded a one-to-one at about the same time: each side then heads the
 * group its peer founded.
 *
 * Only a one-to-one founded by the PEER continues one founded by THIS
 * account. Two one-to-ones founded by the same side are left as they are.
 *
 * Pure: the caller reads the rosters, founders and lineage.
 */
import { oneToOnePeerKey } from "../chat/person-key";

/** A conversation group this device holds, as the rule reads it. */
export interface HeldConversationGroup {
  /** Lower-case hex of the group id. */
  readonly groupHex: string;
  /** The conversation the lineage puts it in (its root's hex). */
  readonly conversationHex: string;
  /** Kind `chat` (ADR-0062). */
  readonly isOneToOne: boolean;
  /** The distinct people of the roster, base64 account keys. */
  readonly people: readonly string[];
  /** The founder's account key (base64), or `null` when unreadable. */
  readonly founderKeyB64: string | null;
}

/** One lineage register to write: `successorHex` continues `rootHex`. */
export interface DuplicateFold {
  readonly successorHex: string;
  readonly rootHex: string;
}

/**
 * The registers that fold each peer-founded one-to-one into the conversation
 * of an older one-to-one with the same person that this account founded.
 * Only a group that is its own conversation (no lineage yet) is folded; when
 * several own-founded one-to-ones with the person exist, the one whose
 * conversation id sorts first is the root, so every device of the account
 * writes the same register.
 */
export function duplicateOneToOneFolds(
  held: readonly HeldConversationGroup[],
  ownKeyB64: string | null,
): DuplicateFold[] {
  if (ownKeyB64 === null) return [];
  const peerOf = (g: HeldConversationGroup): string | null =>
    g.isOneToOne ? oneToOnePeerKey(g.people, ownKeyB64) : null;
  const out: DuplicateFold[] = [];
  for (const joined of held) {
    const peer = peerOf(joined);
    if (peer === null || joined.founderKeyB64 !== peer) continue;
    // Already part of a conversation (a successor, or a root others continue).
    if (joined.conversationHex !== joined.groupHex) continue;
    if (held.some((g) => g.groupHex !== joined.groupHex && g.conversationHex === joined.groupHex)) continue;
    const roots = held
      .filter(
        (g) =>
          g.groupHex !== joined.groupHex &&
          g.conversationHex !== joined.conversationHex &&
          g.founderKeyB64 === ownKeyB64 &&
          peerOf(g) === peer,
      )
      .map((g) => g.conversationHex)
      .sort();
    const root = roots[0];
    if (root !== undefined) out.push({ successorHex: joined.groupHex, rootHex: root });
  }
  return out;
}
