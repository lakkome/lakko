// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0032 committer election + presence tracking (pure, no I/O).
 *
 * The multi-joiner call link (ADR-0032, G-21/G-22) survives the owner
 * leaving mid-call because the acting committer is a **pure function of
 * public state** — every member computes the same answer locally, no
 * election protocol:
 *
 *   > committer = the owner if the owner is present (connected to the SFU
 *   > room); otherwise the present member with the lowest MLS leaf index.
 *
 * The pure decision lives in the Rust core (`electCommitter`, wasm-exposed
 * from Sprint 2); this module is the thin glue that (a) builds the three
 * parallel arrays it consumes from a snapshot of `Group.members()` + the
 * owner's known signature key + the present-key set, and (b) computes that
 * present-key set from what the client can observe on the SFU roster.
 *
 * ## Presence — how a peer is bound to a member
 *
 * The election needs "is member X present?" but the SFU roster is keyed on
 * an opaque per-connection `peer_id` (random per admission token, ADR-0078
 * section 2), not on an MLS leaf, and the SFU knows no account. Two
 * bindings exist:
 *
 *   * **Self** — this client is connected, so its own signature key is
 *     always present.
 *   * **Every other member, the owner included** — each participant
 *     announces its own peer id, signed with its MLS leaf key and sealed
 *     under the call seed (`call-relay.ts`); a verified announcement binds
 *     that peer to that leaf ({@link CallPresence.bindPeerToMember}). The
 *     owner is present iff a peer in the roster announced the capability's
 *     owner key. This replaces the ADR-0021 peer tag the SFU used to send,
 *     and makes the successor election as exact as the owner case: every
 *     member that announced is known, a member that has not is treated as
 *     absent until it does.
 */

/** Minimal view of the wasm `electCommitter` export (Sprint 2). */
export interface ElectCommitterFn {
  /** Returns the acting committer's leaf index, or `undefined` when no
   *  member is present. `is_owner` / `is_present` are 0/1 byte flags
   *  parallel to `leaf_indices`. */
  electCommitter(
    leafIndices: Uint32Array,
    isOwner: Uint8Array,
    isPresent: Uint8Array,
  ): number | undefined;
}

/** One member of the call MLS group, projected for the election. */
export interface ElectionMember {
  /** OpenMLS leaf index at the current epoch. */
  readonly leafIndex: number;
  /** Lowercase-hex of the member's 32-byte Ed25519 signature key —
   *  the stable identity handle used everywhere in this module (leaf
   *  indices renumber across removes; signature keys do not). */
  readonly signatureKeyHex: string;
}

/** Lowercase-hex of a byte slice — the canonical key form for the
 *  presence set + owner matching. */
export function hexKey(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Build the three parallel arrays `electCommitter` consumes from a member
 *  snapshot. Pure — no wasm — so the array construction is unit-testable on
 *  its own. `is_owner[i]` marks the member whose signature key equals the
 *  capability's owner key; `is_present[i]` marks members in `presentKeys`. */
export function buildElectionArrays(
  members: readonly ElectionMember[],
  ownerKeyHex: string,
  presentKeys: ReadonlySet<string>,
): {
  readonly leafIndices: Uint32Array;
  readonly isOwner: Uint8Array;
  readonly isPresent: Uint8Array;
} {
  const leafIndices = new Uint32Array(members.length);
  const isOwner = new Uint8Array(members.length);
  const isPresent = new Uint8Array(members.length);
  members.forEach((m, i) => {
    leafIndices[i] = m.leafIndex;
    isOwner[i] = m.signatureKeyHex === ownerKeyHex ? 1 : 0;
    isPresent[i] = presentKeys.has(m.signatureKeyHex) ? 1 : 0;
  });
  return { leafIndices, isOwner, isPresent };
}

/**
 * Compute the acting committer's leaf index via the Rust election, or
 * `undefined` when no member is present. Deterministic given identical
 * inputs on every client — the property the ADR relies on for
 * split-brain-free failover.
 */
export function electActingCommitterLeaf(
  wasm: ElectCommitterFn,
  members: readonly ElectionMember[],
  ownerKeyHex: string,
  presentKeys: ReadonlySet<string>,
): number | undefined {
  if (members.length === 0) return undefined;
  const { leafIndices, isOwner, isPresent } = buildElectionArrays(
    members,
    ownerKeyHex,
    presentKeys,
  );
  return wasm.electCommitter(leafIndices, isOwner, isPresent);
}

/**
 * Tracks which MLS members are present (connected to the SFU room) from
 * the observable roster, and produces the present-key set the election
 * consumes. See the module doc for the binding model.
 */
export class CallPresence {
  /** The peer ids currently in the SFU room. */
  private readonly roster = new Set<string>();
  /** peer_id → member signature-key hex, from a verified announcement (or
   *  the committer's own admission of a KeyPackage relayed by that peer). */
  private readonly peerToMember = new Map<string, string>();

  constructor(
    /** This client's own signature-key hex — always present. */
    private readonly selfKeyHex: string,
    /** The capability owner's signature-key hex. */
    private readonly ownerKeyHex: string,
  ) {}

  /** Replace the roster from an SFU `Welcome` snapshot. */
  setRoster(peerIds: readonly string[]): void {
    this.roster.clear();
    for (const id of peerIds) this.roster.add(id);
  }

  /** A peer joined the SFU room. */
  addPeer(peerId: string): void {
    this.roster.add(peerId);
  }

  /** The member signature-key hex bound to a peer, or `undefined` when this
   *  client never learnt who that peer is. Read it BEFORE `removePeer`, which
   *  forgets the binding: it is what lets the committer remove a departed
   *  member's leaf and forget its admission so the person can come back. */
  memberKeyFor(peerId: string): string | undefined {
    return this.peerToMember.get(peerId);
  }

  /** A peer left the SFU room. Also drops any member binding it had. */
  removePeer(peerId: string): void {
    this.roster.delete(peerId);
    this.peerToMember.delete(peerId);
  }

  /** Bind a peer to a member signature key: from that peer's verified
   *  announcement, or (committer side) from the KeyPackage it relayed and
   *  this client admitted. A later announcement replaces it. */
  bindPeerToMember(peerId: string, memberKeyHex: string): void {
    this.peerToMember.set(peerId, memberKeyHex);
  }

  /** `true` iff the owner is currently present: this client is the owner,
   *  or a peer in the roster announced the owner's key. Load-bearing for
   *  the common case + reclaim. */
  ownerPresent(): boolean {
    if (this.ownerKeyHex === this.selfKeyHex) return true;
    for (const [peerId, memberKeyHex] of this.peerToMember) {
      if (memberKeyHex === this.ownerKeyHex && this.roster.has(peerId)) return true;
    }
    return false;
  }

  /** The present member signature-key set for the election: self, the
   *  owner (when present), plus every peer→member binding whose peer is in
   *  the current roster. */
  presentKeySet(): Set<string> {
    const present = new Set<string>([this.selfKeyHex]);
    if (this.ownerPresent()) present.add(this.ownerKeyHex);
    for (const [peerId, memberKeyHex] of this.peerToMember) {
      if (this.roster.has(peerId)) present.add(memberKeyHex);
    }
    return present;
  }
}
