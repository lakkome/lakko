// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 8 — where a profile received in a conversation goes, and
// the profile that comes back after this account's key reset.
//
// Profile v6 is signed by its author's account identity key. A profile in a
// conversation is normally the sender's own: it is filed under the sender's
// key (resolved at receipt, C-68) only when its signature verifies under
// that key. After this account's key reset, a peer that returns a
// conversation also posts there the newest profile it holds for THIS person,
// signed by this account's previous key: that one is offered as the own
// profile coming back, never filed under the peer. Anything else is dropped.
//
// iOS mirror of `apps/web/src/profile/profile-signer.ts` and
// `apps/web/src/profile/returned-profile.ts`.

public enum ProfileRoute: Equatable, Sendable {
    case file
    case returnedOwn
    case drop
}

public enum ProfileReturn {
    /// | verifies under the sender | under this account's previous key | route       |
    /// |---------------------------|-----------------------------------|-------------|
    /// | yes                       | -                                 | file        |
    /// | no                        | yes                               | returnedOwn |
    /// | no                        | no                                | drop        |
    public static func route(verifiesUnderSender: Bool, verifiesUnderPreviousOwnKey: Bool) -> ProfileRoute {
        if verifiesUnderSender { return .file }
        return verifiesUnderPreviousOwnKey ? .returnedOwn : .drop
    }

    /// Whether `envelope` verifies under `keyB64` (never throws). `verify` is
    /// the core's `verifyProfileEnvelope`.
    public static func verifies(
        _ envelope: Data, underKeyB64 keyB64: String?,
        verify: (_ envelope: Data, _ identityPk: Data) throws -> FfiProfile = {
            try verifyProfileEnvelope(plaintext: $0, identityPk: $1)
        }
    ) -> Bool {
        guard let keyB64, let key = base64Decode(keyB64) else { return false }
        return (try? verify(envelope, key)) != nil
    }

    /// Whether a verified returned profile is adopted over what is held:
    /// strictly newer than the held stamp, and not zero.
    public static func adopts(candidateStamp: UInt64, heldStamp: UInt64) -> Bool {
        candidateStamp > heldStamp && candidateStamp > 0
    }

    /// The newest envelope held under any of a person's keys (a reset
    /// person's retired keys, newest stamp wins; a tie keeps the first
    /// key's). `nil` when none is held.
    public static func newestHeldEnvelope(
        keysB64: [String], heldEnvelopeOf: (String) -> (envelope: Data, updatedAtMs: UInt64)?
    ) -> Data? {
        var best: (envelope: Data, updatedAtMs: UInt64)?
        for key in keysB64 {
            guard let held = heldEnvelopeOf(key) else { continue }
            if best == nil || held.updatedAtMs > best!.updatedAtMs { best = held }
        }
        return best?.envelope
    }
}

/// The successors the newest profile of a reset person was already posted
/// into (``ContinuityPort/carryProfile``), per account. Bounded to 512.
public struct ProfileCarriedStore {
    static let prefix = "lakko.continuity-profile-carried.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func held(_ accountIdB64: String) -> [String] {
        defaults.stringArray(forKey: Self.prefix + accountIdB64) ?? []
    }

    public func carried(accountIdB64: String, successorGroupIdB64: String) -> Bool {
        held(accountIdB64).contains(successorGroupIdB64)
    }

    public func mark(accountIdB64: String, successorGroupIdB64: String) {
        var list = held(accountIdB64)
        guard !list.contains(successorGroupIdB64) else { return }
        list.append(successorGroupIdB64)
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(Array(list.suffix(512)), forKey: Self.prefix + accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}
