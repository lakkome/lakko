// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The Android copy of the BIP39 English word list against the web client's,
 * which it was generated from: same words, same order, so the copies cannot
 * drift apart unnoticed.
 */
class Bip39WordsTest {

    @Test
    fun `the list is the web client's list, word for word and in order`() {
        val web = webList()
        assertEquals(2048, web.size)
        assertEquals(2048, Bip39Words.all.size)
        assertEquals(web, Bip39Words.all)
        assertEquals(2048, Bip39Words.all.toSet().size)
    }

    @Test
    fun `membership trims and folds case, and refuses what is not a word`() {
        assertTrue(Bip39Words.contains("abandon"))
        assertTrue(Bip39Words.contains("zoo"))
        assertTrue(Bip39Words.contains("  Zebra "))
        assertFalse(Bip39Words.contains(""))
        assertFalse(Bip39Words.contains("abandom"))
        assertFalse(Bip39Words.contains("abandon zoo"))
    }

    /** `BIP39_ENGLISH` of `apps/web/src/recovery/bip39-words.ts`, read from
     *  the repository at test time. */
    private fun webList(): List<String> {
        val relative = "apps/web/src/recovery/bip39-words.ts"
        val file = sequenceOf("", "../", "../../", "../../../")
            .map { File(it + relative) }
            .firstOrNull { it.isFile }
            ?: error("cannot find $relative from ${File("").absolutePath}")
        val source = file.readText()
        val start = source.indexOf("BIP39_ENGLISH")
        check(start >= 0) { "BIP39_ENGLISH not found in $relative" }
        val open = source.indexOf("= [", start)
        val close = source.indexOf("];", open)
        check(open >= 0 && close > open) { "the array literal of BIP39_ENGLISH was not found" }
        return Regex("\"([a-z]+)\"").findAll(source.substring(open, close)).map { it.groupValues[1] }.toList()
    }
}
