// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import org.junit.Assert.assertEquals
import org.junit.Test

/** The action layout of [LakkoDialog] (design system section 3, Dialog). */
class LakkoDialogLayoutTest {
    @Test
    fun `one or two actions share a row, three or more stack`() {
        assertEquals(LakkoDialogActionLayout.Row, lakkoDialogActionLayout(0))
        assertEquals(LakkoDialogActionLayout.Row, lakkoDialogActionLayout(1))
        assertEquals(LakkoDialogActionLayout.Row, lakkoDialogActionLayout(2))
        assertEquals(LakkoDialogActionLayout.Stacked, lakkoDialogActionLayout(3))
        assertEquals(LakkoDialogActionLayout.Stacked, lakkoDialogActionLayout(4))
    }

    @Test
    fun `a row reads Cancel first and the confirming action last`() {
        assertEquals(listOf("cancel", "confirm"), lakkoDialogActionOrder("confirm", emptyList(), "cancel"))
    }

    @Test
    fun `a stack puts the confirming action on top and Cancel last`() {
        assertEquals(
            listOf("confirm", "other", "cancel"),
            lakkoDialogActionOrder("confirm", listOf("other"), "cancel"),
        )
        assertEquals(
            listOf("confirm", "a", "b", "cancel"),
            lakkoDialogActionOrder("confirm", listOf("a", "b"), "cancel"),
        )
    }

    @Test
    fun `a notice has its one button and no Cancel`() {
        assertEquals(listOf("ok"), lakkoDialogActionOrder("ok", emptyList<String>(), null))
    }

    @Test
    fun `a dialog with no confirming action still keeps Cancel`() {
        assertEquals(listOf("cancel"), lakkoDialogActionOrder(null, emptyList(), "cancel"))
    }

    @Test
    fun `the picker shows the label of the current value`() {
        val options = listOf(1 to "One", 2 to "Two")
        assertEquals("Two", lakkoPickerLabel(2, options))
        assertEquals("", lakkoPickerLabel(3, options))
    }
}
