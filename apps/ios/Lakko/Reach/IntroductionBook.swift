// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0060 section 4 — contacts of contacts, the receiving half: the
/// introduction record each contact offered (their own) and the bundle each
/// contact relayed (their contacts'). Kept in the ``ReachCenter`` record, so in
/// the Keychain with the rest of this account's reach state.
@MainActor
public final class IntroductionBook {
    public static let shared = IntroductionBook()

    /// Keep a contact's own introduction record — offered in the one-to-one
    /// conversation, from its other participant (checked by the caller). The
    /// newest per contact wins; a malformed record is dropped.
    /// Returns whether the held offers changed, so the caller relays the new
    /// record to this account's other contacts at once.
    @discardableResult
    public func keepOffer(_ record: Data, fromAccountKeyB64 key: String) -> Bool {
        guard (try? introductionTags(record: record)) != nil else { return false }
        return ReachCenter.shared.storeOffer(record, fromAccountKeyB64: key)
    }

    /// Keep a contact's relayed bundle when it is newer than the one held
    /// (`bundleIsNewer`: a re-send carries its original stamp).
    public func keepBundle(_ bundle: FfiIntroductionsBundle, fromAccountKeyB64 key: String) {
        ReachCenter.shared.storeBundle(bundle, fromAccountKeyB64: key)
    }

    /// Every record in every relayed bundle (padding dummies included, which
    /// never open).
    public func relayedRecords() -> [Data] {
        ReachCenter.shared.relayedRecords()
    }

    /// The records this device relays to its contacts: the newest each contact
    /// offered.
    public func offeredRecords() -> [Data] {
        ReachCenter.shared.offeredRecords()
    }
}
