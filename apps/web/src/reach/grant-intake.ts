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
 *
 * ADR-0078: before any grant is kept, its mailbox binding is verified against
 * the sender's account key resolved at receipt (and the proven keys of that
 * person's chain): {@link grantsWithVerifiedBindings}. A grant whose binding
 * does not verify is dropped, never filed, so no later write is made under a
 * mailbox the sender cannot show is theirs.
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

/** The part of the wasm surface the binding check needs, structurally. */
export interface GrantBindingVerifier {
  decodeReachGrant(bytes: Uint8Array): {
    mailboxPkVerified(ownerAccountPk: Uint8Array): Uint8Array;
    free(): void;
  };
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * ADR-0078 - the grants of one received profile whose mailbox binding
 * verifies against one of `personKeys` (the sender's account key resolved at
 * receipt, C-68, then the proven keys of their chain). Order is kept; a grant
 * that does not decode or does not verify is dropped and logged without
 * identifiers. Never throws.
 */
export function grantsWithVerifiedBindings(
  wasm: GrantBindingVerifier,
  grants: readonly ReceivedGrantWire[],
  personKeys: readonly Uint8Array[],
): ReceivedGrantWire[] {
  const out: ReceivedGrantWire[] = [];
  let refused = 0;
  for (const g of grants) {
    let grant: ReturnType<GrantBindingVerifier["decodeReachGrant"]>;
    try {
      grant = wasm.decodeReachGrant(base64ToBytes(g.wireB64));
    } catch {
      refused += 1;
      continue;
    }
    try {
      let ok = false;
      for (const key of personKeys) {
        try {
          grant.mailboxPkVerified(key);
          ok = true;
          break;
        } catch {
          /* not this key */
        }
      }
      if (ok) out.push(g);
      else refused += 1;
    } finally {
      try {
        grant.free();
      } catch {
        /* already freed */
      }
    }
  }
  if (refused > 0) {
    // eslint-disable-next-line no-console
    console.warn(`ADR-0078: ${refused} profile grant(s) not kept; their mailbox binding did not verify`);
  }
  return out;
}

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
