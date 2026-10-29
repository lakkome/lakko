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
            if due, let answer = await locate(), accountIdB64 == bound {
                city = CityAnswer(city: answer.city, country: answer.country, fetchedAtMs: nowMs)
                if let data = try? JSONEncoder().encode(city) {
                    defaults.set(data, forKey: DeviceCards.cityKey(accountIdB64: bound))
                }
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

    /// UX section 2.1 — the card a joining device puts in its pairing payload
    /// (QR and the code route): what it would send as its own card now, with
    /// the city switch and the anti-censorship rule exactly as for the card
    /// itself. `nil` when it cannot be composed: then the payload carries no
    /// card, never a failed pairing.
    func pairingCardEnvelope(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) -> Data? {
        do {
            return try composeDeviceCardEnvelope(card: Self.ffi(currentFields(nowMs: nowMs)))
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

    /// The roster changed: drop cards of leaves that left.
    func prune(rosterKeysHex: [String]) {
        guard accountIdB64 != nil,
              let next = DeviceCards.prune(cards, rosterKeysHex: rosterKeysHex) else { return }
        cards = next
        persist()
    }

    // MARK: Conversions and platform facts

    static func ffi(_ card: StoredDeviceCard) -> FfiDeviceCard {
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

    static func stored(_ card: FfiDeviceCard) -> StoredDeviceCard {
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
