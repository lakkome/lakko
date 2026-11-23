// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit

/// Device cards (ADR-0080): the stateful half on iOS. Holds the cards the
/// account's other devices sent, this device's own last card, the city
/// answer and the "Show my city to my other devices" switch; decides on the
/// person's actions whether to send a fresh own card; re-sends it when the
/// DeviceGroup grows. The rules are ``DeviceCards``.
///
/// Storage: `UserDefaults.standard`, per account
/// (``AccountLocalDataWipe`` removes the keys). Bound by
/// ``DeviceGroupSyncRepository`` for the signed-in account; the send path is
/// that repository's DeviceGroup `ServiceEnvelope`.
@MainActor
final class DeviceCardCenter: ObservableObject {

    static let shared = DeviceCardCenter()

    /// Received cards, keyed by the sender's leaf key (lower-case hex).
    @Published private(set) var cards: [String: StoredDeviceCard] = [:]
    /// The Privacy switch (per device, on by default).
    @Published private(set) var showCity = true
    /// This device's own last card.
    @Published private(set) var own: StoredDeviceCard?

    private struct Persisted: Codable {
        var cards: [String: StoredDeviceCard]
        var own: StoredDeviceCard?
    }

    private struct CityAnswer: Codable {
        var city: String
        var country: String
        var fetchedAtMs: Int64
    }

    private var accountIdB64: String?
    private var sender: ((Data) async -> Void)?
    @Published private var city: CityAnswer?
    private var deciding = false
    /// UX section 3 — this device has just joined the account's DeviceGroup
    /// from a pairing Welcome and its own card is owed at once. `.some(nil)`:
    /// owed to the next binding (the account was not known at the join);
    /// `.some(id)`: owed to that account's binding. In memory only: a process
    /// that ends first sends the card at its next first foreground anyway.
    private var joinedOwed: String??
    private var growthTask: Task<Void, Never>?
    /// Whether this binding has already taken a foreground as the person's
    /// action (``noteForeground()``).
    private var foregroundNoted = false
    /// A growth re-send whose timer fired while the app was not in the
    /// foreground: sent at the next foreground, never from the background.
    /// Persisted per account, so a process that ends meanwhile still owes it.
    private(set) var resendOwed = false
    private let defaults: UserDefaults
    /// Whether the app is in the foreground; replaceable in a test.
    var isForeground: () -> Bool = { UIApplication.shared.applicationState == .active }
    /// Whether the anti-censorship connection is armed: then the device's
    /// address is Lakko's relay and no city is asked for.
    var antiCensorshipArmed: () -> Bool = { AntiCensorshipConfig.current.armed }
    /// The locate route; replaceable in a test.
    var locate: () async -> (city: String, country: String)? = { await DeviceCardCenter.fetchLocation() }
    /// The clock and the sleep a pairing screen's bounded wait for the city
    /// runs on (UX section 2.1); replaceable in a test.
    var monotonicNow: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    var pairingSleep: (Double) async -> Void = { seconds in
        try? await Task.sleep(nanoseconds: UInt64(max(seconds, 0) * 1_000_000_000))
    }
    /// The one locate request in flight, shared by the own card and a
    /// pairing screen, so opening the screen with a tap never asks twice.
    private var locateInFlight: Task<(city: String, country: String)?, Never>?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    // MARK: Binding

    func bind(accountIdB64: String, sender: @escaping (Data) async -> Void) {
        if self.accountIdB64 == accountIdB64 {
            self.sender = sender
            return
        }
        unbind()
        self.accountIdB64 = accountIdB64
        self.sender = sender
        if let data = defaults.data(forKey: DeviceCards.cardsKey(accountIdB64: accountIdB64)),
           let row = try? JSONDecoder().decode(Persisted.self, from: data) {
            cards = row.cards
            own = row.own
        }
        if let data = defaults.data(forKey: DeviceCards.cityKey(accountIdB64: accountIdB64)) {
            city = try? JSONDecoder().decode(CityAnswer.self, from: data)
        }
        showCity = defaults.object(forKey: DeviceCards.showCityKey(accountIdB64: accountIdB64)) as? Bool ?? true
        resendOwed = defaults.bool(forKey: DeviceCards.resendOwedKey(accountIdB64: accountIdB64))
        if let owed = joinedOwed, owed == nil || owed == accountIdB64 {
            sendJoinedCard()
        }
    }

    func unbind() {
        accountIdB64 = nil
        sender = nil
        city = nil
        growthTask?.cancel()
        growthTask = nil
        cards = [:]
        own = nil
        showCity = true
        foregroundNoted = false
        resendOwed = false
    }

    private func setResendOwed(_ owed: Bool) {
        resendOwed = owed
        guard let accountIdB64 else { return }
        if owed {
            defaults.set(true, forKey: DeviceCards.resendOwedKey(accountIdB64: accountIdB64))
        } else {
            defaults.removeObject(forKey: DeviceCards.resendOwedKey(accountIdB64: accountIdB64))
        }
    }

    /// The app is in the foreground (a launch, a return from the background,
    /// or an account bound while in front). The first foreground of a binding
    /// counts as the person's action: on a cold launch no touch reaches the
    /// recogniser before the account binds, and without this the device
    /// would hold no own card to send. A growth re-send owed from the
    /// background goes now. Nothing happens in the background or unbound.
    func noteForeground(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) {
        guard accountIdB64 != nil, isForeground() else { return }
        if resendOwed, own != nil {
            setResendOwed(false)
            Task { @MainActor [weak self] in await self?.sendOwn() }
        }
        if !foregroundNoted {
            foregroundNoted = true
            noteAction(nowMs: nowMs)
        }
    }

    private func persist() {
        guard let accountIdB64 else { return }
        if let data = try? JSONEncoder().encode(Persisted(cards: cards, own: own)) {
            defaults.set(data, forKey: DeviceCards.cardsKey(accountIdB64: accountIdB64))
        }
    }

    // MARK: The own card

    /// What this device says about itself now.
    func currentFields(nowMs: Int64) -> StoredDeviceCard {
        let facts = Self.platformFacts()
        let place = showCity && !antiCensorshipArmed() ? city : nil
        return StoredDeviceCard(
            platform: facts.platform, model: facts.model, appVersion: facts.appVersion,
            os: facts.os, city: place?.city ?? "", country: place?.country ?? "",
            updatedAtMs: nowMs, lastActiveMs: nowMs)
    }

    private func cityDue(nowMs: Int64) -> Bool {
        guard showCity, !antiCensorshipArmed() else { return false }
        guard let city else { return true }
        let age = nowMs - city.fetchedAtMs
        return age < 0 || age >= DeviceCards.cityRefreshMs
    }

    /// One locate request at a time; a second asker awaits the same answer.
    private func sharedLocate() async -> (city: String, country: String)? {
        if let inFlight = locateInFlight { return await inFlight.value }
        let task = Task { @MainActor [locate] in await locate() }
        locateInFlight = task
        let answer = await task.value
        if locateInFlight == task { locateInFlight = nil }
        return answer
    }

    /// Keep a city answer as the bound account's cached city.
    private func storeCity(_ answer: (city: String, country: String), account: String, nowMs: Int64) {
        city = CityAnswer(city: answer.city, country: answer.country, fetchedAtMs: nowMs)
        if let data = try? JSONEncoder().encode(city) {
            defaults.set(data, forKey: DeviceCards.cityKey(accountIdB64: account))
        }
    }

    /// The person did something on this device. Sends a fresh own card when
    /// the rules say so; never on a timer, never in the background.
    func noteAction(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) {
        guard let bound = accountIdB64, isForeground(), !deciding else { return }
        let due = cityDue(nowMs: nowMs)
        guard due || DeviceCards.shouldSend(last: own, current: currentFields(nowMs: nowMs), nowMs: nowMs)
        else { return }
        deciding = true
        Task { @MainActor in
            defer { deciding = false }
            if due, let answer = await sharedLocate(), accountIdB64 == bound {
                storeCity(answer, account: bound, nowMs: nowMs)
            }
            guard accountIdB64 == bound else { return }
            let fields = currentFields(nowMs: nowMs)
            guard DeviceCards.shouldSend(last: own, current: fields, nowMs: nowMs) else { return }
            own = fields
            persist()
            await sendOwn()
        }
    }

    /// The Privacy switch. Turning it off empties the city and sends at once.
    func setShowCity(_ on: Bool) {
        showCity = on
        if let accountIdB64 {
            defaults.set(on, forKey: DeviceCards.showCityKey(accountIdB64: accountIdB64))
        }
        noteAction()
    }

    /// UX section 2.1 — the screen that shows a pairing code or takes the
    /// six-digit code opened: start the one city request this pairing makes,
    /// signed in or not. Nothing is asked with the anti-censorship
    /// connection armed or the city switch off (a device not signed in has
    /// no switch of its own: the default, on). A signed-in device with a
    /// fresh cached city uses it and asks nothing. The answer is kept as the
    /// bound account's city; with no account bound it lives only in the
    /// returned ``PairingPlace``.
    func beginPairingPlace(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) -> PairingPlace {
        let place = PairingPlace(
            deadline: monotonicNow() + DeviceCards.pairingPlaceWaitSeconds,
            now: monotonicNow, sleep: pairingSleep)
        guard showCity, !antiCensorshipArmed() else {
            place.resolve(nil)
            return place
        }
        let bound = accountIdB64
        if bound != nil, !cityDue(nowMs: nowMs), let city {
            place.resolve(PairingPlace.Answer(city: city.city, country: city.country))
            return place
        }
        NSLog("device card: asking the city for the pairing payload")
        Task { @MainActor [weak self] in
            guard let self else {
                place.resolve(nil)
                return
            }
            let answer = await self.sharedLocate()
            if let answer, let bound, self.accountIdB64 == bound {
                self.storeCity(answer, account: bound, nowMs: nowMs)
            }
            place.resolve(answer.map { PairingPlace.Answer(city: $0.city, country: $0.country) })
        }
        return place
    }

    /// UX section 2.1 — the payload card once `place` has answered or its
    /// bound from the screen opening has passed, whichever comes first. A
    /// city that arrives later is not waited for.
    func pairingCardEnvelope(
        waitingFor place: PairingPlace,
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) async -> Data? {
        _ = await place.waitForAnswer()
        return pairingCardEnvelope(place: place, nowMs: nowMs())
    }

    /// UX section 2.1 — the card a joining device puts in its pairing payload
    /// (QR and the code route): what it would send as its own card now, with
    /// the city `place` received if it has, and the city switch and the
    /// anti-censorship rule exactly as for the card itself. `nil` when it
    /// cannot be composed: then the payload carries no card, never a failed
    /// pairing.
    func pairingCardEnvelope(
        place: PairingPlace? = nil,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) -> Data? {
        var fields = currentFields(nowMs: nowMs)
        if let answer = place?.answer, showCity, !antiCensorshipArmed() {
            fields.city = answer.city
            fields.country = answer.country
        }
        do {
            return try composeDeviceCardEnvelope(card: Self.ffi(fields))
        } catch {
            NSLog("device card: no card in the pairing payload: %@", String(describing: error))
            return nil
        }
    }

    /// UX section 3 — what this device's own row on the Devices screen shows:
    /// the fields it would put in its own card, never a received card.
    func ownRowCard(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) -> StoredDeviceCard {
        currentFields(nowMs: nowMs)
    }

    /// UX section 3 — this device has just joined the account's DeviceGroup
    /// from a pairing Welcome (QR or code route): its own card goes at once,
    /// not after the growth delay (that delay is for the devices that were
    /// already there) and without waiting for an action: the person is
    /// looking at this device. `accountIdB64` is the account it joined, or
    /// `nil` when not known yet; unbound, the card goes when that account
    /// binds. The city is not waited for: a later action sends it.
    func noteJoined(accountIdB64 joined: String?) {
        if let bound = accountIdB64, joined == nil || joined == bound {
            sendJoinedCard()
        } else {
            joinedOwed = .some(joined)
        }
    }

    private func sendJoinedCard(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) {
        joinedOwed = nil
        guard accountIdB64 != nil else { return }
        own = currentFields(nowMs: nowMs)
        persist()
        NSLog("device card: this device just joined; its card goes now")
        Task { @MainActor [weak self] in await self?.sendOwn() }
    }

    /// The own card's envelope, stamps verbatim; `nil` before the first one.
    func ownEnvelope() -> Data? {
        guard let own else { return nil }
        return try? composeDeviceCardEnvelope(card: Self.ffi(own))
    }

    private func sendOwn() async {
        guard let envelope = ownEnvelope(), let sender else { return }
        await sender(envelope)
        NSLog("device card: this %@ device's card queued to send", own?.platform.rawValue ?? "unknown")
    }

    /// The DeviceGroup grew: re-send the stored own card, unchanged, after
    /// up to 300 ms (ADR-0058 discipline), so the new device learns this one.
    func scheduleResend(random: Double = Double.random(in: 0..<1)) {
        guard accountIdB64 != nil, own != nil, growthTask == nil else { return }
        let bound = accountIdB64
        let delay = DeviceCards.growthDelayMs(random: random)
        growthTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000)
            guard let self, !Task.isCancelled else { return }
            self.growthTask = nil
            guard self.accountIdB64 == bound else { return }
            guard self.isForeground() else {
                // Not dropped: owed, and sent at the next foreground.
                self.setResendOwed(true)
                NSLog("device card: growth re-send owed until the next foreground")
                return
            }
            self.setResendOwed(false)
            await self.sendOwn()
        }
    }

    // MARK: Received cards

    /// A card arrived from `senderKeyHex`, resolved at receipt; `rosterKeysHex`
    /// is the DeviceGroup roster at that moment.
    func receive(senderKeyHex: String, card: StoredDeviceCard, rosterKeysHex: [String]) {
        guard accountIdB64 != nil else { return }
        guard let next = DeviceCards.receive(cards, senderKeyHex: senderKeyHex, card: card,
                                             rosterKeysHex: rosterKeysHex) else {
            NSLog("device card: received a %@ device's card and refused it", card.platform.rawValue)
            return
        }
        cards = next
        persist()
        NSLog("device card: received a %@ device's card and stored it", card.platform.rawValue)
    }

    /// UX section 2.1 — the card the new device put in its pairing payload,
    /// kept as the card of its leaf (`keyHex`) once the add landed, so its
    /// row is named at once. Lowest stamp: its own card replaces it.
    /// Nothing when a card from that leaf is already stored, the leaf is not
    /// in `rosterKeysHex`, or the center is bound to another account.
    func seedFromPairing(
        accountIdB64 account: String, keyHex: String, card: StoredDeviceCard, rosterKeysHex: [String]
    ) {
        guard accountIdB64 == account,
              let next = DeviceCards.seedFromPairing(
                  cards, keyHex: keyHex, card: card, rosterKeysHex: rosterKeysHex) else { return }
        cards = next
        persist()
        NSLog("device card: the new %@ device's card from its pairing payload stored", card.platform.rawValue)
    }

    /// The received cards and own card stored for `account`: the live state
    /// when bound to it, else the persisted row.
    private func row(for account: String) -> Persisted {
        if accountIdB64 == account { return Persisted(cards: cards, own: own) }
        guard let data = defaults.data(forKey: DeviceCards.cardsKey(accountIdB64: account)),
              let row = try? JSONDecoder().decode(Persisted.self, from: data) else {
            return Persisted(cards: [:], own: nil)
        }
        return row
    }

    /// ADR-0080 amendment (2026-11-23), confirming device — the cards handed
    /// to a device being added, in the pairing state bundle: every stored
    /// card of a leaf in `rosterKeysHex` (the DeviceGroup after the add),
    /// plus this device's own, its last card as sent (stamps verbatim) or,
    /// before its first one, its current fields at the lowest stamp so any
    /// card it sends later replaces it. A card that does not compose is left
    /// out.
    func cardsForStateBundle(
        accountIdB64 account: String, ownKeyHex: String, rosterKeysHex: [String],
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) -> [StateBundle.CarriedDeviceCard] {
        let stored = row(for: account)
        var ownCard = stored.own ?? currentFields(nowMs: nowMs)
        if stored.own == nil { ownCard.updatedAtMs = 0 }
        var picked: [(String, StoredDeviceCard)] = [(ownKeyHex, ownCard)]
        for (key, card) in stored.cards where key != ownKeyHex && rosterKeysHex.contains(key) {
            picked.append((key, card))
        }
        return picked.compactMap { key, card in
            guard let keyBytes = StateBundle.hexDecode(key),
                  let envelope = try? composeDeviceCardEnvelope(card: Self.ffi(card)) else { return nil }
            return StateBundle.CarriedDeviceCard(
                leafKeyB64: keyBytes.base64EncodedString(), envelopeB64: envelope.base64EncodedString())
        }
    }

    /// ADR-0080 amendment (2026-11-23), new device — store the cards the
    /// confirming device handed over (``DeviceCards/acceptCarried(_:entries:rosterKeysHex:ownKeyHex:)``).
    /// Works before the center is bound to the account (a restore route
    /// lands the session later): the persisted row is updated and read at
    /// the bind. Returns how many were kept.
    @discardableResult
    func storeCarried(
        accountIdB64 account: String, entries: [(keyHex: String, card: StoredDeviceCard)],
        rosterKeysHex: [String], ownKeyHex: String
    ) -> Int {
        let stored = row(for: account)
        guard let next = DeviceCards.acceptCarried(
            stored.cards, entries: entries, rosterKeysHex: rosterKeysHex, ownKeyHex: ownKeyHex) else { return 0 }
        let kept = next.filter { stored.cards[$0.key] != $0.value }.count
        if accountIdB64 == account {
            cards = next
            persist()
        } else if let data = try? JSONEncoder().encode(Persisted(cards: next, own: stored.own)) {
            defaults.set(data, forKey: DeviceCards.cardsKey(accountIdB64: account))
        }
        return kept
    }

    /// The roster changed: drop cards of leaves that left.
    func prune(rosterKeysHex: [String]) {
        guard accountIdB64 != nil,
              let next = DeviceCards.prune(cards, rosterKeysHex: rosterKeysHex) else { return }
        cards = next
        persist()
    }

    // MARK: Conversions and platform facts

    nonisolated static func ffi(_ card: StoredDeviceCard) -> FfiDeviceCard {
        let platform: FfiDevicePlatform
        switch card.platform {
        case .ios: platform = .ios
        case .ipados: platform = .ipados
        case .macos: platform = .macos
        case .android: platform = .android
        case .web: platform = .web
        case .desktop: platform = .desktop
        }
        return FfiDeviceCard(
            updatedAtMs: UInt64(max(card.updatedAtMs, 0)),
            lastActiveMs: UInt64(max(card.lastActiveMs, 0)),
            platform: platform, model: card.model, appVersion: card.appVersion,
            os: card.os, city: card.city, country: card.country)
    }

    nonisolated static func stored(_ card: FfiDeviceCard) -> StoredDeviceCard {
        let platform: DeviceCardPlatform
        switch card.platform {
        case .ios: platform = .ios
        case .ipados: platform = .ipados
        case .macos: platform = .macos
        case .android: platform = .android
        case .web: platform = .web
        case .desktop: platform = .desktop
        }
        return StoredDeviceCard(
            platform: platform, model: card.model, appVersion: card.appVersion, os: card.os,
            city: card.city, country: card.country,
            updatedAtMs: Int64(clamping: card.updatedAtMs), lastActiveMs: Int64(clamping: card.lastActiveMs))
    }

    struct PlatformFacts {
        var platform: DeviceCardPlatform
        var model: String
        var os: String
        var appVersion: String
    }

    static func platformFacts() -> PlatformFacts {
        let device = UIDevice.current
        let appVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        if ProcessInfo.processInfo.isiOSAppOnMac {
            return PlatformFacts(
                platform: .macos, model: "Mac",
                os: "macOS \(ProcessInfo.processInfo.operatingSystemVersionString)", appVersion: appVersion)
        }
        let pad = device.userInterfaceIdiom == .pad
        return PlatformFacts(
            platform: pad ? .ipados : .ios,
            model: DeviceCards.appleModelName(hardwareIdentifier()),
            os: "\(pad ? "iPadOS" : "iOS") \(device.systemVersion)",
            appVersion: appVersion)
    }

    /// `iPhone17,1` and the like; in the Simulator, the simulated model's.
    static func hardwareIdentifier() -> String {
        if let simulated = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] {
            return simulated
        }
        var info = utsname()
        uname(&info)
        let bytes = withUnsafeBytes(of: &info.machine) { Array($0) }
        let end = bytes.firstIndex(of: 0) ?? bytes.endIndex
        return String(decoding: bytes[..<end], as: UTF8.self)
    }

    /// `GET /discovery/locate` on the direct path (never asked while the
    /// anti-censorship connection is armed). No session, no token.
    static func fetchLocation() async -> (city: String, country: String)? {
        guard let url = URL(string: DiscoveryClient.defaultBaseURL + "/discovery/locate") else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.httpShouldHandleCookies = false
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return (body["city"] as? String ?? "", body["country"] as? String ?? "")
    }
}

/// UX section 2.1 — the city one pairing screen asked for when it opened,
/// held for that pairing only. The QR waits for it at most
/// ``DeviceCards/pairingPlaceWaitSeconds`` from the opening; the code route
/// takes it if it has arrived by the time the code is typed.
@MainActor
final class PairingPlace {

    struct Answer: Equatable {
        var city: String
        var country: String
    }

    /// The answer, once it arrived; `nil` before and when none came.
    private(set) var answer: Answer?
    /// The request ended (with or without an answer), or none was made.
    private(set) var settled = false
    private var timedOut = false
    private let deadline: TimeInterval
    private let now: () -> TimeInterval
    private let sleep: (Double) async -> Void
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private var timer: Task<Void, Never>?

    init(deadline: TimeInterval, now: @escaping () -> TimeInterval, sleep: @escaping (Double) async -> Void) {
        self.deadline = deadline
        self.now = now
        self.sleep = sleep
    }

    func resolve(_ answer: Answer?) {
        guard !settled else { return }
        self.answer = answer
        settled = true
        timer?.cancel()
        timer = nil
        release()
    }

    /// Wait until the answer arrives or the bound passes, then return what
    /// is there.
    func waitForAnswer() async -> Answer? {
        if settled || timedOut { return answer }
        let remaining = deadline - now()
        guard remaining > 0 else {
            timedOut = true
            return answer
        }
        if timer == nil {
            timer = Task { @MainActor [weak self, sleep] in
                await sleep(remaining)
                guard !Task.isCancelled, let self else { return }
                self.timedOut = true
                self.timer = nil
                self.release()
            }
        }
        await withCheckedContinuation { waiters.append($0) }
        return answer
    }

    private func release() {
        let pending = waiters
        waiters = []
        for waiter in pending { waiter.resume() }
    }
}
