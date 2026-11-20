// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0090 — a publisher's device request and the channel admission mark ride
 * a PublisherGroup journal; the background peek keeps both off every preview.
 */
class Adr0090EnvelopeFilterTest {
    @Test
    fun the_device_request_and_the_admission_mark_are_suppressed_by_their_domain() {
        val request = "lakko/publisher-device-request/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(50)
        val mark = "lakko/channel-admission-mark/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(25)
        assertTrue(looksLikePublisherChannelEnvelope(request))
        assertTrue(looksLikePublisherChannelEnvelope(mark))
        // The domain without its NUL is not the envelope.
        assertFalse(looksLikePublisherChannelEnvelope("lakko/publisher-device-request/v1".toByteArray()))
        assertFalse(looksLikePublisherChannelEnvelope("lakko/channel-admission-mark/v1".toByteArray()))
    }
}
