// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import XCTest

@testable import Lakko

/// Keeps the preference domains the tests write to under CFPreferences' size
/// limit, run after run on one simulator.
///
/// Many tests bootstrap a chat repository (and its mirrors) under a fresh
/// random account and never wipe it, so per-account rows piled up across runs
/// in the App Group suite (`me.lakko.snapshot-leaf-key.v1.<account>`,
/// `me.lakko.continuity-mirror.v1.<account>`, `me.lakko.push-route.v2.…`):
/// 30 289 keys, a 4.8 MB plist, on the test simulator by 2026-11-13. Past
/// 4 194 304 bytes CFPreferences refuses the domain ("Attempting to store >=
/// 4194304 bytes … is invalid"), so writes and removals in the App Group suite
/// stopped landing reliably, and `AccountLocalDataWipeTests` saw rows the wipe
/// had removed come back and seeded rows vanish, depending on how much an
/// earlier run had left.
///
/// Before the bundle starts and after it finishes, every row of the App
/// Group suite and of the standard domain that names a 16-byte account id
/// (standard base64, 24 characters ending `==`) other than the persisted
/// session's account is removed. Installed by the principal class
/// (``NetworkGuard``).
final class TestDefaultsHygiene: NSObject, XCTestObservation {

    override init() {
        super.init()
        XCTestObservationCenter.shared.addTestObserver(self)
    }

    func testBundleWillStart(_ testBundle: Bundle) { Self.prune() }
    func testBundleDidFinish(_ testBundle: Bundle) { Self.prune() }

    /// A 16-byte account id in standard base64.
    private static let accountPattern = try? NSRegularExpression(pattern: "[A-Za-z0-9+/]{22}==")

    /// The account ids in `key`.
    static func accountIds(in key: String) -> [String] {
        guard let accountPattern else { return [] }
        let range = NSRange(key.startIndex..., in: key)
        return accountPattern.matches(in: key, range: range).compactMap {
            Range($0.range, in: key).map { String(key[$0]) }
        }
    }

    /// Whether `key` is a per-account row of another account than `keep`.
    static func isDebris(_ key: String, keep: String?) -> Bool {
        let ids = accountIds(in: key)
        guard !ids.isEmpty else { return false }
        if let keep, ids.contains(keep) { return false }
        return true
    }

    /// Remove the debris rows from `defaults`. Returns how many went.
    @discardableResult
    static func prune(_ defaults: UserDefaults, keep: String?) -> Int {
        var removed = 0
        for key in defaults.dictionaryRepresentation().keys where isDebris(key, keep: keep) {
            defaults.removeObject(forKey: key)
            removed += 1
        }
        return removed
    }

    static func prune() {
        let keep = SessionStore().load()?.accountIdB64
        var removed = prune(.standard, keep: keep)
        if let shared = SharedStorage.sharedDefaults() {
            removed += prune(shared, keep: keep)
        }
        NSLog("[test-hygiene] removed %d per-account preference row(s) of other accounts", removed)
    }
}

final class TestDefaultsHygieneTests: XCTestCase {
    func testOnlyRowsNamingAnotherAccountAreDebris() {
        let a = "////++++WhEiM0RVZneIoQ=="
        let b = "CxsrO0tba3uLm6u7y9vrsg=="
        XCTAssertTrue(TestDefaultsHygiene.isDebris("me.lakko.continuity-mirror.v1.\(a)", keep: b))
        XCTAssertFalse(TestDefaultsHygiene.isDebris("me.lakko.continuity-mirror.v1.\(b)", keep: b))
        XCTAssertFalse(TestDefaultsHygiene.isDebris("lakko.l10n.notification-words", keep: b))
        XCTAssertTrue(TestDefaultsHygiene.isDebris("me.lakko.call-peer.v2.\(a).aa00", keep: nil))
    }
}
