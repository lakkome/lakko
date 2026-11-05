// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type

/**
 * Base scenario 10 — a hardware keyboard works in the composer and in every
 * dialog: Enter submits (sends the message, saves the edit), Shift+Enter
 * inserts a line break. The on-screen keyboard is left alone (its Enter is a
 * line break in a multi-line field, as on every phone).
 */
public enum class EnterKey { SUBMIT, NEWLINE, PASS }

/** What one key press does in a multi-line field that submits on Enter. */
public fun enterKeyAction(isEnter: Boolean, keyDown: Boolean, shift: Boolean, fromHardware: Boolean): EnterKey = when {
    !isEnter || !fromHardware -> EnterKey.PASS
    shift -> EnterKey.NEWLINE
    keyDown -> EnterKey.SUBMIT
    // The key-up of a submitting Enter: swallowed with it.
    else -> EnterKey.SUBMIT
}

/** [enterKeyAction] on the field: Enter from a hardware keyboard calls
 *  [onSubmit] once (on key-down) and inserts nothing; Shift+Enter and every
 *  other key go to the field as before. */
public fun Modifier.submitOnHardwareEnter(onSubmit: () -> Unit): Modifier = onPreviewKeyEvent { event ->
    // An on-screen keyboard marks the key events it sends as soft; a key event
    // without the mark came from a keyboard.
    val hardware = (event.nativeKeyEvent.flags and android.view.KeyEvent.FLAG_SOFT_KEYBOARD) == 0
    val isEnter = event.key == Key.Enter || event.key == Key.NumPadEnter
    when (enterKeyAction(isEnter, event.type == KeyEventType.KeyDown, event.isShiftPressed, hardware)) {
        EnterKey.SUBMIT -> {
            if (event.type == KeyEventType.KeyDown) onSubmit()
            true
        }
        EnterKey.NEWLINE, EnterKey.PASS -> false
    }
}
