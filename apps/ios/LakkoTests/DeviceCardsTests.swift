// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

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
        XCTAssertEqual(DeviceCards.growthDelayMs(random: 0.5), 150)
        XCTAssertLessThan(DeviceCards.growthDelayMs(random: 1), 300)
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

    // MARK: - Cold launch and the background growth re-send

    private func freshCenter() throws -> (DeviceCardCenter, UserDefaults, String) {
        let suite = "lakko.tests.device-cards.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        let center = DeviceCardCenter(defaults: defaults)
        center.antiCensorshipArmed = { false }
        center.locate = { ("Almere", "NL") }
        return (center, defaults, suite)
    }

    /// On a cold launch no touch is noted before the account binds: the first
    /// foreground of the binding is the action, once.
    func testTheFirstForegroundAfterAColdLaunchNotesAnAction() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.isForeground = { true }
        var sent = 0

        center.noteForeground(nowMs: now)
        XCTAssertNil(center.own, "nothing before the account binds")

        center.bind(accountIdB64: "acct") { _ in sent += 1 }
        XCTAssertNil(center.own, "binding alone sends nothing")
        center.noteForeground(nowMs: now)
        for _ in 0..<100 where sent == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertNotNil(center.own)
        XCTAssertEqual(sent, 1)

        center.noteForeground(nowMs: now + 60 * 60 * 1000)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(sent, 1, "only the first foreground of a binding counts")
        center.unbind()
    }

    func testABackgroundGrowthReSendIsSentAtTheNextForeground() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        var foreground = true
        center.isForeground = { foreground }
        var sent = 0
        center.bind(accountIdB64: "acct") { _ in sent += 1 }
        center.noteForeground(nowMs: now)
        for _ in 0..<100 where sent == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(sent, 1)

        foreground = false
        center.scheduleResend(random: 0)
        for _ in 0..<100 where !center.resendOwed { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(center.resendOwed)
        XCTAssertEqual(sent, 1, "nothing is sent from the background")
        XCTAssertTrue(defaults.bool(forKey: DeviceCards.resendOwedKey(accountIdB64: "acct")), "kept across a relaunch")

        center.noteForeground()
        XCTAssertEqual(sent, 1, "still in the background")

        foreground = true
        center.noteForeground()
        for _ in 0..<100 where sent == 1 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(sent, 2)
        XCTAssertFalse(center.resendOwed)
        XCTAssertFalse(defaults.bool(forKey: DeviceCards.resendOwedKey(accountIdB64: "acct")))

        center.noteForeground()
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(sent, 2, "sent once")
        center.unbind()
    }

    func testAGrowthReSendInTheForegroundIsSentAtOnceAndOwesNothing() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.isForeground = { true }
        var sent = 0
        center.bind(accountIdB64: "acct") { _ in sent += 1 }
        center.noteForeground(nowMs: now)
        for _ in 0..<100 where sent == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        center.scheduleResend(random: 0)
        for _ in 0..<100 where sent == 1 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(sent, 2)
        XCTAssertFalse(center.resendOwed)
        center.unbind()
    }

    // MARK: - Naming the device being added (UX sections 2.1 and 3)

    /// The joining device puts in its payload the card it would send as its
    /// own, with the city switch and the anti-censorship rule as for the card.
    func testThePairingPayloadCarriesTheCardTheDeviceWouldSend() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.isForeground = { true }
        center.bind(accountIdB64: "acct") { _ in }
        center.noteAction(nowMs: now)
        for _ in 0..<100 where center.own == nil { try await Task.sleep(nanoseconds: 10_000_000) }

        let envelope = try XCTUnwrap(center.pairingCardEnvelope(nowMs: now))
        let fields = center.currentFields(nowMs: now)
        XCTAssertEqual(fields.city, "Almere")
        XCTAssertEqual(DeviceCardCenter.stored(try decodeDeviceCardEnvelope(plaintext: envelope)), fields)

        let identity = try Identity.fromSeed(seed: Data(repeating: 0x6D, count: 32))
        let handle = Data(repeating: 0x5C, count: 32)
        let wire = try PairingCodec.buildPairingQrPayload(
            loginHandle: handle, keyPackage: identity.createKeyPackage(),
            rendezvousPseudonym: Data(repeating: 0x7E, count: 32), deviceCardEnvelope: envelope)
        let decoded = try PairingCodec.parsePairingQrPayload(wireBytes: wire, expectedLoginHandle: handle)
        XCTAssertEqual(PairingCodec.deviceCard(of: decoded), fields, "the confirming device reads the same card")

        let bare = try PairingCodec.buildPairingQrPayload(
            loginHandle: handle, keyPackage: identity.createKeyPackage(),
            rendezvousPseudonym: Data(repeating: 0x7E, count: 32))
        XCTAssertNil(PairingCodec.deviceCard(of: try PairingCodec.parsePairingQrPayload(
            wireBytes: bare, expectedLoginHandle: handle)), "no card: nothing named")

        center.antiCensorshipArmed = { true }
        let armed = try XCTUnwrap(center.pairingCardEnvelope(nowMs: now))
        XCTAssertEqual(try decodeDeviceCardEnvelope(plaintext: armed).city, "", "no city under anti-censorship")
        center.antiCensorshipArmed = { false }
        center.setShowCity(false)
        let hidden = try XCTUnwrap(center.pairingCardEnvelope(nowMs: now))
        XCTAssertEqual(try decodeDeviceCardEnvelope(plaintext: hidden).city, "", "no city with the switch off")
        center.unbind()
    }

    // MARK: - The place in the payload card (UX section 2.1, 2026-10-29)

    /// A locate the test answers when it chooses, counting requests.
    @MainActor
    private final class GatedLocate {
        var requests = 0
        private var waiting: [CheckedContinuation<(city: String, country: String)?, Never>] = []
        func ask() async -> (city: String, country: String)? {
            requests += 1
            return await withCheckedContinuation { waiting.append($0) }
        }
        var pending: Bool { !waiting.isEmpty }
        func answer(_ value: (city: String, country: String)?) {
            let all = waiting
            waiting = []
            for continuation in all { continuation.resume(returning: value) }
        }
    }

    private func city(of envelope: Data?) throws -> String {
        try decodeDeviceCardEnvelope(plaintext: try XCTUnwrap(envelope)).city
    }

    /// A device not signed in asks once when the screen opens; an answer
    /// inside the bound is in the card, and is held for this pairing only.
    func testAPlaceThatArrivesWithinTheBoundIsInTheCardOfADeviceNotSignedIn() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        var requests = 0
        center.locate = { @MainActor in requests += 1; return ("Almere", "NL") }
        // The bound never passes in this test.
        center.pairingSleep = { _ in try? await Task.sleep(nanoseconds: 60_000_000_000) }

        let place = center.beginPairingPlace(nowMs: now)
        let envelope = await center.pairingCardEnvelope(waitingFor: place, nowMs: { self.now })
        XCTAssertEqual(try city(of: envelope), "Almere")
        XCTAssertEqual(try decodeDeviceCardEnvelope(plaintext: try XCTUnwrap(envelope)).country, "NL")
        XCTAssertEqual(requests, 1, "one request, and nothing else")
        XCTAssertEqual(center.currentFields(nowMs: now).city, "", "held in memory for this pairing only")
        XCTAssertEqual(try city(of: center.pairingCardEnvelope(nowMs: now)), "", "not without the pairing's place")
    }

    /// No answer by the bound: the QR's card has no place. The code route,
    /// sealed later, takes the answer that has arrived by then.
    func testNoPlaceAfterTheBoundAndTheCodeRouteTakesALaterAnswer() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        let gate = GatedLocate()
        center.locate = { @MainActor in await gate.ask() }
        var slept: [Double] = []
        center.pairingSleep = { @MainActor in slept.append($0) }   // the bound passes at once
        center.monotonicNow = { 100 }

        let place = center.beginPairingPlace(nowMs: now)
        for _ in 0..<100 where !gate.pending { await Task.yield() }
        let qr = await center.pairingCardEnvelope(waitingFor: place, nowMs: { self.now })
        XCTAssertEqual(try city(of: qr), "", "no answer by the bound: no place")
        XCTAssertEqual(slept, [DeviceCards.pairingPlaceWaitSeconds])

        gate.answer(("Almere", "NL"))
        for _ in 0..<100 where !place.settled { await Task.yield() }
        let sealedLater = await center.pairingCardEnvelope(waitingFor: place, nowMs: { self.now })
        XCTAssertEqual(try city(of: sealedLater), "Almere", "the code is typed after the answer arrived")
        XCTAssertEqual(gate.requests, 1)
        XCTAssertEqual(slept.count, 1, "no second wait")
    }

    /// The bound runs from the screen opening: a wait that starts after it
    /// has passed does not wait at all.
    func testTheBoundCountsFromTheScreenOpening() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        let gate = GatedLocate()
        center.locate = { @MainActor in await gate.ask() }
        var clock: TimeInterval = 100
        center.monotonicNow = { clock }
        var slept = 0
        center.pairingSleep = { @MainActor seconds in _ = seconds; slept += 1 }

        let place = center.beginPairingPlace(nowMs: now)
        clock += DeviceCards.pairingPlaceWaitSeconds
        let placeEnvelope = await center.pairingCardEnvelope(waitingFor: place, nowMs: { self.now })
        XCTAssertEqual(try city(of: placeEnvelope), "")
        XCTAssertEqual(slept, 0)
        for _ in 0..<100 where !gate.pending { await Task.yield() }
        gate.answer(nil)
    }

    /// No request with the anti-censorship connection armed or the switch
    /// off; the card then has no place.
    func testNoPlaceIsAskedWithAntiCensorshipArmedOrTheSwitchOff() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        var requests = 0
        center.locate = { @MainActor in requests += 1; return ("Almere", "NL") }
        center.pairingSleep = { _ in }

        center.antiCensorshipArmed = { true }
        let armed = center.beginPairingPlace(nowMs: now)
        XCTAssertTrue(armed.settled)
        let armedEnvelope = await center.pairingCardEnvelope(waitingFor: armed, nowMs: { self.now })
        XCTAssertEqual(try city(of: armedEnvelope), "")
        XCTAssertEqual(requests, 0, "nothing asked under anti-censorship")

        center.antiCensorshipArmed = { false }
        center.isForeground = { false }   // the switch sends no card here
        center.bind(accountIdB64: "acct") { _ in }
        center.setShowCity(false)
        let hidden = center.beginPairingPlace(nowMs: now)
        XCTAssertTrue(hidden.settled)
        let hiddenEnvelope = await center.pairingCardEnvelope(waitingFor: hidden, nowMs: { self.now })
        XCTAssertEqual(try city(of: hiddenEnvelope), "")
        XCTAssertEqual(requests, 0, "nothing asked with the switch off")
        center.unbind()
    }

    /// A signed-in device with a fresh cached city asks nothing; one without
    /// asks once and keeps the answer as its city.
    func testASignedInDeviceUsesAFreshCityOrKeepsTheAnswer() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        var requests = 0
        center.locate = { @MainActor in requests += 1; return ("Almere", "NL") }
        center.isForeground = { false }
        center.pairingSleep = { _ in try? await Task.sleep(nanoseconds: 60_000_000_000) }
        center.bind(accountIdB64: "acct") { _ in }

        let asked = center.beginPairingPlace(nowMs: now)
        let askedEnvelope = await center.pairingCardEnvelope(waitingFor: asked, nowMs: { self.now })
        XCTAssertEqual(try city(of: askedEnvelope), "Almere")
        XCTAssertEqual(requests, 1)
        XCTAssertNotNil(defaults.data(forKey: DeviceCards.cityKey(accountIdB64: "acct")), "kept as the account's city")
        XCTAssertEqual(center.currentFields(nowMs: now).city, "Almere")

        let cached = center.beginPairingPlace(nowMs: now + 60_000)
        XCTAssertTrue(cached.settled)
        let cachedEnvelope = await center.pairingCardEnvelope(waitingFor: cached, nowMs: { self.now })
        XCTAssertEqual(try city(of: cachedEnvelope), "Almere")
        XCTAssertEqual(requests, 1, "a fresh cached city: nothing asked")
        center.unbind()
    }

    func testTheConfirmStepNamesTheDeviceByItsCardAndOtherwiseKeepsTodaysTitle() {
        let named = { (name: String) in "Link \u{201C}\(name)\u{201D}?" }
        let web = card(city: "Almere", country: "NL", model: "Chrome on macOS", appVersion: "web", os: "macOS 26", platform: .web)
        XCTAssertEqual(
            DeviceCards.pairingConfirm(web, words: words, titleNamed: named, titleUnnamed: "Link this device?"),
            DeviceCards.PairingConfirm(
                title: "Link \u{201C}Chrome on macOS\u{201D}?", detail: "Lakko web · macOS 26", place: "Almere, Netherlands"))
        let row = view(DeviceCards.RowInput(keyHex: "aa", leafIndex: 0, isSelf: false, card: web))
        let confirm = DeviceCards.pairingConfirm(web, words: words, titleNamed: named, titleUnnamed: "x")
        XCTAssertEqual([confirm.detail, confirm.place], [row.detail, row.place], "the lines its row will show")
        XCTAssertEqual(
            DeviceCards.pairingConfirm(nil, words: words, titleNamed: named, titleUnnamed: "Link this device?"),
            DeviceCards.PairingConfirm(title: "Link this device?", detail: "", place: ""))
        XCTAssertEqual(
            DeviceCards.pairingConfirm(card(model: " "), words: words, titleNamed: named, titleUnnamed: "Link this device?").title,
            "Link this device?", "a card with no model names nothing")
        XCTAssertEqual(DeviceCards.pairingName(web), "Chrome on macOS")
        XCTAssertNil(DeviceCards.pairingName(nil))

        // The catalog copy.
        XCTAssertEqual(DevicePairingCopy.confirm(nil).title, Localization.shared.t("devices.scan.confirm.title"))
        let titled = DevicePairingCopy.confirm(web).title
        XCTAssertEqual(
            titled, Localization.shared.t("devices.scan.confirm.title_named", texts: ["name": "Chrome on macOS"]))
        XCTAssertTrue(titled.contains("Chrome on macOS"))
    }

    /// UX section 2.1 (decided 2026-10-29) — the scan route has no result
    /// line: a linked device goes straight back to the Devices list, which
    /// shows nothing when the keys were handed over.
    func testTheScanRouteReturnsToTheListOnceLinked() {
        let notice = DeviceScanNotice()
        notice.linked(accountIdB64: "acct", keysSent: true)
        XCTAssertFalse(notice.showsKeysNotSent(accountIdB64: "acct"), "no line under the list")
    }

    /// UX section 2.1 — a scan link that could not hand over the keys keeps
    /// the keys-not-sent line, under the Devices list, until `Add a device`
    /// is pressed again; it is not shown for another account.
    func testAScanLinkWithoutKeysKeepsTheLineUntilAddADevice() {
        let notice = DeviceScanNotice()
        notice.linked(accountIdB64: "acct", keysSent: false)
        XCTAssertTrue(notice.showsKeysNotSent(accountIdB64: "acct"))
        XCTAssertFalse(notice.showsKeysNotSent(accountIdB64: "other"))
        notice.addDevicePressed()
        XCTAssertFalse(notice.showsKeysNotSent(accountIdB64: "acct"), "cleared by Add a device")
        notice.linked(accountIdB64: "acct", keysSent: false)
        notice.linked(accountIdB64: "acct", keysSent: true)
        XCTAssertFalse(notice.showsKeysNotSent(accountIdB64: "acct"), "a later link with keys clears it")
    }

    func testThePayloadCardNamesTheNewLeafUntilItsOwnCardArrives() async throws {
        let payloadCard = card(updatedAtMs: now, model: "Chrome on macOS", platform: .web)
        XCTAssertNil(DeviceCards.seedFromPairing([:], keyHex: "nn", card: payloadCard, rosterKeysHex: ["aa"]),
                     "not in the roster")
        let real = card(updatedAtMs: 5, model: "Pixel 7a", platform: .android)
        XCTAssertNil(DeviceCards.seedFromPairing(["nn": real], keyHex: "nn", card: payloadCard, rosterKeysHex: ["nn"]),
                     "a stored card is never overwritten by the payload's")
        let seeded = try XCTUnwrap(
            DeviceCards.seedFromPairing([:], keyHex: "nn", card: payloadCard, rosterKeysHex: ["nn"]))
        XCTAssertEqual(seeded["nn"]?.model, "Chrome on macOS")
        XCTAssertEqual(seeded["nn"]?.updatedAtMs, 0, "the lowest stamp")
        let own = card(updatedAtMs: 1, city: "", model: "Chrome on macOS", platform: .web)
        XCTAssertEqual(DeviceCards.receive(seeded, senderKeyHex: "nn", card: own, rosterKeysHex: ["nn"])?["nn"], own,
                       "the device's own card replaces it")

        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.bind(accountIdB64: "acct") { _ in }
        center.seedFromPairing(accountIdB64: "other", keyHex: "nn", card: payloadCard, rosterKeysHex: ["nn"])
        XCTAssertNil(center.cards["nn"], "another account's add seeds nothing")
        center.seedFromPairing(accountIdB64: "acct", keyHex: "nn", card: payloadCard, rosterKeysHex: ["mm", "nn"])
        XCTAssertEqual(center.cards["nn"]?.model, "Chrome on macOS")
        center.receive(senderKeyHex: "nn", card: own, rosterKeysHex: ["mm", "nn"])
        XCTAssertEqual(center.cards["nn"], own)
        center.seedFromPairing(accountIdB64: "acct", keyHex: "nn", card: payloadCard, rosterKeysHex: ["mm", "nn"])
        XCTAssertEqual(center.cards["nn"], own, "a later payload card does not replace the real one")
        center.unbind()
    }

    func testThisDevicesRowIsBuiltFromItsOwnFields() throws {
        let received = ["self": card(model: "stale"), "other": card(model: "Pixel 7a", platform: .android)]
        let ownCard = card(model: "iPhone 16 Pro")
        XCTAssertEqual(
            DeviceCards.rowCard(keyHex: "self", isSelf: true, cards: received, ownCard: ownCard), ownCard)
        XCTAssertEqual(
            DeviceCards.rowCard(keyHex: "self", isSelf: true, cards: [:], ownCard: ownCard)?.model, "iPhone 16 Pro",
            "never the received-cards map, where the own card never is")
        XCTAssertEqual(
            DeviceCards.rowCard(keyHex: "other", isSelf: false, cards: received, ownCard: ownCard)?.model, "Pixel 7a")

        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.bind(accountIdB64: "acct") { _ in }
        let row = center.ownRowCard(nowMs: now)
        XCTAssertEqual(row, center.currentFields(nowMs: now))
        let shown = view(DeviceCards.RowInput(keyHex: "self", leafIndex: 0, isSelf: true, card: row))
        XCTAssertNotEqual(shown.title, "Device")
        XCTAssertFalse(row.model.isEmpty)
        center.unbind()
    }

    /// A device that has just joined sends its own card at once: no growth
    /// delay, no foreground action, no city lookup first.
    func testAJoinedDeviceSendsItsCardAtOnce() async throws {
        let (center, defaults, suite) = try freshCenter()
        defer { defaults.removePersistentDomain(forName: suite) }
        center.isForeground = { false }
        center.locate = {
            try? await Task.sleep(nanoseconds: 60_000_000_000)
            return nil
        }
        var sent: [Data] = []
        center.bind(accountIdB64: "acct") { sent.append($0) }
        center.noteJoined(accountIdB64: "acct")
        for _ in 0..<100 where sent.isEmpty { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(sent.count, 1)
        let own = try XCTUnwrap(center.own)
        XCTAssertEqual(DeviceCardCenter.stored(try decodeDeviceCardEnvelope(plaintext: sent[0])), own)
        center.unbind()

        // Unbound at the join (a fresh device): owed to that account's binding.
        var later = 0
        center.noteJoined(accountIdB64: "acct2")
        center.bind(accountIdB64: "other") { _ in later += 1 }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(later, 0, "not to another account")
        center.unbind()
        center.bind(accountIdB64: "acct2") { _ in later += 1 }
        for _ in 0..<100 where later == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(later, 1)
        center.unbind()
        center.bind(accountIdB64: "acct2") { _ in later += 1 }
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(later, 1, "sent once")
        center.unbind()
    }
}
