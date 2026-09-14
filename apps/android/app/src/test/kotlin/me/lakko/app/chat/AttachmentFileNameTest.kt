// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/** `AttachmentRef.name` is chosen by the sender, so these pin that it can
 *  never steer a write outside the directory the recipient's Save uses. */
class AttachmentFileNameTest {
    private val fallback = "attachment-deadbeef"

    @Test
    fun parentTraversalKeepsOnlyTheLastComponent() {
        assertEquals("x.xml", AttachmentBridge.safeFileName("../../shared_prefs/x.xml", fallback))
    }

    @Test
    fun backslashIsTreatedAsASeparatorToo() {
        assertEquals("evil.txt", AttachmentBridge.safeFileName("..\\..\\evil.txt", fallback))
    }

    @Test
    fun dotAndDotDotAreNotNames() {
        assertEquals(fallback, AttachmentBridge.safeFileName("..", fallback))
        assertEquals(fallback, AttachmentBridge.safeFileName(".", fallback))
        assertEquals(fallback, AttachmentBridge.safeFileName("a/..", fallback))
    }

    @Test
    fun trailingSeparatorLeavesNothingUsable() {
        assertEquals(fallback, AttachmentBridge.safeFileName("dir/", fallback))
    }

    @Test
    fun controlAndFormatCharactersAreDropped() {
        // U+202E RIGHT-TO-LEFT OVERRIDE renders "gpj.exe" as "exe.jpg".
        assertEquals("photogpj.exe", AttachmentBridge.safeFileName("photo\u202Egpj.exe", fallback))
        assertEquals("abc", AttachmentBridge.safeFileName("a\u0000b\nc", fallback))
    }

    @Test
    fun nullAndBlankFallBack() {
        assertEquals(fallback, AttachmentBridge.safeFileName(null, fallback))
        assertEquals(fallback, AttachmentBridge.safeFileName("   ", fallback))
    }

    @Test
    fun lengthIsCapped() {
        val long = "a".repeat(1000)
        assertEquals(
            AttachmentBridge.MAX_SAFE_FILE_NAME_LENGTH,
            AttachmentBridge.safeFileName(long, fallback).length,
        )
    }

    @Test
    fun ordinaryNamesPassThrough() {
        assertEquals("Report 2026.pdf", AttachmentBridge.safeFileName("Report 2026.pdf", fallback))
        assertEquals(".hidden", AttachmentBridge.safeFileName(".hidden", fallback))
    }
}
