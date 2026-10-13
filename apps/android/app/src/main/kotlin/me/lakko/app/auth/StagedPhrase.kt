// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/** One account's row of the cached recovery phrase (production: the
 *  Keystore-backed [me.lakko.app.backup.PhraseStore]). */
internal interface PhraseRow {
    /** The stored phrase bytes, or `null` when the row is absent. */
    fun read(): ByteArray?

    fun write(phrase: ByteArray)

    fun delete()
}

/**
 * A recovery phrase written to its account's row BEFORE the step that makes
 * it the account's only way back (the publish of a fresh identity), and taken
 * out again when that step does not happen.
 *
 * The order is the point. Written after the publish, a failed write, a killed
 * process or a session that changed underneath leaves an identity whose
 * phrase exists nowhere. Written before, the worst case is a row that names a
 * phrase the server never learned of, and [rollback] removes exactly that.
 */
internal class StagedPhrase(private val row: PhraseRow, phrase: String) {
    private val phraseBytes: ByteArray = phrase.toByteArray(Charsets.UTF_8)
    private var previous: ByteArray? = null

    /**
     * Remember what the row holds and write the phrase over it.
     *
     * @throws Throwable when the row cannot be read or written; nothing
     *   irreversible has happened and the caller refuses to go on.
     */
    fun stage() {
        previous = row.read()
        row.write(phraseBytes)
    }

    /** Whether the row holds this phrase now (a forget-device in between
     *  wiped it; a later write replaced it). Never throws. */
    fun isHeld(): Boolean = try {
        row.read()?.let { held -> held.contentEquals(phraseBytes).also { held.fill(0) } } ?: false
    } catch (_: Throwable) {
        false
    }

    /**
     * The step did not happen: put the row back as [stage] found it. Only
     * when it still holds THIS phrase; a row wiped or rewritten meanwhile is
     * somebody else's decision and is left as it is. Never throws; `false`
     * when the row could not be put back.
     */
    fun rollback(): Boolean {
        if (!isHeld()) return true
        return try {
            val before = previous
            if (before != null) row.write(before) else row.delete()
            true
        } catch (_: Throwable) {
            false
        }
    }

    /** Wipe the copies this object holds. The row is not touched. */
    fun close() {
        phraseBytes.fill(0)
        previous?.fill(0)
        previous = null
    }
}
