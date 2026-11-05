// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Test

/** An accept that could not found the one-to-one leaves the request with an
 *  error line and a Try again button (no contact is made). */
class AcceptButtonTest {
    @Test
    fun `after a failed accept the button reads Try again`() {
        val en = EnglishForTests.install()
        assertEquals("Accept", en[acceptButtonKey(acceptFailed = false)])
        assertEquals("Try again", en[acceptButtonKey(acceptFailed = true)])
    }
}
