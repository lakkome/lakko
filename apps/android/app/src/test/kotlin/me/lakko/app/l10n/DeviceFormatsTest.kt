// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import me.lakko.app.ui.chat.dateSeparatorLabel
import me.lakko.app.ui.chat.formatClockTime
import me.lakko.app.ui.chat.formatListTime
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId
import java.time.ZonedDateTime
import java.util.Locale

/**
 * `docs/ux/localization.md` section 5, "The language decides words, the
 * device decides formats" (2026-10-29): the clock, the field order and the
 * separators follow the device; the interface language supplies words.
 */
class DeviceFormatsTest {
    init {
        EnglishForTests.install()
    }

    @After
    fun release() {
        DeviceFormats.overrideForTests(null, null)
    }

    private val utc = ZoneId.of("UTC")
    private val at = ZonedDateTime.of(2025, 9, 16, 13, 5, 0, 0, utc)
    private val atMs = at.toInstant().toEpochMilli()
    private val english = Locale.ENGLISH
    private val netherlands = Locale("nl", "NL")

    @Test
    fun `English interface on a 24-hour device shows 13 05, not 1 05 PM`() {
        // The person switched "Use 24-hour format" on.
        assertTrue(DeviceFormats.use24Hour("24", Locale.US))
        // Or left it on the region default, and the region keeps 24 hours.
        assertTrue(DeviceFormats.use24Hour(null, netherlands))
        assertEquals("13:05", formatClockTime(atMs, utc, use24h = true, locale = english, region = Locale.US))
        assertEquals("13:05", formatClockTime(atMs, utc, use24h = true, locale = english, region = netherlands))
    }

    @Test
    fun `a 12-hour device shows the AM PM form whatever the interface language`() {
        assertFalse(DeviceFormats.use24Hour("12", Locale.GERMANY))
        assertFalse(DeviceFormats.use24Hour(null, Locale.US))
        val german = formatClockTime(atMs, utc, use24h = false, locale = Locale.GERMAN, region = Locale.GERMANY)
        assertTrue(german, german.startsWith("1:05"))
        assertFalse(german.startsWith("13"))
        assertEquals("1:05 PM", plain(formatClockTime(atMs, utc, use24h = false, locale = english, region = Locale.US)))
    }

    @Test
    fun `a Korean or Chinese interface on a 12-hour device puts the day-period word first`() {
        // The clock is laid out the way the interface language writes it:
        // Korean and Chinese write the AM/PM word before the time.
        val korean = formatClockTime(atMs, utc, use24h = false, locale = Locale.KOREAN, region = Locale.KOREA)
        assertEquals("오후 1:05", plain(korean))
        // Korean on a Dutch device: the language still decides the layout.
        assertEquals("오후 1:05", plain(formatClockTime(atMs, utc, use24h = false, locale = Locale.KOREAN, region = netherlands)))
        // The same languages on a 24-hour device: no day-period word at all.
        assertEquals("13:05", formatClockTime(atMs, utc, use24h = true, locale = Locale.KOREAN, region = Locale.KOREA))
        assertEquals("13:05", formatClockTime(atMs, utc, use24h = true, locale = Locale.SIMPLIFIED_CHINESE, region = Locale.CHINA))
    }

    /** CLDR separates the clock and the day-period word with a narrow or a
     *  no-break space; compare them as a plain space. */
    private fun plain(s: String): String = s.replace('\u202f', ' ').replace('\u00a0', ' ')

    @Test
    fun `month names come from the interface language, the order from the region`() {
        assertEquals("16 Sep", DeviceFormats.dayMonth(at, english, Locale.UK))
        assertEquals("Sep 16", DeviceFormats.dayMonth(at, english, Locale.US))
        val russianOnDutch = DeviceFormats.dayMonth(at, Locale("ru"), netherlands, long = true)
        assertTrue(russianOnDutch, russianOnDutch.startsWith("16 ") && russianOnDutch.contains("сентя"))
        assertEquals("September 16, 2025", DeviceFormats.dayMonthYear(at, english, Locale.US, long = true))
        assertEquals("16 September 2025", DeviceFormats.dayMonthYear(at, english, Locale.UK, long = true))
        assertEquals(DeviceFormats.DateOrder.YMD, DeviceFormats.dateOrder(Locale.JAPAN))
    }

    @Test
    fun `the timeline and the list use the device region`() {
        val now = Instant.parse("2026-10-29T12:00:00Z").toEpochMilli()
        assertEquals("16 September 2025", dateSeparatorLabel(atMs, now, utc, english, region = Locale.UK))
        assertEquals("September 16, 2025", dateSeparatorLabel(atMs, now, utc, english, region = Locale.US))
        val sameYear = ZonedDateTime.of(2026, 9, 16, 13, 5, 0, 0, utc).toInstant().toEpochMilli()
        assertEquals("Sep 16", formatListTime(sameYear, now, utc, use24h = true, locale = english, region = Locale.US))
        assertEquals("16 Sep", formatListTime(sameYear, now, utc, use24h = true, locale = english, region = netherlands))
        // Today: the device's clock, here 24 hours, under an English interface.
        val today = ZonedDateTime.of(2026, 10, 29, 13, 5, 0, 0, utc).toInstant().toEpochMilli()
        assertEquals("13:05", formatListTime(today, now, utc, use24h = true, locale = english, region = Locale.US))
    }

    @Test
    fun `numbers take the region's separators`() {
        assertEquals("1,5", DeviceFormats.decimal(1.5, 1, english, Locale.GERMANY))
        assertEquals("1.5", DeviceFormats.decimal(1.5, 1, english, Locale.US))
        assertEquals("1.234", DeviceFormats.integer(1234, english, Locale.GERMANY))
        assertEquals("1,234", DeviceFormats.integer(1234, english, Locale.US))
        DeviceFormats.overrideForTests(Locale.GERMANY, true)
        assertEquals("1,2 MB", me.lakko.app.chat.AttachmentBridge.formatSize(1_200_000UL))
    }
}
