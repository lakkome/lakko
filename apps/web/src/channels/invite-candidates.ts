// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Who `Invite people` and `Add publisher` offer (UX
 * `docs/ux/group-and-channel-privacy.md` 14.1 and 15.2): the contacts, less
 * the people this device knows are in the channel already. Pure.
 *
 * Known to be in it: this account, the owner and every publisher (the
 * PublisherGroup's members), and, for `Invite people`, every subscriber who
 * shares a card this device could open and whose account binding verified.
 * A subscriber who shares no card cannot be known and stays on the list
 * (14.1: they can be invited and see `Open`).
 */
import type { AcceptedContact } from "../contacts/local-store";

/** `accepted` with every contact whose account key (current or an earlier
 *  one) is in `inChannelKeysB64` left out, in the order given. */
export function channelInviteCandidates(
  accepted: readonly AcceptedContact[],
  inChannelKeysB64: ReadonlySet<string>,
): AcceptedContact[] {
  if (inChannelKeysB64.size === 0) return [...accepted];
  return accepted.filter((c) => {
    if (inChannelKeysB64.has(c.signaturePubkeyB64)) return false;
    return !(c.previousSignaturePubkeysB64 ?? []).some((k) => inChannelKeysB64.has(k));
  });
}

/** The account keys known to be in the channel, from what each source
 *  holds; an absent source adds nothing. Pure. */
export function inChannelKeys(input: {
  readonly ownKeyB64: string | null;
  readonly publisherKeysB64: readonly string[];
  readonly memberKeysB64?: readonly (string | undefined)[];
}): Set<string> {
  const out = new Set<string>();
  if (input.ownKeyB64 !== null) out.add(input.ownKeyB64);
  for (const k of input.publisherKeysB64) out.add(k);
  for (const k of input.memberKeysB64 ?? []) {
    if (k !== undefined && k !== "self") out.add(k);
  }
  return out;
}
