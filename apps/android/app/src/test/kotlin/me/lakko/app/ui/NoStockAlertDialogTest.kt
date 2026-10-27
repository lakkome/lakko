// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * No stock alert dialog inside a Lakko screen: every confirmation and notice
 * is `ui/components/LakkoDialog.kt` (the design system's Dialog). The stock
 * Material `AlertDialog` and the framework `android.app.AlertDialog` look
 * foreign and differ from the web and iOS shape.
 *
 * The twin of the web client's `ui/no-window-confirm.test.ts`. It reads the
 * sources.
 */
class NoStockAlertDialogTest {
    private val sources: List<File> by lazy {
        val root = listOf(File("src/main/kotlin"), File("app/src/main/kotlin")).first { it.isDirectory }
        root.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
    }

    private fun codeLines(file: File): List<String> =
        file.readLines().filterNot { line ->
            val trimmed = line.trim()
            trimmed.startsWith("//") || trimmed.startsWith("*") || trimmed.startsWith("/*")
        }

    @Test
    fun `no screen draws a stock alert dialog`() {
        val offenders = sources.filter { file ->
            codeLines(file).any {
                "AlertDialog(" in it || "material3.AlertDialog" in it ||
                    "android.app.AlertDialog" in it || "AlertDialog.Builder" in it || "DatePickerDialog" in it
            }
        }
        assertEquals(emptyList<String>(), offenders.map { it.path })
    }

    @Test
    fun `there are sources to read`() {
        assertTrue(sources.size > 10)
    }
}
