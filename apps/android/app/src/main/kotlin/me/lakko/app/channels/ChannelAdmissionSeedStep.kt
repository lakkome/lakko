// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * Why a private channel's admission seed is wanted (ADR-0087 section 7,
 * security pass F1 of 2026-11-15).
 */
public enum class AdmissionSeedReason {
    /** A link, a ticket or the account reader key needs the seed this device
     *  holds. A device without one waits for the owner account's
     *  admission-seed register (ADR-0090); it never mints one and never marks
     *  rows: "no seed here" is not "the channel never had one" on a second
     *  owner device, and every row the service lists could be planted. */
    USE,

    /** New channel made this private channel here: version 0 is minted with
     *  the admission mark left at 0. A fresh private channel has no genuine
     *  rows, so any row the service lists then was planted by it. */
    CREATE,

    /** This device switched the channel from public to private (UX section
     *  4.1, "Everyone subscribed stays"): the rows of today become readers,
     *  then a seed is minted when none is held. */
    GO_PRIVATE,
}

/**
 * The admission seed for [reason], or `null` when this device must wait for
 * the register. [held] reads the newest held seed (the caller owns and wipes
 * what it returns), [mint] mints version 0 (and writes it to the register),
 * [markRowsAsReaders] raises this device's mark over today's rows. Rows are
 * marked only for [AdmissionSeedReason.GO_PRIVATE]; a seed is minted only for
 * [AdmissionSeedReason.CREATE] and [AdmissionSeedReason.GO_PRIVATE].
 */
public suspend fun admissionSeedFor(
    reason: AdmissionSeedReason,
    held: () -> Pair<UShort, ByteArray>?,
    mint: () -> Pair<UShort, ByteArray>,
    markRowsAsReaders: suspend () -> Unit,
): Pair<UShort, ByteArray>? {
    if (reason == AdmissionSeedReason.GO_PRIVATE) markRowsAsReaders()
    held()?.let { return it }
    return when (reason) {
        AdmissionSeedReason.USE -> null
        AdmissionSeedReason.CREATE, AdmissionSeedReason.GO_PRIVATE -> mint()
    }
}
