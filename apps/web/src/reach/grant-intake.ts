// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - filing the reach grants a received profile carried.
 *
 * Called by the profile runtime for every profile that arrives in a
 * conversation, with the sender's account key resolved from the MLS sender
 * leaf at receipt (C-68). Each grant passes {@link acceptReceivedGrant}
 * first; a scoped grant the sender's previous profile in this scope carried
 * but this one does not is withdrawn (the sender narrowed their setting).
 */
import type { ReceivedGrantWire } from "../profile/profile-envelope";
import { acceptReceivedGrant, type GrantReceipt } from "./grant-rules";
import { reachClassName } from "./reach-model";
import {
  fileContactGrant,
  fileScopedGrant,
  withdrawMissingScopedGrants,
  type ReceivedGrants,
  type ScopedGrantClass,
} from "./received-grants";

/** The pure fold: the store after one profile's grants. */
export function foldProfileGrants(
  store: ReceivedGrants,
  args: {
    readonly senderHex: string;
    /** The account key the sender is ADDRESSED by now (resolved through the
     *  proven-predecessor table); the sender's own key when omitted. ADR-0074
     *  section 3.5a rule 6. */
    readonly addressHex?: string;
    readonly ownHex: string;
    readonly scopeHex: string;
    readonly oneToOne: boolean;
    readonly grants: readonly ReceivedGrantWire[];
    readonly nowMs: number;
  },
): ReceivedGrants {
  const receipt: GrantReceipt = {
    carrier: { kind: "profile", conversation: args.oneToOne ? "one-to-one" : "group" },
    senderAccountKeyHex: args.senderHex,
    ownAccountKeyHex: args.ownHex,
    // A profile that decrypted in this group came from a member of it at the
    // epoch it was sealed at.
    senderIsMember: true,
  };
  let next = store;
  const present = new Set<ScopedGrantClass>();
  for (const g of args.grants) {
    const cls = reachClassName(g.classByte);
    if (cls === null || !acceptReceivedGrant(cls, receipt)) continue;
    const held = { wireB64: g.wireB64, version: g.version, receivedAtMs: args.nowMs };
    if (cls === "contact") {
      next = fileContactGrant(next, args.senderHex, held, args.addressHex ?? args.senderHex);
    } else if (cls === "group" || cls === "member") {
      present.add(cls);
      next = fileScopedGrant(next, args.senderHex, args.scopeHex, cls, held, args.addressHex ?? args.senderHex);
    }
  }
  if (args.senderHex !== args.ownHex) {
    next = withdrawMissingScopedGrants(next, args.senderHex, args.scopeHex, present);
  }
  return next;
}
