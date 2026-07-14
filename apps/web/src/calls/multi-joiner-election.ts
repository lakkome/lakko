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
 * ## Presence — what the reference can and cannot bind
 *
 * The election needs "is member X present?" but the SFU roster is keyed on
 * an opaque per-connection `peer_id`, not on an MLS leaf. Two bindings are
 * available WITHOUT any new wire (invariant #2):
 *
 *   * **Self** — this client is connected, so its own signature key is
 *     always present.
 *   * **Owner** — the owner's per-call `peer_tag`
 *     (`sfuPeerTag(call_id, owner_account_id)`, ADR-0021) is re-derivable
 *     from the capability's `owner_account_id`, so the owner is present iff
 *     that tag appears in the current roster. This makes the common case
 *     (owner-present → owner commits) and the reclaim case (owner returns →
 *     roster tag reappears → committer reverts to the owner) fully correct.
 *
 * A NON-owner member's leaf↔peer binding is the documented residual: a
 * stranger joiner only knows its own + the owner's account id, so it cannot
 * in general map another joiner's peer to that joiner's leaf. The reference
 * binds a non-owner member opportunistically when it observes a relay FROM
 * that peer ({@link CallPresence.bindPeerToMember}); members it has never
 * heard from are treated as absent for the successor election. Per ADR-0032
 * this is a low-severity residual (a redirected joiner lands in an isolated
 * group, never a plaintext or cross-group E2EE break); a strong-auth
 * successor delegation is deferred.
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
  /** peer_id → peer_tag (undefined when the SFU emits no tag). */
  private readonly roster = new Map<string, string | undefined>();
  /** peer_id → member signature-key hex, learned from a relay's `from`. */
  private readonly peerToMember = new Map<string, string>();

  constructor(
    /** This client's own signature-key hex — always present. */
    private readonly selfKeyHex: string,
    /** The capability owner's signature-key hex. */
    private readonly ownerKeyHex: string,
    /** The owner's per-call `peer_tag` (`sfuPeerTag(call_id,
     *  owner_account_id)`), or `null` when this client IS the owner (then
     *  owner-presence rides `selfKeyHex`). */
    private readonly ownerPeerTag: string | null,
  ) {}

  /** Replace the roster from an SFU `Welcome` snapshot. */
  setRoster(
    entries: readonly { peerId: string; peerTag?: string | undefined }[],
  ): void {
    this.roster.clear();
    for (const e of entries) this.roster.set(e.peerId, e.peerTag);
  }

  /** A peer joined the SFU room. */
  addPeer(peerId: string, peerTag?: string): void {
    this.roster.set(peerId, peerTag);
  }

  /** A peer left the SFU room. Also drops any member binding it had. */
  removePeer(peerId: string): void {
    this.roster.delete(peerId);
    this.peerToMember.delete(peerId);
  }

  /** Opportunistically bind a peer to a member signature key — learned
   *  when a relay arrives FROM that peer carrying a KeyPackage (committer
   *  side) or a Welcome (joiner side, binds the committer/owner peer). */
  bindPeerToMember(peerId: string, memberKeyHex: string): void {
    this.peerToMember.set(peerId, memberKeyHex);
  }

  /** `true` iff the owner is currently present (this client is the owner,
   *  or the owner's peer_tag is in the roster). Load-bearing for the
   *  common case + reclaim. */
  ownerPresent(): boolean {
    if (this.ownerKeyHex === this.selfKeyHex) return true;
    if (this.ownerPeerTag === null) return false;
    for (const tag of this.roster.values()) {
      if (tag === this.ownerPeerTag) return true;
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
