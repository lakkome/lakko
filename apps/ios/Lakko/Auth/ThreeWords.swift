// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// "Three words from your phrase": the pure rules of the local three-word
/// check. Mirror of `apps/web/src/auth/recover/three-words.ts`.
///
/// The check is LOCAL, like a wallet's "enter words 7, 3 and 12". A device
/// that holds the account's keys also holds the recovery phrase
/// (``DevicePhrase``). Forgot the password on such a device: the client draws
/// three fresh random positions, the person types those words, the client
/// compares them with the phrase it holds, and on a match signs in with the
/// seed it already has. The server takes no part in the check and stores
/// nothing about the words.
///
/// ## What the budget is, stated plainly
///
/// The budget is a CLIENT-SIDE gate in front of a credential this device
/// already holds. The phrase sits in this device's Keychain; someone who can
/// run their own code as this app does not need to guess three words. The
/// budget therefore protects against exactly one thing: a person at the
/// unlocked device typing guesses into the form. It is not a server limit
/// and must never be described as one.
enum ThreeWords {

    /// How many words the person answers with.
    static let wordCount = 3

    /// Misses allowed before the route pauses.
    static let maxMisses = 5

    /// How long the route pauses after the last allowed miss.
    static let pauseMs: Int64 = 15 * 60 * 1000

    // MARK: - Row visibility

    /// Whether the recovery list offers "Three words from your phrase" on
    /// this device. Every input is a local fact, so the answer says nothing
    /// about any account on the server:
    ///
    ///   - an account is cached on this device and has a username (the
    ///     sign-in that follows the check is addressed by the name's login
    ///     handle; a guest account has none);
    ///   - the device holds the recovery phrase for that account;
    ///   - the device holds the account's keys (else the sign-in would only
    ///     reach "Restore your keys", which three words cannot answer).
    static func offers(cachedUsername: String?, phraseOnDevice: Bool, keysOnDevice: Bool) -> Bool {
        guard let cachedUsername,
              !cachedUsername.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return false
        }
        return phraseOnDevice && keysOnDevice
    }

    // MARK: - Guess budget

    /// The budget for one account on this device.
    struct Budget: Equatable, Codable {
        /// Misses since the last success or the last pause.
        var misses: Int
        /// When the pause ends (unix ms), or `nil` when the route is open.
        var pausedUntilMs: Int64?

        static let fresh = Budget(misses: 0, pausedUntilMs: nil)
    }

    /// The budget as it stands at `nowMs`: a pause that has run out is over
    /// and the count starts again.
    static func budget(_ budget: Budget, at nowMs: Int64) -> Budget {
        if let until = budget.pausedUntilMs, nowMs >= until { return .fresh }
        return budget
    }

    /// Milliseconds of pause left at `nowMs`; 0 when the route is open. A
    /// device clock set back cannot stretch a pause past its full length.
    static func pauseRemainingMs(_ budget: Budget, at nowMs: Int64) -> Int64 {
        guard let until = Self.budget(budget, at: nowMs).pausedUntilMs else { return 0 }
        return min(pauseMs, max(0, until - nowMs))
    }

    /// Tries left before the pause, at `nowMs` (0 while paused).
    static func triesLeft(_ budget: Budget, at nowMs: Int64) -> Int {
        let current = Self.budget(budget, at: nowMs)
        if current.pausedUntilMs != nil { return 0 }
        return max(0, maxMisses - current.misses)
    }

    /// The budget after one more miss at `nowMs`. The fifth miss starts the
    /// pause. A miss while paused does not happen (the form is closed) and
    /// changes nothing.
    static func afterMiss(_ budget: Budget, at nowMs: Int64) -> Budget {
        let current = Self.budget(budget, at: nowMs)
        if current.pausedUntilMs != nil { return current }
        let misses = current.misses + 1
        return misses >= maxMisses
            ? Budget(misses: misses, pausedUntilMs: nowMs + pauseMs)
            : Budget(misses: misses, pausedUntilMs: nil)
    }

    // MARK: - Presentation

    /// A wait as `m:ss` (Latin digits in every language, like the word
    /// numbers), rounded up to the next second so it never reads 0:00 while
    /// the wait still runs.
    static func formatCountdown(remainingMs: Int64) -> String {
        let totalSecs = max(0, (remainingMs + 999) / 1000)
        return String(format: "%d:%02d", locale: Locale(identifier: "en_US_POSIX"), totalSecs / 60, totalSecs % 60)
    }
}

/// Where the three-word budget is kept: `UserDefaults`, per account, so a
/// relaunch does not reset it. Cleared on a success, on Forget device and on
/// account deletion (``AccountLocalDataWipe``).
struct ThreeWordsBudgetStore {
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        "lakko.three-words-budget.v1.\(accountIdB64)"
    }

    /// The stored budget, or a fresh one when nothing (or nothing readable)
    /// is stored. A malformed row reads as fresh rather than locking the
    /// route forever; a row whose pause lies further ahead than a full pause
    /// (a clock that was set back) is clamped by
    /// ``ThreeWords/pauseRemainingMs(_:at:)``.
    func load(accountIdB64: String) -> ThreeWords.Budget {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)),
              let stored = try? JSONDecoder().decode(ThreeWords.Budget.self, from: data),
              stored.misses >= 0 else {
            return .fresh
        }
        return ThreeWords.Budget(
            misses: min(stored.misses, ThreeWords.maxMisses),
            pausedUntilMs: stored.pausedUntilMs
        )
    }

    func save(_ budget: ThreeWords.Budget, accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(budget) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}
