// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0032 committer election + presence tracking (pure, no I/O) — the iOS
/// mirror of `apps/web/src/calls/multi-joiner-election.ts`.
///
/// The multi-joiner call link (ADR-0032, G-21/G-22) survives the owner leaving
/// mid-call because the acting committer is a **pure function of public state**
/// — every member computes the same answer locally, no election protocol:
///
///   > committer = the owner if the owner is present (connected to the SFU
///   > room); otherwise the present member with the lowest MLS leaf index.
///
/// The pure decision lives in the Rust core (`electCommitter`, UniFFI-exposed);
/// this module is the thin glue that (a) builds the `CommitterCandidate`s it
/// consumes from a snapshot of `Group.members()` + the owner's known signature
/// key + the present-key set, and (b) computes that present-key set from what
/// the client can observe on the SFU roster.
///
/// ## Presence — how a peer is bound to a member
///
///   * **Self** — this client is connected, so its own signature key is always
///     present.
///   * **Every other member, the owner included** — each participant announces
///     its own SFU peer id, signed with its MLS leaf key and sealed under the
///     call seed (ADR-0078 section 2, ``CallRelay``); a verified announcement
///     binds that peer to that leaf
///     (``CallPresence/bindPeerToMember(peerId:memberKeyHex:)``). The owner is
///     present iff a peer in the roster announced the capability's owner key.
///     This replaces the ADR-0021 peer tag the SFU used to send; a member that
///     has not announced yet is treated as absent until it does.

/// Lowercase-hex of a byte slice — the canonical key form used everywhere in
/// the ADR-0032 orchestration (leaf indices renumber across removes; signature
/// keys do not).
func hexEncode(_ data: Data) -> String {
    var out = ""
    out.reserveCapacity(data.count * 2)
    for byte in data {
        out += String(format: "%02x", byte)
    }
    return out
}

/// One member of the call MLS group, projected for the election.
struct ElectionMember {
    /// OpenMLS leaf index at the current epoch.
    let leafIndex: UInt32
    /// Lowercase-hex of the member's 32-byte Ed25519 signature key.
    let signatureKeyHex: String
}

/// Compute the acting committer's leaf index via the Rust election, or `nil`
/// when no member is present. Deterministic given identical inputs on every
/// client — the property the ADR relies on for split-brain-free failover.
func electActingCommitterLeaf(
    members: [ElectionMember],
    ownerKeyHex: String,
    presentKeys: Set<String>
) -> UInt32? {
    guard !members.isEmpty else { return nil }
    let candidates = members.map { member in
        CommitterCandidate(
            leafIndex: member.leafIndex,
            isOwner: member.signatureKeyHex == ownerKeyHex,
            isPresent: presentKeys.contains(member.signatureKeyHex)
        )
    }
    return electCommitter(candidates: candidates)
}

/// Tracks which MLS members are present (connected to the SFU room) from the
/// observable roster, and produces the present-key set the election consumes.
/// See the module doc for the binding model.
///
/// Accessed only from the `@MainActor`-isolated ``MultiJoinerCall``, so it needs
/// no internal synchronisation.
final class CallPresence {
    /// The peer ids currently in the SFU room.
    private var roster: Set<String> = []
    /// peer_id → member signature-key hex, from a verified announcement
    /// (ADR-0078 section 2) or the committer's own admission of a KeyPackage
    /// relayed by that peer.
    private var peerToMember: [String: String] = [:]

    /// This client's own signature-key hex — always present.
    private let selfKeyHex: String
    /// The capability owner's signature-key hex.
    private let ownerKeyHex: String

    init(selfKeyHex: String, ownerKeyHex: String) {
        self.selfKeyHex = selfKeyHex
        self.ownerKeyHex = ownerKeyHex
    }

    /// Replace the roster from an SFU `Welcome` snapshot.
    func setRoster(_ peerIds: [String]) {
        roster = Set(peerIds)
    }

    /// A peer joined the SFU room.
    func addPeer(peerId: String) {
        roster.insert(peerId)
    }

    /// The member signature-key hex bound to a peer, or `nil` when this client
    /// never learnt who that peer is. Read it BEFORE `removePeer`, which forgets
    /// the binding: it is what lets the committer remove a departed member's
    /// leaf and forget its admission so the person can come back.
    func memberKeyFor(peerId: String) -> String? {
        peerToMember[peerId]
    }

    /// A peer left the SFU room. Also drops any member binding it had.
    func removePeer(peerId: String) {
        roster.remove(peerId)
        peerToMember.removeValue(forKey: peerId)
    }

    /// Bind a peer to a member signature key: from that peer's verified
    /// announcement, or (committer side) from the KeyPackage it relayed and
    /// this client admitted. A later announcement replaces it.
    func bindPeerToMember(peerId: String, memberKeyHex: String) {
        peerToMember[peerId] = memberKeyHex
    }

    /// `true` iff the owner is currently present: this client is the owner, or
    /// a peer in the roster announced the owner's key. Load-bearing for the
    /// common case + reclaim.
    func ownerPresent() -> Bool {
        if ownerKeyHex == selfKeyHex { return true }
        return peerToMember.contains { peerId, memberKeyHex in
            memberKeyHex == ownerKeyHex && roster.contains(peerId)
        }
    }

    /// The present member signature-key set for the election: self, the owner
    /// (when present), plus every peer→member binding whose peer is in the
    /// current roster.
    func presentKeySet() -> Set<String> {
        var present: Set<String> = [selfKeyHex]
        if ownerPresent() { present.insert(ownerKeyHex) }
        for (peerId, memberKeyHex) in peerToMember where roster.contains(peerId) {
            present.insert(memberKeyHex)
        }
        return present
    }
}
