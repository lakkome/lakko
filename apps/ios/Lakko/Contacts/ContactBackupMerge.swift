// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Reconstruct restored backup contacts into ``AcceptedContact``s, deduped
/// against the existing list by Ed25519 pubkey (ADR-0021 contact-in-backup
/// round-trip).
///
/// Pure + side-effect-free so it is unit-testable without the Keychain /
/// `UserDefaults`: the caller loads `existing` from + saves the merged list to
/// ``AcceptedContactsStore``. Returns ONLY the new contacts to append — a
/// pubkey already present is skipped so a contact accepted AFTER the backup is
/// not clobbered.
///
/// The backup carries no stranger-inbox metadata, so the reconstructed
/// ``IncomingContactRequest`` fills the rest with sentinels: a NEGATIVE
/// `messageId` (a real DS id is non-negative, so these never collide, and each
/// restored entry gets a distinct one), a zero pseudonym, and no intro. An
/// empty `displayName` round-trips back to `nil` (unset) rather than promoting
/// the fingerprint fallback into a "chosen" name.
func reconstructRestoredContacts(
    existing: [AcceptedContact],
    restored: [BackupContactRecord],
    createdAtMs: UInt64
) -> [AcceptedContact] {
    let known = Set(existing.map { $0.request.fromPubkey })
    var out: [AcceptedContact] = []
    for (idx, rec) in restored.enumerated() where !known.contains(rec.ed25519Pubkey) {
        let request = IncomingContactRequest(
            messageId: Int64(-(idx + 1)),
            fromPubkey: rec.ed25519Pubkey,
            fromAccountId: rec.accountId,
            fromPseudonym: Data(count: 32),
            validAfterMs: createdAtMs,
            intro: nil,
            createdAt: ""
        )
        out.append(
            AcceptedContact(
                request: request,
                groupIdHex: nil,
                displayName: rec.displayName.isEmpty ? nil : rec.displayName
            )
        )
    }
    return out
}
