// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * No operating-system share sheet inside a Lakko screen (the design system's
 * rule; `docs/ux/call-links.md` section 3).
 *
 * A link is handed out by Copy link and, where it makes sense, by sending it
 * to a contact inside the app. A file leaves through the system's save picker
 * (`ui/components/FileSaver.kt`), which is the narrowest mechanism the
 * platform has. The share sheet came back once per feature until this test
 * existed: the call invite, the contact invite links, the group invite link,
 * the co-publisher link, the feed file, the saved attachment and the account
 * export each opened one.
 *
 * The twin of the web client's `ui/no-system-share.test.ts`. It reads the
 * sources, since no host test can observe an `Intent`.
 */
class NoSystemShareSheetTest {
    private val sources: List<File> by lazy {
        val root = listOf(File("src/main/kotlin"), File("app/src/main/kotlin")).first { it.isDirectory }
        root.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
    }

    /** The lines of [file] that are code, not commentary. */
    private fun codeLines(file: File): List<String> =
        file.readLines().filterNot { line ->
            val trimmed = line.trim()
            trimmed.startsWith("//") || trimmed.startsWith("*") || trimmed.startsWith("/*")
        }

    @Test
    fun `no screen opens a chooser`() {
        val offenders = sources.filter { file ->
            codeLines(file).any { "createChooser" in it || "ShareCompat" in it }
        }
        assertEquals(emptyList<String>(), offenders.map { it.path })
    }

    @Test
    fun `ACTION_SEND is only ever something the app receives`() {
        // `MainActivity` reads it off an incoming intent (a feed file shared TO
        // Lakko). Nothing builds one.
        val users = sources.filter { file -> codeLines(file).any { "ACTION_SEND" in it } }
        assertEquals(listOf("MainActivity.kt"), users.map { it.name })
        val building = codeLines(users.single()).filter { "Intent(Intent.ACTION_SEND" in it }
        assertTrue(building.isEmpty())
    }

    @Test
    fun `there are sources to read`() {
        // A wrong working directory must fail here, not pass the two above on
        // an empty list.
        assertTrue(sources.size > 100)
    }
}
