// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import uniffi.lakko_ffi_uniffi.BackupContactRecord

/**
 * Reconstruct restored backup contacts into [AcceptedContact]s, deduped against
 * the existing list by Ed25519 pubkey (ADR-0021 contact-in-backup round-trip).
 *
 * Pure + `Context`-free so it is unit-testable without an Android runtime: the
 * caller loads `existing` from + saves the merged list to
 * [AcceptedContactsStore]. Returns ONLY the new contacts to append — a pubkey
 * already present is skipped so a contact accepted AFTER the backup is not
 * clobbered.
 *
 * The backup carries no stranger-inbox metadata, so the reconstructed
 * [IncomingContactRequest] fills the rest with sentinels: a NEGATIVE
 * `messageId` (a real DS id is non-negative, so these never collide, and each
 * restored entry gets a distinct one), a zero pseudonym, and no intro. An empty
 * `displayName` round-trips back to `null` (unset) rather than promoting the
 * fingerprint fallback into a "chosen" name.
 */
public fun reconstructRestoredContacts(
    existing: List<AcceptedContact>,
    restored: List<BackupContactRecord>,
    createdAtMs: ULong,
): List<AcceptedContact> {
    val known = existing.mapTo(HashSet()) { it.request.fromPubkey.toList() }
    return restored.mapIndexedNotNull { idx, rec ->
        if (rec.ed25519Pubkey.toList() in known) {
            return@mapIndexedNotNull null
        }
        AcceptedContact(
            request =
                IncomingContactRequest(
                    messageId = -(idx.toLong() + 1),
                    fromPubkey = rec.ed25519Pubkey,
                    fromAccountId = rec.accountId,
                    fromPseudonym = ByteArray(32),
                    validAfterMs = createdAtMs,
                    intro = null,
                    createdAt = "",
                ),
            groupIdHex = null,
            displayName = rec.displayName.ifEmpty { null },
        )
    }
}
