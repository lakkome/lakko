// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0066 section 3.5, "Peers" — the proven-predecessor rule (Stage A).
//
// An account that changes its keys WITH a valid supersession proof (the
// ADR-0016 gate verified it under the pinned key and re-pinned) is the same
// person under a new key. Its conversation leaves keep their bindings under
// the OLD key until Stage B moves them, so the old key stays what those
// leaves are: the same person. Nothing is evicted, no chat closes, no
// successor is founded, and NOTHING IS SHOWN to anyone.
//
// An UNPROVEN change (a reset, attested or not; a refusal of the gate) never
// gets this treatment: its old key is a superseded key, handled by ADR-0065
// phase 2.
//
//   * `KeyChangeEvidence` / `ProvenPredecessorRule`   what earns the treatment;
//   * `ProvenPredecessors`                            the resolver: who is the
//                                                     same person across a
//                                                     proven rotation;
//   * `ProvenPredecessorStore`                        where it is kept, per
//                                                     signed-in account
//                                                     (public keys only).
//
// The same resolver serves the account's OWN old key on its own devices
// (``OwnAccountKeys``).

/// What stood behind a change of an account's published key.
public enum KeyChangeEvidence: Equatable {
    /// A supersession proof signed by the previously pinned key, verified by
    /// the ADR-0016 gate, naming exactly the new key. With or without a
    /// key-reset attestation beside it: what tells a rotation from a reset is
    /// the proof.
    case verifiedProof
    /// An attestation at a higher generation and no proof: a reset (or an
    /// account taken back). The old key is superseded, not a predecessor.
    case attestationOnly
    /// Nothing verifiable (no proof, a proof that did not verify, a proof
    /// naming another key).
    case none
}

/// Who earns predecessor treatment.
public enum ProvenPredecessorRule {

    public enum Treatment: Equatable {
        /// The old key is the proven predecessor of the new one: the same
        /// person. No flag, no banner, no closed chat, no eviction.
        case provenPredecessor
        /// The old key is NOT a predecessor. Whatever ADR-0065 phase 2 does
        /// for a reset applies; this rule adds nothing.
        case none
    }

    public static func treatment(for evidence: KeyChangeEvidence) -> Treatment {
        evidence == .verifiedProof ? .provenPredecessor : .none
    }

    /// Whether the ADR-0065 phase-2 "keys reset" state may be recorded for a
    /// key change: never for a proven rotation.
    public static func flagsAsReset(_ evidence: KeyChangeEvidence) -> Bool {
        evidence != .verifiedProof
    }
}

/// The resolver: `predecessor key -> the key that superseded it by proof`,
/// both as standard base64 of the 32-byte Ed25519 account key.
///
/// A value type so the rule is testable without a store; ``ProvenPredecessorStore``
/// persists one per signed-in account.
public struct ProvenPredecessors: Codable, Equatable {

    /// The longest chain followed. An account that rotated more often than
    /// this between two observations is not resolved further (chosen, not
    /// measured; each hop is one proven rotation).
    public static let maxChain = 16

    private(set) var successor: [String: String]

    public init(successor: [String: String] = [:]) {
        self.successor = successor
    }

    public var isEmpty: Bool { successor.isEmpty }

    /// Every key recorded as superseded by proof (sorted, for a stable order).
    public var predecessorKeysB64: [String] { successor.keys.sorted() }

    /// Record that `oldKeyB64` was superseded by `newKeyB64`, for
    /// `evidence`. Only a verified proof records anything; returns whether
    /// the map changed.
    ///
    /// Refused: a key over itself, and an edge that would close a cycle (a
    /// later proof leading back to a key already superseded: old keys do not
    /// come back, invariant #6).
    @discardableResult
    public mutating func record(
        oldKeyB64: String, newKeyB64: String, evidence: KeyChangeEvidence
    ) -> Bool {
        guard ProvenPredecessorRule.treatment(for: evidence) == .provenPredecessor else { return false }
        guard !oldKeyB64.isEmpty, !newKeyB64.isEmpty, oldKeyB64 != newKeyB64 else { return false }
        // `new` must not already resolve to `old`.
        guard current(of: newKeyB64) != oldKeyB64 else { return false }
        guard successor[oldKeyB64] != newKeyB64 else { return false }
        successor[oldKeyB64] = newKeyB64
        return true
    }

    /// The current key of the person `keyB64` belongs to: the end of the
    /// chain of proven rotations, or `keyB64` itself when it was never
    /// superseded by proof.
    public func current(of keyB64: String) -> String {
        var key = keyB64
        var hops = 0
        while let next = successor[key], hops < Self.maxChain {
            key = next
            hops += 1
        }
        return key
    }

    /// Whether two account keys are the same person across proven rotations.
    /// Two unrelated keys, and a key and its UNPROVEN replacement, never are.
    public func samePerson(_ a: String, _ b: String) -> Bool {
        a == b || current(of: a) == current(of: b)
    }

    /// Every key of the person `keyB64` belongs to: the current key first,
    /// then its proven predecessors (nearest first).
    public func allKeys(of keyB64: String) -> [String] {
        let head = current(of: keyB64)
        var keys = [head]
        var frontier = [head]
        var hops = 0
        while !frontier.isEmpty, hops < Self.maxChain {
            let next = successor.filter { frontier.contains($0.value) }.map(\.key).sorted()
            let fresh = next.filter { !keys.contains($0) }
            keys.append(contentsOf: fresh)
            frontier = fresh
            hops += 1
        }
        return keys
    }

    /// The proven predecessors of the person `keyB64` belongs to (without the
    /// current key).
    public func predecessors(of keyB64: String) -> [String] {
        Array(allKeys(of: keyB64).dropFirst())
    }

    /// A later RESET of the account whose current key is `currentKeyB64`
    /// makes every predecessor, proven or not, a superseded key (ADR-0066
    /// section 3.5): drop them all. Returns whether anything was dropped.
    @discardableResult
    public mutating func forgetPerson(currentKeyB64: String) -> Bool {
        let keys = Set(allKeys(of: currentKeyB64))
        let before = successor.count
        successor = successor.filter { !keys.contains($0.key) && !keys.contains($0.value) }
        return successor.count != before
    }
}

/// The per-account store of ``ProvenPredecessors``: one JSON map in
/// `UserDefaults`, public keys only. Cleared by the account wipe.
public final class ProvenPredecessorStore {

    private static let prefix = "me.lakko.contacts.proven-predecessors.v1."

    private let defaults: UserDefaults
    /// The account whose map this is; `nil` = the bound account, read at each
    /// call (the ``PeerPinStore`` rule).
    private let ownerAccountIdB64: String?

    public init(ownerAccountIdB64: String? = nil, defaults: UserDefaults = .standard) {
        self.ownerAccountIdB64 = ownerAccountIdB64
        self.defaults = defaults
    }

    private var storageKey: String? {
        (ownerAccountIdB64 ?? AcceptedContactsStore.boundAccountIdB64).map { Self.prefix + $0 }
    }

    public func load() -> ProvenPredecessors {
        guard let storageKey, let raw = defaults.data(forKey: storageKey),
              let map = try? JSONDecoder().decode(ProvenPredecessors.self, from: raw) else {
            return ProvenPredecessors()
        }
        return map
    }

    /// Record a proven rotation; returns whether the map changed. Posts
    /// ``changed`` when it did, so the name resolver re-reads.
    @discardableResult
    public func record(oldKeyB64: String, newKeyB64: String, evidence: KeyChangeEvidence) -> Bool {
        var map = load()
        guard map.record(oldKeyB64: oldKeyB64, newKeyB64: newKeyB64, evidence: evidence) else {
            return false
        }
        save(map)
        return true
    }

    /// A reset of the person holding `currentKeyB64`: their predecessors are
    /// superseded keys from now on.
    @discardableResult
    public func forgetPerson(currentKeyB64: String) -> Bool {
        var map = load()
        guard map.forgetPerson(currentKeyB64: currentKeyB64) else { return false }
        save(map)
        return true
    }

    private func save(_ map: ProvenPredecessors) {
        guard let storageKey else {
            NSLog("ProvenPredecessorStore: no account bound; nothing written")
            return
        }
        if map.isEmpty {
            defaults.removeObject(forKey: storageKey)
        } else if let data = try? JSONEncoder().encode(map) {
            defaults.set(data, forKey: storageKey)
        }
        NotificationCenter.default.post(name: Self.changed, object: nil)
    }

    public static func clear(ownerAccountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + ownerAccountIdB64)
    }

    /// Posted after the map changed.
    public static let changed = Notification.Name("me.lakko.contacts.proven-predecessors.changed")
}
