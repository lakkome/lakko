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
/// ## Presence — what the reference can and cannot bind
///
///   * **Self** — this client is connected, so its own signature key is always
///     present.
///   * **Owner** — the owner's per-call `peer_tag`
///     (`sfuPeerTag(call_id, owner_account_id)`, ADR-0021) is re-derivable from
///     the capability's `owner_account_id`, so the owner is present iff that tag
///     appears in the current roster. This makes the common case (owner-present
///     → owner commits) and the reclaim case (owner returns → tag reappears →
///     committer reverts to the owner) fully correct.
///
/// A NON-owner member's leaf↔peer binding is the documented residual: a stranger
/// joiner only knows its own + the owner's account id, so it binds a non-owner
/// member opportunistically when it observes a relay FROM that peer
/// (``CallPresence/bindPeerToMember(peerId:memberKeyHex:)``). Per ADR-0032 this
/// is a low-severity residual (a redirected joiner lands in an isolated group,
/// never a plaintext or cross-group E2EE break).

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
    /// peer_id → peer_tag (empty string when the SFU emits no tag).
    private var roster: [String: String] = [:]
    /// peer_id → member signature-key hex, learned from a relay's `from`.
    private var peerToMember: [String: String] = [:]

    /// This client's own signature-key hex — always present.
    private let selfKeyHex: String
    /// The capability owner's signature-key hex.
    private let ownerKeyHex: String
    /// The owner's per-call `peer_tag` (`sfuPeerTag(call_id, owner_account_id)`),
    /// or `nil` when this client IS the owner (then owner-presence rides
    /// `selfKeyHex`).
    private let ownerPeerTag: String?

    init(selfKeyHex: String, ownerKeyHex: String, ownerPeerTag: String?) {
        self.selfKeyHex = selfKeyHex
        self.ownerKeyHex = ownerKeyHex
        self.ownerPeerTag = ownerPeerTag
    }

    /// Replace the roster from an SFU `Welcome` snapshot.
    func setRoster(_ entries: [(peerId: String, peerTag: String)]) {
        roster.removeAll(keepingCapacity: true)
        for entry in entries {
            roster[entry.peerId] = entry.peerTag
        }
    }

    /// A peer joined the SFU room.
    func addPeer(peerId: String, peerTag: String) {
        roster[peerId] = peerTag
    }

    /// A peer left the SFU room. Also drops any member binding it had.
    func removePeer(peerId: String) {
        roster.removeValue(forKey: peerId)
        peerToMember.removeValue(forKey: peerId)
    }

    /// Opportunistically bind a peer to a member signature key — learned when a
    /// relay arrives FROM that peer carrying a KeyPackage (committer side) or a
    /// Welcome (joiner side, binds the committer/owner peer).
    func bindPeerToMember(peerId: String, memberKeyHex: String) {
        peerToMember[peerId] = memberKeyHex
    }

    /// `true` iff the owner is currently present (this client is the owner, or
    /// the owner's peer_tag is in the roster). Load-bearing for the common case
    /// + reclaim.
    func ownerPresent() -> Bool {
        if ownerKeyHex == selfKeyHex { return true }
        guard let tag = ownerPeerTag, !tag.isEmpty else { return false }
        for value in roster.values where !value.isEmpty {
            if value == tag { return true }
        }
        return false
    }

    /// The present member signature-key set for the election: self, the owner
    /// (when present), plus every peer→member binding whose peer is in the
    /// current roster.
    func presentKeySet() -> Set<String> {
        var present: Set<String> = [selfKeyHex]
        if ownerPresent() { present.insert(ownerKeyHex) }
        for (peerId, memberKeyHex) in peerToMember where roster[peerId] != nil {
            present.insert(memberKeyHex)
        }
        return present
    }
}
