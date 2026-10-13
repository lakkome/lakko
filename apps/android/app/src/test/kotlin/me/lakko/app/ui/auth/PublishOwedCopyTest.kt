// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0075 - the publish-owed screen after a reset or a take-back says
 *  nothing about words to an account that is not phrase-protected. */
class PublishOwedCopyTest {

    private val wordsKey = "recovery_phrase.fresh.publish_owed"
    private val wordlessKey = "account.keys_changed.finishing"

    @Test
    fun `a phrase-protected account keeps the existing text`() {
        assertEquals(wordsKey, publishOwedLeadKeys(sampleRetryable(), wordless = false).first())
        assertEquals("recovery_phrase.fresh.title", publishOwedTitleKey(wordless = false))
    }

    @Test
    fun `any other account is told the change is finishing, with no mention of words`() {
        val keys = publishOwedLeadKeys(sampleRetryable(), wordless = true)
        assertEquals(listOf(wordlessKey), keys)
        assertFalse(keys.contains(wordsKey))
        assertTrue(publishOwedTitleKey(wordless = true) != "recovery_phrase.fresh.title")
    }

    private fun sampleRetryable() = me.lakko.app.auth.PublishOwedReason.Retryable(null)
}
