// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import me.lakko.app.devices.JoinWrongAccount
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0061 section 6 — a failed restore renders from its kind. A join answered
 * for a different account has its own sentence; it used to fall through to the
 * generic one because it was thrown as a bare `IllegalStateException`.
 */
class DeviceRestoreErrorTextTest {
    @Test
    fun `a join for another account has its own sentence`() {
        assertEquals(UiText("device_link.error.wrong_account"), deviceRestoreErrorText(JoinWrongAccount()))
    }

    @Test
    fun `an unknown failure stays generic`() {
        assertEquals(UiText("device_link.error.generic"), deviceRestoreErrorText(IllegalStateException("x")))
    }
}
