// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0061 S6 — signed translation updates on iOS: the pure launch rule and
// cadence, and the updater end to end over the real `l10nAcceptBundle` with
// bundles signed by a TEST key.
//
// Fixtures: the key and the three bundles below were made with
// `lakko-l10n keygen` / `lakko-l10n sign` from a throwaway seed that was
// never committed. The public key is a test key and is pinned nowhere. Each
// bundle carries one payload, `{"es": {"settings.about.version":
// "Versión (actualizada)", "auth.error.wrong_credentials": "PISADO"}}`;
// the second key is security-critical in the built-in catalog, so the core
// must never take it from a bundle.
private enum Fixture {
    static let publicKey = "et0WY-7UuJ0bAo8MepnbwLNr9XvIEM0PN9uLKTM7FgI"
    /// version 2, min_release 1.
    static let v2 = "AQAAAAAAAAACAAAAAAAAAAEAAAGg9rOGHgAAAHx7ImZvcm1hdCI6MSwibGFuZ3VhZ2VzIjp7ImVzIjp7InNldHRpbmdzLmFib3V0LnZlcnNpb24iOiJWZXJzacOzbiAoYWN0dWFsaXphZGEpIiwiYXV0aC5lcnJvci53cm9uZ19jcmVkZW50aWFscyI6IlBJU0FETyJ9fX0KXLf9FEpzevY4WNOxHQ81Gg5qFyznc5hjIgE6wyoDVp7sojliKc0TZYRHnvNfturaT9mtIDL8nw9uWMaRsGSQDw=="
    /// version 3, min_release 99 (needs a newer app).
    static let v3TooNew = "AQAAAAAAAAADAAAAAAAAAGMAAAGg9rOGIgAAAHx7ImZvcm1hdCI6MSwibGFuZ3VhZ2VzIjp7ImVzIjp7InNldHRpbmdzLmFib3V0LnZlcnNpb24iOiJWZXJzacOzbiAoYWN0dWFsaXphZGEpIiwiYXV0aC5lcnJvci53cm9uZ19jcmVkZW50aWFscyI6IlBJU0FETyJ9fX0KevqbtCKj+83SFkboIVJD3xDbhQo6u+tZqkfQLwyE8igUGmyYgEgX5Pld5ldcR/zuatN1gVAKVK7ptFBzJKY6Cw=="
    /// version 5, min_release 1.
    static let v5 = "AQAAAAAAAAAFAAAAAAAAAAEAAAGg9rOGJQAAAHx7ImZvcm1hdCI6MSwibGFuZ3VhZ2VzIjp7ImVzIjp7InNldHRpbmdzLmFib3V0LnZlcnNpb24iOiJWZXJzacOzbiAoYWN0dWFsaXphZGEpIiwiYXV0aC5lcnJvci53cm9uZ19jcmVkZW50aWFscyI6IlBJU0FETyJ9fX0KS+bbRBRAK8Y0uLuXyPMyiF7y/WfJGZCHmSzJRluXe8B6RMhn30ijplQDjfvZOmbsaQuDRywifjRvwW+0EOXBDw=="

    static func bytes(_ b64: String) -> Data { Data(base64Encoded: b64)! }
    static var key: Data { base64UrlDecodeNoPad(publicKey)! }
    static let updatedVersionRow = "Versión (actualizada)"
}

private final class MemoryStore: TranslationBundleStore {
    var stored: StoredTranslationBundle?
    var failSave = false
    func load() -> StoredTranslationBundle? { stored }
    func save(_ bundle: StoredTranslationBundle) throws {
        if failSave { throw CocoaError(.fileWriteUnknown) }
        stored = bundle
    }
    func delete() { stored = nil }
}

// MARK: - Pure rules

final class TranslationUpdatePolicyTests: XCTestCase {
    func testStoredBundleIsUsedOnlyWhenItReverifiesAsTheSameVersion() {
        XCTAssertEqual(storedTranslationVerdict(storedVersion: 4, reverifiedVersion: 4), .use)
        XCTAssertEqual(storedTranslationVerdict(storedVersion: 4, reverifiedVersion: nil), .discard)
        XCTAssertEqual(storedTranslationVerdict(storedVersion: 4, reverifiedVersion: 5), .discard)
        XCTAssertEqual(storedTranslationVerdict(storedVersion: 4, reverifiedVersion: 3), .discard)
    }

    func testCadenceIsOnceADayAfterAnAnsweredCheck() {
        let day: Int64 = 86_400_000
        XCTAssertTrue(translationCheckDue(lastCheckMillis: nil, nowMillis: 5, throttleMillis: day))
        XCTAssertFalse(translationCheckDue(lastCheckMillis: 1_000, nowMillis: 1_000 + day - 1, throttleMillis: day))
        XCTAssertTrue(translationCheckDue(lastCheckMillis: 1_000, nowMillis: 1_000 + day, throttleMillis: day))
        // A clock that moved backwards does not silence the check.
        XCTAssertTrue(translationCheckDue(lastCheckMillis: 10 * day, nowMillis: day, throttleMillis: day))
    }

    func testOnlyA2xxAnswerCountsAsACheck() {
        XCTAssertTrue(translationResponseCountsAsCheck(httpStatus: 200))
        XCTAssertFalse(translationResponseCountsAsCheck(httpStatus: 404))
        XCTAssertFalse(translationResponseCountsAsCheck(httpStatus: 503))
        XCTAssertFalse(translationResponseCountsAsCheck(httpStatus: nil))
    }
}

// MARK: - Updater over the real core

@MainActor
final class TranslationUpdaterTests: XCTestCase {
    private var suiteName = ""
    private var defaults: UserDefaults!
    private var store: MemoryStore!
    private var served: Data?
    private var status = 200
    private var requests: [URLRequest] = []
    private var pinned: Data? = Fixture.key

    override func setUp() {
        super.setUp()
        suiteName = "TranslationUpdaterTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
        store = MemoryStore()
        served = nil
        status = 200
        requests = []
        pinned = Fixture.key
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func spanish() -> Localization {
        Localization(defaults: defaults, bundle: .main, systemPreferred: { ["es-ES"] })
    }

    private func updater(_ l10n: Localization) -> TranslationUpdater {
        let env = TranslationUpdater.Environment(
            verifyingKey: { [unowned self] in pinned },
            bundleUrl: { URL(string: "https://lakko.test/.well-known/lakko-translations") },
            currentRelease: { 1 },
            fetch: { [unowned self] request in
                requests.append(request)
                guard let served else { throw URLError(.notConnectedToInternet) }
                let response = HTTPURLResponse(
                    url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
                return (served, response)
            },
            accept: TranslationUpdater.Environment.live.accept
        )
        return TranslationUpdater(localization: l10n, store: store, defaults: defaults, environment: env)
    }

    func testDownloadedBundleIsAppliedStoredAndThrottled() async {
        let l10n = spanish()
        let builtInRow = l10n.t("settings.about.version")
        let critical = l10n.t("auth.error.wrong_credentials")
        XCTAssertNotEqual(builtInRow, Fixture.updatedVersionRow)
        served = Fixture.bytes(Fixture.v2)

        await updater(l10n).check()

        XCTAssertEqual(l10n.t("settings.about.version"), Fixture.updatedVersionRow)
        // A security-critical key never comes from a bundle.
        XCTAssertEqual(l10n.t("auth.error.wrong_credentials"), critical)
        XCTAssertNotNil(l10n.translationsSignedAtMillis)
        XCTAssertEqual(store.stored?.version, 2)
        XCTAssertEqual(store.stored?.bytes, Fixture.bytes(Fixture.v2))
        XCTAssertEqual(store.stored?.signedAtMillis, l10n.translationsSignedAtMillis)
        XCTAssertNotNil(defaults.object(forKey: TranslationUpdater.lastCheckKey))
        // The request names nothing about the device's language.
        XCTAssertEqual(requests.count, 1)
        XCTAssertNil(requests[0].url?.query)
        XCTAssertNil(requests[0].httpBody)
        XCTAssertNil(requests[0].value(forHTTPHeaderField: "Accept-Language"))
    }

    func testSecondCheckWithinADayDoesNotFetch() async {
        let l10n = spanish()
        served = Fixture.bytes(Fixture.v2)
        let updater = updater(l10n)
        await updater.check()
        await updater.check()
        XCTAssertEqual(requests.count, 1)
    }

    func testOlderBundleNeverRollsBackANewerOne() async {
        let l10n = spanish()
        served = Fixture.bytes(Fixture.v5)
        let updater = updater(l10n)
        await updater.check()
        XCTAssertEqual(store.stored?.version, 5)

        served = Fixture.bytes(Fixture.v2)
        defaults.removeObject(forKey: TranslationUpdater.lastCheckKey)
        await updater.check()
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(store.stored?.version, 5)
        XCTAssertEqual(l10n.translationBundle?.version(), 5)
    }

    func testBundleForANewerReleaseIsIgnored() async {
        let l10n = spanish()
        served = Fixture.bytes(Fixture.v3TooNew)
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationsSignedAtMillis)
        XCTAssertNotEqual(l10n.t("settings.about.version"), Fixture.updatedVersionRow)
    }

    func testTamperedBundleIsIgnored() async {
        let l10n = spanish()
        var bytes = Fixture.bytes(Fixture.v2)
        bytes[40] ^= 0x01
        served = bytes
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationBundle)
    }

    func testNotPublishedYetRetriesNextLaunch() async {
        let l10n = spanish()
        served = Data("not found".utf8)
        status = 404
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(defaults.object(forKey: TranslationUpdater.lastCheckKey))
    }

    func testNetworkFailureKeepsCurrentState() async {
        let l10n = spanish()
        served = nil
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationBundle)
    }

    func testUnpinnedBuildIsDormantAndDropsAStoredBundle() async {
        let l10n = spanish()
        pinned = nil
        store.stored = StoredTranslationBundle(bytes: Fixture.bytes(Fixture.v2), version: 2, signedAtMillis: 1)
        served = Fixture.bytes(Fixture.v5)
        await updater(l10n).check()
        XCTAssertTrue(requests.isEmpty)
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationBundle)
    }

    func testLaunchUsesAStoredBundleThatStillVerifies() async {
        let l10n = spanish()
        store.stored = StoredTranslationBundle(bytes: Fixture.bytes(Fixture.v2), version: 2, signedAtMillis: 7)
        // Checked recently: no fetch, the stored bundle alone.
        defaults.set(NSNumber(value: Int64(Date().timeIntervalSince1970 * 1000)), forKey: TranslationUpdater.lastCheckKey)
        await updater(l10n).check()
        XCTAssertTrue(requests.isEmpty)
        XCTAssertEqual(l10n.translationBundle?.version(), 2)
        XCTAssertEqual(l10n.t("settings.about.version"), Fixture.updatedVersionRow)
    }

    func testLaunchDiscardsAStoredBundleThatNoLongerVerifies() async {
        let l10n = spanish()
        // Signed for a release this build is not: refused on re-verify.
        store.stored = StoredTranslationBundle(bytes: Fixture.bytes(Fixture.v3TooNew), version: 3, signedAtMillis: 7)
        defaults.set(NSNumber(value: Int64(Date().timeIntervalSince1970 * 1000)), forKey: TranslationUpdater.lastCheckKey)
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationBundle)
    }

    func testLaunchDiscardsAStoredBundleWhoseRecordDisagrees() async {
        let l10n = spanish()
        store.stored = StoredTranslationBundle(bytes: Fixture.bytes(Fixture.v2), version: 5, signedAtMillis: 7)
        defaults.set(NSNumber(value: Int64(Date().timeIntervalSince1970 * 1000)), forKey: TranslationUpdater.lastCheckKey)
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertNil(l10n.translationBundle)
    }

    func testFailedSaveStillAppliesForThisRun() async {
        let l10n = spanish()
        store.failSave = true
        served = Fixture.bytes(Fixture.v2)
        await updater(l10n).check()
        XCTAssertNil(store.stored)
        XCTAssertEqual(l10n.t("settings.about.version"), Fixture.updatedVersionRow)
    }

    func testFileStoreRoundTrip() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("TranslationBundleStoreTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let fileStore = FileTranslationBundleStore(dir: dir)
        XCTAssertNil(fileStore.load())
        let bundle = StoredTranslationBundle(bytes: Fixture.bytes(Fixture.v2), version: 2, signedAtMillis: 42)
        try fileStore.save(bundle)
        XCTAssertEqual(fileStore.load(), bundle)
        let excluded = try dir.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup
        XCTAssertEqual(excluded, true)
        fileStore.delete()
        XCTAssertNil(fileStore.load())
    }
}
