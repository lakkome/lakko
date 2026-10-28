// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0080 device cards: the receiver's store rules, the send rule, the
/// Devices screen's order and row text, and the center's persistence and
/// throttle.
@MainActor
final class DeviceCardsTests: XCTestCase {

    private let calendar: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "Europe/Amsterdam") ?? .current
        return c
    }()

    private var now: Int64 {
        let date = calendar.date(from: DateComponents(year: 2026, month: 10, day: 14, hour: 15)) ?? Date()
        return Int64(date.timeIntervalSince1970 * 1000)
    }

    private func card(
        updatedAtMs: Int64? = nil, lastActiveMs: Int64? = nil, city: String = "Almere", country: String = "NL",
        model: String = "iPhone 16 Pro", appVersion: String = "1.4", os: String = "iOS 27.0.1",
        platform: DeviceCardPlatform = .ios
    ) -> StoredDeviceCard {
        StoredDeviceCard(
            platform: platform, model: model, appVersion: appVersion, os: os, city: city, country: country,
            updatedAtMs: updatedAtMs ?? now - 1000, lastActiveMs: lastActiveMs ?? now - 1000)
    }

    func testKeepsACardFromTheRosterAndOnlyAStrictlyNewerOne() {
        XCTAssertEqual(DeviceCards.receive([:], senderKeyHex: "aa", card: card(), rosterKeysHex: ["aa"]), ["aa": card()])
        XCTAssertNil(DeviceCards.receive([:], senderKeyHex: "cc", card: card(), rosterKeysHex: ["aa"]))
        let stored = ["aa": card(updatedAtMs: 10)]
        XCTAssertNil(DeviceCards.receive(stored, senderKeyHex: "aa", card: card(updatedAtMs: 10, model: "X"), rosterKeysHex: ["aa"]))
        XCTAssertNil(DeviceCards.receive(stored, senderKeyHex: "aa", card: card(updatedAtMs: 9), rosterKeysHex: ["aa"]))
        XCTAssertEqual(
            DeviceCards.receive(stored, senderKeyHex: "aa", card: card(updatedAtMs: 11, model: "X"), rosterKeysHex: ["aa"])?["aa"]?.model,
            "X")
    }

    func testDropsTheCardOfALeafThatLeft() {
        let stored = ["aa": card(), "bb": card()]
        XCTAssertEqual(DeviceCards.prune(stored, rosterKeysHex: ["aa"]), ["aa": card()])
        XCTAssertNil(DeviceCards.prune(stored, rosterKeysHex: ["aa", "bb"]))
    }

    func testSendsWithNoneYetOnAFieldChangeAndAfterThirtyMinutes() {
        let fields = card()
        XCTAssertTrue(DeviceCards.shouldSend(last: nil, current: fields, nowMs: now))
        let last = card(updatedAtMs: now - 60_000)
        XCTAssertFalse(DeviceCards.shouldSend(last: last, current: fields, nowMs: now))
        var moved = fields
        moved.city = "Utrecht"
        XCTAssertTrue(DeviceCards.shouldSend(last: last, current: moved, nowMs: now))
        XCTAssertFalse(DeviceCards.shouldSend(last: card(updatedAtMs: now - DeviceCards.refreshMs + 1), current: fields, nowMs: now))
        XCTAssertTrue(DeviceCards.shouldSend(last: card(updatedAtMs: now - DeviceCards.refreshMs), current: fields, nowMs: now))
        XCTAssertTrue(DeviceCards.shouldSend(last: card(updatedAtMs: now + 5_000), current: fields, nowMs: now))
        XCTAssertEqual(DeviceCards.growthDelayMs(random: 0.5), 15_000)
        XCTAssertLessThan(DeviceCards.growthDelayMs(random: 1), 30_000)
    }

    func testOrdersThisDeviceFirstThenNewestActivityThenCardless() {
        let rows = [
            DeviceCards.RowInput(keyHex: "old", leafIndex: 1, isSelf: false, card: card(lastActiveMs: 100)),
            DeviceCards.RowInput(keyHex: "none", leafIndex: 2, isSelf: false, card: nil),
            DeviceCards.RowInput(keyHex: "self", leafIndex: 3, isSelf: true, card: card()),
            DeviceCards.RowInput(keyHex: "new", leafIndex: 4, isSelf: false, card: card(lastActiveMs: 200)),
            DeviceCards.RowInput(keyHex: "none0", leafIndex: 0, isSelf: false, card: nil),
        ]
        let ordered = DeviceCards.order(rows)
        XCTAssertEqual(ordered.current?.keyHex, "self")
        XCTAssertEqual(ordered.others.map(\.keyHex), ["new", "old", "none0", "none"])
    }

    private var words: DeviceCards.Words {
        DeviceCards.Words(
            device: "Device", online: "online",
            app: { $0.isEmpty ? "Lakko" : "Lakko \($0)" },
            place: { "\($0), \($1)" },
            countryName: { $0 == "NL" ? "Netherlands" : $0 })
    }

    private func view(_ row: DeviceCards.RowInput) -> DeviceCards.RowView {
        DeviceCards.view(row, words: words, nowMs: now, calendar: calendar, locale: Locale(identifier: "en_GB"))
    }

    func testARowReadsLikeAnActiveSession() {
        let v = view(DeviceCards.RowInput(keyHex: "aa", leafIndex: 0, isSelf: false, card: card()))
        XCTAssertEqual(v.title, "iPhone 16 Pro")
        XCTAssertEqual(v.detail, "Lakko 1.4 · iOS 27.0.1")
        XCTAssertEqual(v.place, "Almere, Netherlands")
        XCTAssertEqual(v.icon, .phone)
        XCTAssertEqual(view(DeviceCards.RowInput(keyHex: "aa", leafIndex: 0, isSelf: true, card: card())).activity, "online")
        XCTAssertEqual(view(DeviceCards.RowInput(keyHex: "aa", leafIndex: 0, isSelf: false, card: card(city: ""))).place, "Netherlands")
        let none = view(DeviceCards.RowInput(keyHex: "aa", leafIndex: 0, isSelf: false, card: nil))
        XCTAssertEqual(none, DeviceCards.RowView(title: "Device", detail: "", place: "", activity: "", icon: .unknown))
    }

    func testNamesCommonIPhonesAndFallsBackToTheIdentifier() {
        XCTAssertEqual(DeviceCards.appleModelName("iPhone17,1"), "iPhone 16 Pro")
        XCTAssertEqual(DeviceCards.appleModelName("iPhone99,9"), "iPhone99,9")
    }

    func testTheCenterKeepsCardsPerAccountAndThrottlesItsOwn() async throws {
        let suite = "lakko.tests.device-cards.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let center = DeviceCardCenter(defaults: defaults)
        center.isForeground = { true }
        center.antiCensorshipArmed = { false }
        center.locate = { ("Almere", "NL") }
        center.bind(accountIdB64: "acct") { _ in }
        center.receive(senderKeyHex: "aa", card: card(), rosterKeysHex: ["aa", "bb"])
        center.receive(senderKeyHex: "cc", card: card(), rosterKeysHex: ["aa", "bb"])
        XCTAssertEqual(Set(center.cards.keys), ["aa"])

        center.noteAction(nowMs: now)
        for _ in 0..<100 where center.own == nil { try await Task.sleep(nanoseconds: 10_000_000) }
        let first = try XCTUnwrap(center.own)
        XCTAssertEqual(first.city, "Almere")
        center.noteAction(nowMs: now + 60_000)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(center.own, first)

        center.unbind()
        center.bind(accountIdB64: "acct") { _ in }
        XCTAssertEqual(Set(center.cards.keys), ["aa"])
        center.prune(rosterKeysHex: ["bb"])
        XCTAssertTrue(center.cards.isEmpty)
        center.unbind()
        center.bind(accountIdB64: "other") { _ in }
        XCTAssertNil(center.own)
    }
}
