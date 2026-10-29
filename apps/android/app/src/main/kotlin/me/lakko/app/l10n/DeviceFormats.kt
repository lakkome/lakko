// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import java.text.DecimalFormat
import java.text.DecimalFormatSymbols
import java.text.NumberFormat
import java.time.ZonedDateTime
import java.time.chrono.IsoChronology
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeFormatterBuilder
import java.time.format.FormatStyle
import java.util.Locale

/**
 * `docs/ux/localization.md` section 5, "The language decides words, the
 * device decides formats" (2026-10-29): the interface language supplies the
 * words only (month and weekday names, relative-time words, the AM/PM
 * marker). The 12- or 24-hour clock, the order of day, month and year and
 * the decimal and grouping separators come from the device's own regional
 * settings, whatever language Lakko is shown in.
 *
 * On Android the per-app language (`setApplicationLocales`) also changes the
 * app's own `Configuration`, so `DateFormat.is24HourFormat(context)` on an
 * app context, in "use locale default" mode, answered from the interface
 * language: a Dutch 24-hour phone showed `12:58 PM` with English chosen. The
 * region here is read from the SYSTEM configuration instead.
 *
 * Every function that turns a time, date or number into display text goes
 * through here. Fixed formats (safety numbers, recovery-phrase positions)
 * do not.
 */
public object DeviceFormats {
    @Volatile private var regionOverride: Locale? = null

    @Volatile private var use24hOverride: Boolean? = null

    @Volatile private var appContext: android.content.Context? = null

    /** Remember the application context, for [is24Hour] without one. */
    public fun init(context: android.content.Context) {
        appContext = context.applicationContext
    }

    /** [is24Hour] through the remembered application context; before
     *  [init], the region's own clock. */
    public fun is24Hour(): Boolean {
        use24hOverride?.let { return it }
        return appContext?.let(::is24Hour) ?: regionUses24Hour(region())
    }

    /** The device's region (system locale), never the app's chosen language. */
    public fun region(): Locale {
        regionOverride?.let { return it }
        return try {
            android.content.res.Resources.getSystem().configuration.locales.get(0)
        } catch (_: Throwable) {
            null
        } ?: Locale.getDefault(Locale.Category.FORMAT)
    }

    /**
     * Whether the device shows a 24-hour clock: the system "Use 24-hour
     * format" switch when the person set it, else the system region's default.
     */
    public fun is24Hour(context: android.content.Context): Boolean {
        use24hOverride?.let { return it }
        val setting = try {
            android.provider.Settings.System.getString(
                context.contentResolver,
                android.provider.Settings.System.TIME_12_24,
            )
        } catch (_: Throwable) {
            null
        }
        return use24Hour(setting, region())
    }

    /** The rule behind [is24Hour]: `"24"` / `"12"` decide, anything else
     *  (unset: "use locale default") takes the region's own clock. */
    public fun use24Hour(setting: String?, region: Locale): Boolean = when (setting) {
        "24" -> true
        "12" -> false
        else -> regionUses24Hour(region)
    }

    /** Whether [region]'s own short time pattern is a 24-hour one. */
    public fun regionUses24Hour(region: Locale): Boolean {
        val pattern = localizedPattern(null, FormatStyle.SHORT, region)
        return pattern != null && ('H' in pattern || 'k' in pattern)
    }

    /** Test hook: pin the region and the clock, `null` to release. */
    public fun overrideForTests(region: Locale?, use24h: Boolean?) {
        regionOverride = region
        use24hOverride = use24h
    }

    // ------------------------------------------------------------------
    // Times and dates: the pattern from the region, the words from [words].
    // ------------------------------------------------------------------

    /**
     * "13:05", "1:05 PM" or "오후 1:05": the clock per [use24h] (the device's
     * decision), laid out the way the interface language writes that clock
     * (where the AM/PM word goes), with the language's AM/PM word.
     */
    public fun clockTime(
        at: ZonedDateTime,
        use24h: Boolean,
        words: Locale,
        region: Locale = region(),
    ): String {
        val combined = combined(words, region)
        return DateTimeFormatter.ofPattern(clockPattern(use24h, combined), combined).format(at)
    }

    /** The interface language with the device's region (`ko` on a Dutch
     *  device: `ko-NL`), the locale a clock pattern is chosen for. */
    public fun combined(words: Locale, region: Locale): Locale {
        if (region.country.isNullOrEmpty()) return words
        return try {
            Locale.Builder().setLocale(words).setRegion(region.country).build()
        } catch (_: Throwable) {
            words
        }
    }

    /**
     * The clock pattern for [locale] with the hour cycle forced to [use24h]:
     * the platform's best pattern for the skeleton `Hm` / `hm`
     * (`DateFormat.getBestDateTimePattern`, ICU), else (off a device, or a
     * failure) the locale's short time pattern turned to that hour cycle.
     */
    public fun clockPattern(use24h: Boolean, locale: Locale): String {
        val best = try {
            android.text.format.DateFormat.getBestDateTimePattern(locale, if (use24h) "Hm" else "hm")
        } catch (_: Throwable) {
            null
        }
        val pattern = best?.takeIf { it.isNotBlank() }?.let(::withPlainDayPeriod)
            ?: fallbackClockPattern(use24h, locale)
        return if (validPattern(pattern)) pattern else if (use24h) "HH:mm" else "h:mm a"
    }

    /** ICU may name a flexible day period (`B`, `b`); java.time on older
     *  Android does not read it, so it becomes the plain AM/PM field. */
    private fun withPlainDayPeriod(pattern: String): String =
        outsideQuotes(pattern) { it.replace(Regex("B+|b+"), "a") }

    /** Apply [change] to the pattern letters only, never to quoted literal
     *  text (`HH 'h' mm`). */
    private fun outsideQuotes(pattern: String, change: (String) -> String): String {
        val out = StringBuilder()
        val run = StringBuilder()
        var quoted = false
        for (c in pattern) {
            if (c == '\'') {
                if (!quoted) {
                    out.append(change(run.toString()))
                    run.clear()
                }
                quoted = !quoted
                out.append(c)
            } else if (quoted) {
                out.append(c)
            } else {
                run.append(c)
            }
        }
        out.append(change(run.toString()))
        return out.toString()
    }

    /** The locale's short time pattern with its hour cycle forced. */
    internal fun fallbackClockPattern(use24h: Boolean, locale: Locale): String {
        val base = localizedPattern(null, FormatStyle.SHORT, locale) ?: return if (use24h) "HH:mm" else "h:mm a"
        val letters = StringBuilder()
        outsideQuotes(base) { run -> letters.append(run); run }
        val twelve = 'h' in letters || 'K' in letters
        return when {
            use24h && !twelve -> base
            use24h -> outsideQuotes(base) { run ->
                run.replace(Regex("\\s*a+\\s*"), " ")
                    .replace(Regex("[hK]+")) { if (it.value.length > 1) "HH" else "H" }
            }.trim()
            twelve -> base
            else -> outsideQuotes(base) { run -> run.replace(Regex("[Hk]+"), "h") } + " a"
        }
    }

    private fun validPattern(pattern: String): Boolean = try {
        DateTimeFormatter.ofPattern(pattern)
        true
    } catch (_: Throwable) {
        false
    }

    /** The order a date's fields take in [region]. */
    public enum class DateOrder { DMY, MDY, YMD }

    public fun dateOrder(region: Locale): DateOrder {
        val pattern = localizedPattern(FormatStyle.SHORT, null, region) ?: return DateOrder.DMY
        val d = pattern.indexOf('d')
        val m = pattern.indexOf('M').let { if (it < 0) pattern.indexOf('L') else it }
        val y = pattern.indexOf('y').let { if (it < 0) pattern.indexOf('u') else it }
        return when {
            y in 0 until minOf(d.takeIf { it >= 0 } ?: Int.MAX_VALUE, m.takeIf { it >= 0 } ?: Int.MAX_VALUE) -> DateOrder.YMD
            m in 0 until (d.takeIf { it >= 0 } ?: Int.MAX_VALUE) -> DateOrder.MDY
            else -> DateOrder.DMY
        }
    }

    /** "16 Sep" / "Sep 16" ([long]: "16 September" / "September 16"). */
    public fun dayMonth(at: ZonedDateTime, words: Locale, region: Locale, long: Boolean = false): String {
        val month = if (long) "MMMM" else "MMM"
        val pattern = when (dateOrder(region)) {
            DateOrder.DMY -> "d $month"
            DateOrder.MDY, DateOrder.YMD -> "$month d"
        }
        return DateTimeFormatter.ofPattern(pattern, words).format(at)
    }

    /** "16 Sep 2025" / "Sep 16, 2025" / "2025 Sep 16" ([long] with full month names). */
    public fun dayMonthYear(at: ZonedDateTime, words: Locale, region: Locale, long: Boolean = false): String {
        val month = if (long) "MMMM" else "MMM"
        val pattern = when (dateOrder(region)) {
            DateOrder.DMY -> "d $month yyyy"
            DateOrder.MDY -> "$month d, yyyy"
            DateOrder.YMD -> "yyyy $month d"
        }
        return DateTimeFormatter.ofPattern(pattern, words).format(at)
    }

    /** The region's short numeric date ("16.09.25", "9/16/25"). */
    public fun numericDate(at: ZonedDateTime, words: Locale, region: Locale): String =
        formatLocalized(at, FormatStyle.SHORT, null, words, region)

    /** The region's medium date ("16 Sep 2025", "Sep 16, 2025"), words from [words]. */
    public fun mediumDate(at: ZonedDateTime, words: Locale, region: Locale): String =
        formatLocalized(at, FormatStyle.MEDIUM, null, words, region)

    /** [mediumDate] of an epoch-millis instant in this device's zone, words in
     *  the interface [language] (a BCP 47 tag). */
    public fun mediumDate(atMs: Long, language: String): String =
        mediumDate(
            java.time.Instant.ofEpochMilli(atMs).atZone(java.time.ZoneId.systemDefault()),
            Locale.forLanguageTag(language),
            region(),
        )

    /** [mediumDateTime] of an epoch-millis instant in this device's zone, with
     *  the device's clock ([is24Hour]). */
    public fun mediumDateTime(atMs: Long, language: String): String =
        mediumDateTime(
            java.time.Instant.ofEpochMilli(atMs).atZone(java.time.ZoneId.systemDefault()),
            is24Hour(),
            Locale.forLanguageTag(language),
            region(),
        )

    /** [mediumDate] followed by the clock time per [use24h]. */
    public fun mediumDateTime(at: ZonedDateTime, use24h: Boolean, words: Locale, region: Locale): String =
        mediumDate(at, words, region) + ", " + clockTime(at, use24h, words, region)

    private fun formatLocalized(
        at: ZonedDateTime,
        date: FormatStyle?,
        time: FormatStyle?,
        words: Locale,
        region: Locale,
    ): String {
        val pattern = localizedPattern(date, time, region)
        return if (pattern != null) {
            DateTimeFormatter.ofPattern(pattern, words).format(at)
        } else {
            val f = if (date != null) DateTimeFormatter.ofLocalizedDate(date) else DateTimeFormatter.ofLocalizedTime(time)
            f.withLocale(words).format(at)
        }
    }

    private fun localizedPattern(date: FormatStyle?, time: FormatStyle?, region: Locale): String? = try {
        DateTimeFormatterBuilder.getLocalizedDateTimePattern(date, time, IsoChronology.INSTANCE, region)
    } catch (_: Throwable) {
        null
    }

    // ------------------------------------------------------------------
    // Numbers: separators from the region, digits from [words].
    // ------------------------------------------------------------------

    private fun symbols(words: Locale, region: Locale): DecimalFormatSymbols {
        val s = DecimalFormatSymbols.getInstance(region)
        s.zeroDigit = DecimalFormatSymbols.getInstance(words).zeroDigit
        return s
    }

    /** A whole number with the region's grouping ("1,234" / "1.234"). */
    public fun integer(value: Long, words: Locale, region: Locale): String {
        val f = NumberFormat.getIntegerInstance(region)
        (f as? DecimalFormat)?.decimalFormatSymbols = symbols(words, region)
        return f.format(value)
    }

    /** A decimal with exactly [digits] fraction digits and the region's separators. */
    public fun decimal(value: Double, digits: Int, words: Locale, region: Locale): String {
        val f = NumberFormat.getNumberInstance(region)
        f.minimumFractionDigits = digits
        f.maximumFractionDigits = digits
        (f as? DecimalFormat)?.decimalFormatSymbols = symbols(words, region)
        return f.format(value)
    }
}
