// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The title of a channel's contact picker says what the pick does.
 *
 * Seen on a live walk: `Add publisher` opened a list titled
 * `Invite to call`, because the picker was the call screen's and kept its
 * title.
 */
class ChannelInvitePickerTitleTest {
    @Test
    fun `the publisher picker is titled Add publisher`() {
        assertEquals("Add publisher", en(channelInvitePickerTitleKey(publisher = true)))
    }

    @Test
    fun `the subscriber picker is titled Invite people`() {
        assertEquals("Invite people", en(channelInvitePickerTitleKey(publisher = false)))
    }
}
