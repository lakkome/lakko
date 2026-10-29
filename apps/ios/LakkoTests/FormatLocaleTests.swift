// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/localization.md` section 5, "The language decides words, the
/// device decides formats" (2026-10-29): seen live, an English interface on a
/// 24-hour device showed "12:58 PM".
final class FormatLocaleTests: XCTestCase {

    private var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }

    /// 2025-09-16 13:05:00 UTC.
    private let ms: Int64 = 1_758_027_900_000

    private func system(_ identifier: String, hourCycle: Locale.HourCycle) -> Locale {
        var components = Locale.Components(identifier: identifier)
        components.hourCycle = hourCycle
        return Locale(components: components)
    }

    private func time(language: String, system: Locale) -> String {
        ChatTimeline.timeLabel(
            ms: ms, calendar: utc, locale: FormatLocale.make(language: language, system: system))
    }

    func testAnEnglishInterfaceOnA24HourDeviceShows24Hours() {
        let shown = time(language: "en", system: system("en_US", hourCycle: .zeroToTwentyThree))
        XCTAssertEqual(shown, "13:05")
        XCTAssertFalse(shown.contains("PM"))
        XCTAssertEqual(
            time(language: "en", system: system("nl_NL", hourCycle: .zeroToTwentyThree)), "13:05")
    }

    func testAnotherLanguageOnA12HourDeviceShowsTheTwelveHourForm() {
        let shown = time(language: "ru", system: system("en_US", hourCycle: .oneToTwelve))
        XCTAssertTrue(shown.contains("1:05"), shown)
        XCTAssertFalse(shown.contains("13"), shown)
        // Without the device's setting Russian alone would say 13:05.
        XCTAssertEqual(
            ChatTimeline.timeLabel(ms: ms, calendar: utc, locale: Locale(identifier: "ru")), "13:05")
    }

    func testMonthNamesComeFromTheInterfaceLanguage() {
        let label = ChatTimeline.dateLabel(
            ms: ms, nowMs: ms + 400 * 86_400_000, calendar: utc,
            locale: FormatLocale.make(language: "ru", system: system("en_US", hourCycle: .oneToTwelve)))
        XCTAssertTrue(label.contains("сентября"), label)
        XCTAssertFalse(label.contains("September"), label)
    }

    func testDayMonthOrderAndSeparatorsFollowTheRegion() {
        let other = ms + 400 * 86_400_000
        let us = ChatTimeline.dateLabel(
            ms: ms, nowMs: other, calendar: utc,
            locale: FormatLocale.make(language: "en", system: system("en_US", hourCycle: .oneToTwelve)))
        let nl = ChatTimeline.dateLabel(
            ms: ms, nowMs: other, calendar: utc,
            locale: FormatLocale.make(language: "en", system: system("nl_NL", hourCycle: .zeroToTwentyThree)))
        XCTAssertTrue(us.hasPrefix("September"), us)
        XCTAssertTrue(nl.hasPrefix("16"), nl)
        XCTAssertTrue(nl.contains("September"), "the words stay English: \(nl)")
        let decimal = (1.5).formatted(
            .number.precision(.fractionLength(1))
                .locale(FormatLocale.make(language: "en", system: system("nl_NL", hourCycle: .zeroToTwentyThree))))
        XCTAssertEqual(decimal, "1,5")
    }

    func testTheInterfaceLanguageIsKept() {
        let locale = FormatLocale.make(language: "ru", system: system("nl_NL", hourCycle: .zeroToTwentyThree))
        XCTAssertEqual(locale.language.languageCode?.identifier, "ru")
        XCTAssertEqual(locale.region?.identifier, "NL")
    }
}
