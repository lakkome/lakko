// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit

// ADR-0060 section 6, "An add a contact proposed", and
// `crates/lakko-contacts/src/committer_delegation.rs`, "Lifetime: the proposer
// re-sends". A delegation lives less than a day and an ADR-0045 proposal has
// no deadline, so an Add that waits longer than its envelope would lose its
// way to the person. The proposing device re-sends while the Add waits.
//
// When one is due is the core's rule (`committerEnvelopeDue`), never
// re-derived here. What this file holds is the iOS half of it: what is
// remembered of a send, and which of this device's own requests the rule is
// asked about. The send itself is the first send's
// (`ChatRepository.sendCommitterDelegations`).

/// The last committer envelope this device published for one Add, kept on that
/// Add's ``MembershipIntent``. Durable, and wiped with the intents.
public struct SentCommitterEnvelope: Equatable {
    /// When it was published, unix milliseconds.
    public let sentAtMs: UInt64
    /// Whether it carried delegations. One that did not names a binding, which
    /// does not expire.
    public let signed: Bool
    /// The committer candidates it covered, standard base64.
    public let coveredB64: [String]

    public init(sentAtMs: UInt64, signed: Bool, coveredB64: [String]) {
        self.sentAtMs = sentAtMs
        self.signed = signed
        self.coveredB64 = coveredB64
    }

    var plist: [String: Any] {
        ["sentAtMs": NSNumber(value: sentAtMs), "signed": NSNumber(value: signed), "covered": coveredB64]
    }

    init?(plist: [String: Any]) {
        guard let sentAtMs = (plist["sentAtMs"] as? NSNumber)?.uint64Value,
              let signed = (plist["signed"] as? NSNumber)?.boolValue,
              let covered = plist["covered"] as? [String] else { return nil }
        self.init(sentAtMs: sentAtMs, signed: signed, coveredB64: covered)
    }
}

/// Whether `intent` is an Add this device proposed that still waits: the
/// person is not in the group yet and its proposal has not been dropped. A
/// dropped proposal is the superseded re-request's business, and that sends
/// its own envelope. A torn roster read answers `false`: nothing is sent on
/// a group this device could not see.
public func ownAddStillWaiting(_ intent: MembershipIntent, _ snapshot: SupersededSnapshot) -> Bool {
    guard case .add(let person, _, let leafKeys) = intent.request,
          !snapshot.membershipEnded,
          let leaves = snapshot.leaves,
          !leaves.contains(where: { $0.personKeyB64 == person }) else { return false }
    let pending = Set(snapshot.pendingKeys)
    return leafKeys.contains { pending.contains("add:\($0)") }
}

/// The core's rule over what this device remembers. `candidates` are the
/// group's committer candidates now, as an envelope names them. A key of the
/// wrong length never reaches here; if one did, nothing is due.
public func committerEnvelopeIsDue(
    _ last: SentCommitterEnvelope?, candidates: [Data], nowMs: UInt64
) -> Bool {
    let due = try? committerEnvelopeDue(
        sentAtMs: last?.sentAtMs,
        delegated: last?.signed ?? false,
        covered: (last?.coveredB64 ?? []).compactMap(base64Decode),
        candidates: candidates,
        nowMs: nowMs)
    return due ?? false
}

/// Whether the app is in the foreground, where a re-send may run.
enum CommitterEnvelopeForeground {
    @MainActor static func isActive() -> Bool {
        UIApplication.shared.applicationState == .active
    }
}
