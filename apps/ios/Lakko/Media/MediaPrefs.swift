// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 — per-device media preferences. Mirror of
/// `apps/web/src/media/media-prefs.ts`.
///
/// All of these are conveniences a user can lose without harm, so every read
/// falls back to the spec's default:
///
///   * the composer's record mode, mic or camera (UX spec §1, "remembered per
///     device");
///   * "Autoplay video notes", default OFF (§3 — autoplay downloads media);
///   * the local-only "played" marker on voice messages and video notes (§8 —
///     never sent, it is not a read receipt).
///
/// The first three are about this device and stay device-wide. The played
/// marker is about one account's messages, so since 2026-09-30 it is filed
/// under the account (`lakko.media.played.<account>`). A read resolves the
/// account at each call (by default the one ``AcceptedContactsStore`` has
/// bound); a write names the account its work was started for and is made
/// only while that account is still the bound one. No account: nothing reads
/// as played and nothing is written. Logout keeps it;
/// forget-device drops it through ``clearPlayed(accountIdB64:)``.
public struct MediaPrefs {

    public static let recordModeKey = "lakko.composer.recordMode"
    public static let autoplayKey = "lakko.media.autoplayVideoNotes"
    /// The device-wide played set used before it was per account. Never read;
    /// removed by ``clearPlayed(accountIdB64:)``.
    public static let playedKey = "lakko.media.played"
    /// Per-account played set; the account id follows.
    public static let playedKeyPrefix = "lakko.media.played."
    /// The played set is capped so it cannot grow without bound; the oldest
    /// entries fall off first (an old message then shows its dot again, which
    /// is harmless).
    public static let maxPlayed = 2000

    private let defaults: UserDefaults
    private let accountIdB64: () -> String?

    /// The played set acts on the account ``AcceptedContactsStore`` has bound.
    public init(defaults: UserDefaults = .standard) {
        self.init(defaults: defaults, accountIdB64: { AcceptedContactsStore.boundAccountIdB64 })
    }

    /// The played set acts on the account `accountIdB64` names at each call.
    init(defaults: UserDefaults, accountIdB64: @escaping () -> String?) {
        self.defaults = defaults
        self.accountIdB64 = accountIdB64
    }

    private var playedStorageKey: String? {
        accountIdB64().map { Self.playedKeyPrefix + $0 }
    }

    public var recordMode: CaptureKind {
        get { defaults.string(forKey: Self.recordModeKey) == CaptureKind.videoNote.rawValue ? .videoNote : .voice }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Self.recordModeKey) }
    }

    public var autoplayVideoNotes: Bool {
        get { defaults.bool(forKey: Self.autoplayKey) }
        nonmutating set { defaults.set(newValue, forKey: Self.autoplayKey) }
    }

    public func playedKeys() -> [String] {
        guard let storageKey = playedStorageKey else { return [] }
        return defaults.stringArray(forKey: storageKey) ?? []
    }

    /// The account the played set acts on now (the one bound), for a caller
    /// that captures it when its work starts and hands it back to
    /// ``markPlayed(_:accountIdB64:)``.
    public var boundAccountIdB64: String? { accountIdB64() }

    /// Mark `key` played for `accountIdB64`, the account the caller's work
    /// was started for (a chat screen captures it when it opens). Written only
    /// while that account is still the bound one: a late mark from account A's
    /// screen, finishing after B signed in, would otherwise land in B's set.
    public func markPlayed(_ key: String, accountIdB64 captured: String?) {
        guard let account = captured, account == accountIdB64() else { return }
        // Not written once the account's session has ended (``AccountStoreGate``).
        guard let gate = AccountStoreGate.enter(account) else { return }
        defer { gate.leave() }
        let storageKey = Self.playedKeyPrefix + account
        var list = defaults.stringArray(forKey: storageKey) ?? []
        guard !list.contains(key) else { return }
        list.append(key)
        if list.count > Self.maxPlayed { list.removeFirst(list.count - Self.maxPlayed) }
        defaults.set(list, forKey: storageKey)
    }

    /// Forget-device: drop one account's played set (and the old device-wide
    /// one). Other accounts' stay.
    public func clearPlayed(accountIdB64: String) {
        defaults.removeObject(forKey: Self.playedKeyPrefix + accountIdB64)
        defaults.removeObject(forKey: Self.playedKey)
    }
}
