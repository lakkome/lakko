// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// Device cards (ADR-0080, docs/ux/second-device.md section 3): the pure
// rules. The same rules are written for the web (`device-cards.ts`) and
// Android (`DeviceCards.kt`); the wire format is Rust's
// (`lakko-contacts::device_card`).

/// The closed set of platforms a card may name.
public enum DeviceCardPlatform: String, Codable, Equatable, CaseIterable {
    case ios, ipados, macos, android, web, desktop
}

/// A device card as this client keeps it. Times are UNIX milliseconds.
public struct StoredDeviceCard: Codable, Equatable {
    public var platform: DeviceCardPlatform
    public var model: String
    public var appVersion: String
    public var os: String
    public var city: String
    public var country: String
    public var updatedAtMs: Int64
    public var lastActiveMs: Int64

    /// `true` when the two differ in a field other than the times.
    func fieldsDiffer(from other: StoredDeviceCard) -> Bool {
        platform != other.platform || model != other.model || appVersion != other.appVersion
            || os != other.os || city != other.city || country != other.country
    }
}

public enum DeviceCards {

    /// A device sends a fresh card after an action when its last one is older
    /// than this (ADR-0080 section 3).
    public static let refreshMs: Int64 = 30 * 60 * 1000
    /// The longest wait before the growth re-send of the own card.
    public static let growthMaxDelayMs: Int64 = 30_000
    /// The city is asked for again at most this often (chosen, not measured).
    public static let cityRefreshMs: Int64 = 6 * 60 * 60 * 1000

    /// Keep a received card, keyed by the sender's DeviceGroup leaf key (hex)
    /// resolved at receipt. `nil` when nothing changes: the sender is not in
    /// the roster, or the card is not strictly newer than the stored one.
    public static func receive(
        _ cards: [String: StoredDeviceCard],
        senderKeyHex: String,
        card: StoredDeviceCard,
        rosterKeysHex: [String]
    ) -> [String: StoredDeviceCard]? {
        guard rosterKeysHex.contains(senderKeyHex) else { return nil }
        if let stored = cards[senderKeyHex], card.updatedAtMs <= stored.updatedAtMs { return nil }
        var next = cards
        next[senderKeyHex] = card
        return next
    }

    /// Drop the card of every leaf that left. `nil` when none did.
    public static func prune(
        _ cards: [String: StoredDeviceCard], rosterKeysHex: [String]
    ) -> [String: StoredDeviceCard]? {
        let keep = cards.filter { rosterKeysHex.contains($0.key) }
        return keep.count == cards.count ? nil : keep
    }

    /// Whether the person's action at `nowMs` must send a fresh own card: none
    /// yet, a field other than the times changed, or the last one is older
    /// than ``refreshMs``. A stamp from the future counts as old.
    public static func shouldSend(last: StoredDeviceCard?, current: StoredDeviceCard, nowMs: Int64) -> Bool {
        guard let last else { return true }
        if last.fieldsDiffer(from: current) { return true }
        let age = nowMs - last.updatedAtMs
        return age < 0 || age >= refreshMs
    }

    /// A random delay in `[0, 30 s)` for the growth re-send.
    public static func growthDelayMs(random: Double = Double.random(in: 0..<1)) -> Int64 {
        let clamped = random.isFinite ? min(max(random, 0), 0.999_999) : 0
        return Int64(clamped * Double(growthMaxDelayMs))
    }

    // MARK: The screen

    /// Which icon a row shows.
    public enum IconKind: Equatable {
        case phone, tablet, computer, browser, unknown
    }

    public static func iconKind(_ platform: DeviceCardPlatform?) -> IconKind {
        switch platform {
        case .ios, .android: return .phone
        case .ipados: return .tablet
        case .macos, .desktop: return .computer
        case .web: return .browser
        case .none: return .unknown
        }
    }

    /// A device of the roster, with its card when one arrived.
    public struct RowInput: Equatable {
        public var keyHex: String
        public var leafIndex: UInt32
        public var isSelf: Bool
        public var card: StoredDeviceCard?
    }

    /// This device, then the others newest activity first; cardless last, in
    /// roster order.
    public static func order(_ rows: [RowInput]) -> (current: RowInput?, others: [RowInput]) {
        let me = rows.first { $0.isSelf }
        let others = rows.filter { !$0.isSelf }.sorted { a, b in
            switch (a.card, b.card) {
            case let (ca?, cb?):
                if ca.lastActiveMs != cb.lastActiveMs { return ca.lastActiveMs > cb.lastActiveMs }
                return a.leafIndex < b.leafIndex
            case (.some, .none): return true
            case (.none, .some): return false
            case (.none, .none): return a.leafIndex < b.leafIndex
            }
        }
        return (me, others)
    }

    /// The words a row needs, translated by the caller.
    public struct Words {
        public var device: String
        public var online: String
        public var app: (String) -> String
        public var place: (String, String) -> String
        public var countryName: (String) -> String
    }

    /// What a row shows.
    public struct RowView: Equatable {
        public var title: String
        public var detail: String
        public var place: String
        public var activity: String
        public var icon: IconKind
    }

    public static func view(
        _ row: RowInput, words: Words, nowMs: Int64, calendar: Calendar, locale: Locale
    ) -> RowView {
        let activity: String
        if row.isSelf {
            activity = words.online
        } else if let card = row.card, card.lastActiveMs > 0 {
            activity = ChatListFormatting.lastMessageTime(
                ms: card.lastActiveMs, nowMs: nowMs, calendar: calendar, locale: locale)
        } else {
            activity = ""
        }
        guard let card = row.card else {
            return RowView(title: words.device, detail: "", place: "", activity: activity, icon: .unknown)
        }
        let app = words.app(card.appVersion)
        let detail = card.os.isEmpty ? app : "\(app) · \(card.os)"
        let country = card.country.isEmpty ? "" : words.countryName(card.country)
        let place: String
        if !card.city.isEmpty, !country.isEmpty {
            place = words.place(card.city, country)
        } else if !card.city.isEmpty {
            place = card.city
        } else {
            place = country
        }
        return RowView(
            title: card.model.isEmpty ? words.device : card.model,
            detail: detail, place: place, activity: activity, icon: iconKind(card.platform))
    }

    // MARK: This device

    /// The marketing name of an Apple hardware identifier, from a small table
    /// of common iPhone models; the identifier itself otherwise.
    public static func appleModelName(_ identifier: String) -> String {
        appleModelNames[identifier] ?? identifier
    }

    static let appleModelNames: [String: String] = [
        "iPhone14,4": "iPhone 13 mini",
        "iPhone14,5": "iPhone 13",
        "iPhone14,2": "iPhone 13 Pro",
        "iPhone14,3": "iPhone 13 Pro Max",
        "iPhone14,6": "iPhone SE",
        "iPhone14,7": "iPhone 14",
        "iPhone14,8": "iPhone 14 Plus",
        "iPhone15,2": "iPhone 14 Pro",
        "iPhone15,3": "iPhone 14 Pro Max",
        "iPhone15,4": "iPhone 15",
        "iPhone15,5": "iPhone 15 Plus",
        "iPhone16,1": "iPhone 15 Pro",
        "iPhone16,2": "iPhone 15 Pro Max",
        "iPhone17,3": "iPhone 16",
        "iPhone17,4": "iPhone 16 Plus",
        "iPhone17,1": "iPhone 16 Pro",
        "iPhone17,2": "iPhone 16 Pro Max",
        "iPhone17,5": "iPhone 16e",
    ]

    /// Storage keys, per account (``AccountLocalDataWipe`` removes them).
    public static func cardsKey(accountIdB64: String) -> String { "lakko.device-cards.v1.\(accountIdB64)" }
    public static func cityKey(accountIdB64: String) -> String { "lakko.device-city.v1.\(accountIdB64)" }
    public static func showCityKey(accountIdB64: String) -> String { "lakko.device-city-shown.v1.\(accountIdB64)" }
    /// A growth re-send owed until the next foreground (``DeviceCardCenter``).
    public static func resendOwedKey(accountIdB64: String) -> String { "lakko.device-card-resend-owed.v1.\(accountIdB64)" }
}
