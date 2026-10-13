// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// "Three words from your phrase": the row-visibility rule, the local guess
/// budget and its store, and the core's check driven for real.
final class ThreeWordsTests: XCTestCase {

    private let t0: Int64 = 1_700_000_000_000

    // MARK: - Row visibility

    func testTheRowNeedsACachedUsernameThePhraseAndTheKeys() {
        XCTAssertTrue(ThreeWords.offers(cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true, suspended: false, phraseSaved: true))
        // No cached account, or a guest (no username to address the sign-in by).
        XCTAssertFalse(ThreeWords.offers(cachedUsername: nil, phraseOnDevice: true, keysOnDevice: true, suspended: false, phraseSaved: true))
        XCTAssertFalse(ThreeWords.offers(cachedUsername: "", phraseOnDevice: true, keysOnDevice: true, suspended: false, phraseSaved: true))
        XCTAssertFalse(ThreeWords.offers(cachedUsername: "  ", phraseOnDevice: true, keysOnDevice: true, suspended: false, phraseSaved: true))
        // No phrase on the device: nothing to check against.
        XCTAssertFalse(ThreeWords.offers(cachedUsername: "alice", phraseOnDevice: false, keysOnDevice: true, suspended: false, phraseSaved: true))
        // No keys: the sign-in would only reach the restore prompt.
        XCTAssertFalse(ThreeWords.offers(cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: false, suspended: false, phraseSaved: true))
    }

    // MARK: - Budget

    func testFourMissesLeaveTheRouteOpenAndTheFifthPausesItFor15Minutes() {
        var budget = ThreeWords.Budget.fresh
        XCTAssertEqual(ThreeWords.triesLeft(budget, at: t0), 5)
        for miss in 1...4 {
            budget = ThreeWords.afterMiss(budget, at: t0)
            XCTAssertEqual(budget.misses, miss)
            XCTAssertNil(budget.pausedUntilMs)
            XCTAssertEqual(ThreeWords.triesLeft(budget, at: t0), 5 - miss)
            XCTAssertEqual(ThreeWords.pauseRemainingMs(budget, at: t0), 0)
        }
        budget = ThreeWords.afterMiss(budget, at: t0)
        XCTAssertEqual(budget.pausedUntilMs, t0 + 15 * 60 * 1000)
        XCTAssertEqual(ThreeWords.triesLeft(budget, at: t0), 0)
        XCTAssertEqual(ThreeWords.pauseRemainingMs(budget, at: t0), 15 * 60 * 1000)
        XCTAssertEqual(ThreeWords.pauseRemainingMs(budget, at: t0 + 60_000), 14 * 60 * 1000)
    }

    func testTheCountRestartsAtZeroOnceThePauseIsOver() {
        var budget = ThreeWords.Budget.fresh
        for _ in 1...5 { budget = ThreeWords.afterMiss(budget, at: t0) }
        let after = t0 + ThreeWords.pauseMs
        XCTAssertEqual(ThreeWords.budget(budget, at: after), .fresh)
        XCTAssertEqual(ThreeWords.triesLeft(budget, at: after), 5)
        XCTAssertEqual(ThreeWords.pauseRemainingMs(budget, at: after), 0)
        // The next miss is the first of a new run.
        XCTAssertEqual(ThreeWords.afterMiss(budget, at: after), ThreeWords.Budget(misses: 1, pausedUntilMs: nil))
    }

    func testAMissWhilePausedChangesNothing() {
        var budget = ThreeWords.Budget.fresh
        for _ in 1...5 { budget = ThreeWords.afterMiss(budget, at: t0) }
        XCTAssertEqual(ThreeWords.afterMiss(budget, at: t0 + 1_000), budget)
    }

    func testAClockSetBackNeverYieldsMoreThanAFullPause() {
        let budget = ThreeWords.Budget(misses: 5, pausedUntilMs: t0 + ThreeWords.pauseMs)
        // The clock went back a day: the wait is still at most 15 minutes.
        XCTAssertEqual(ThreeWords.pauseRemainingMs(budget, at: t0 - 86_400_000), ThreeWords.pauseMs)
    }

    func testTheCountdownReadsMinutesAndSecondsRoundedUp() {
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: 15 * 60 * 1000), "15:00")
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: 899_001), "15:00")
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: 61_000), "1:01")
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: 1), "0:01")
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: 0), "0:00")
        XCTAssertEqual(ThreeWords.formatCountdown(remainingMs: -5), "0:00")
    }

    // MARK: - Budget store

    private func store() throws -> (ThreeWordsBudgetStore, UserDefaults) {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "three-words-\(UUID().uuidString)"))
        return (ThreeWordsBudgetStore(defaults: defaults), defaults)
    }

    func testTheBudgetIsKeptPerAccountAndSurvivesANewStore() throws {
        let (store, defaults) = try store()
        let paused = ThreeWords.Budget(misses: 5, pausedUntilMs: t0 + 1)
        store.save(ThreeWords.Budget(misses: 2, pausedUntilMs: nil), accountIdB64: "A")
        store.save(paused, accountIdB64: "B")
        // A second store over the same defaults: what a relaunch reads.
        let relaunched = ThreeWordsBudgetStore(defaults: defaults)
        XCTAssertEqual(relaunched.load(accountIdB64: "A"), ThreeWords.Budget(misses: 2, pausedUntilMs: nil))
        XCTAssertEqual(relaunched.load(accountIdB64: "B"), paused)
        XCTAssertEqual(relaunched.load(accountIdB64: "C"), .fresh)
    }

    func testClearDropsOneAccountOnly() throws {
        let (store, _) = try store()
        store.save(ThreeWords.Budget(misses: 3, pausedUntilMs: nil), accountIdB64: "A")
        store.save(ThreeWords.Budget(misses: 4, pausedUntilMs: nil), accountIdB64: "B")
        store.clear(accountIdB64: "A")
        XCTAssertEqual(store.load(accountIdB64: "A"), .fresh)
        XCTAssertEqual(store.load(accountIdB64: "B").misses, 4)
    }

    func testAMalformedRowReadsAsFreshAndAnOversizedCountIsClamped() throws {
        let (store, defaults) = try store()
        defaults.set(Data("not json".utf8), forKey: ThreeWordsBudgetStore.key(accountIdB64: "A"))
        XCTAssertEqual(store.load(accountIdB64: "A"), .fresh)
        defaults.set(Data(#"{"misses":-1}"#.utf8), forKey: ThreeWordsBudgetStore.key(accountIdB64: "B"))
        XCTAssertEqual(store.load(accountIdB64: "B"), .fresh)
        defaults.set(Data(#"{"misses":99}"#.utf8), forKey: ThreeWordsBudgetStore.key(accountIdB64: "C"))
        XCTAssertEqual(store.load(accountIdB64: "C").misses, ThreeWords.maxMisses)
    }

    @MainActor
    func testForgetDeviceClearsTheBudgetAndTheRecoveryMarkers() {
        let accountIdB64 = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
        let budgets = ThreeWordsBudgetStore()
        let markers = RecoveryMarkers()
        budgets.save(ThreeWords.Budget(misses: 3, pausedUntilMs: nil), accountIdB64: accountIdB64)
        markers.markPublishOwed(
            KeyResetPublishRecord(
                attestation: Data(repeating: 0xA7, count: 177), keyGeneration: 2, signedAtMs: 1,
                newSignaturePubkey: Data(repeating: 0x0A, count: 32)),
            accountIdB64: accountIdB64)
        markers.markPhraseOwed(accountIdB64: accountIdB64)
        UserDefaults.standard.set(true, forKey: ThreeWordsBudgetStore.suspendedKey(accountIdB64: accountIdB64))

        AccountLocalDataWipe.wipe(accountIdB64: accountIdB64, removeDeliveredNotifications: {})

        XCTAssertEqual(budgets.load(accountIdB64: accountIdB64), .fresh)
        XCTAssertFalse(budgets.isSuspended(accountIdB64: accountIdB64))
        XCTAssertFalse(markers.isPublishOwed(accountIdB64: accountIdB64))
        XCTAssertFalse(markers.isPhraseOwed(accountIdB64: accountIdB64))
    }

    // MARK: - Refused sign-in rule

    func testOnlyARefusedCredentialSuspendsTheRoute() {
        // ADR-0073: the recovery credential moves with the keys in one
        // server transaction, so a refused credential is stale, never
        // pending.
        XCTAssertEqual(ThreeWords.stalePhraseVerdict(failure: .invalidCredentials), .suspendThreeWords)
        XCTAssertEqual(ThreeWords.stalePhraseVerdict(failure: .other), .keep)
    }

    func testOnlyThe401OfTheRecoveryLoginIsARefusedCredential() {
        func http(_ status: Int, _ endpoint: String) -> AuthClientError {
            .http(status: status, endpoint: endpoint, body: "")
        }
        XCTAssertEqual(ThreeWords.signInFailure(http(401, "/recovery/login/finish")), .invalidCredentials)
        XCTAssertEqual(ThreeWords.signInFailure(http(401, "/recovery/login/start")), .invalidCredentials)
        for status in [400, 404, 429, 500, 503] {
            XCTAssertEqual(ThreeWords.signInFailure(http(status, "/recovery/login/finish")), .other, "\(status)")
        }
        // A 401 from another route (the handle bind, say) says nothing
        // about the phrase.
        XCTAssertEqual(ThreeWords.signInFailure(http(401, "/auth/login-handle")), .other)
        XCTAssertEqual(
            ThreeWords.signInFailure(
                AuthClientError.network(endpoint: "/recovery/login/start", underlying: URLError(.notConnectedToInternet))),
            .other)
        XCTAssertEqual(ThreeWords.signInFailure(ThreeWordsError.rejected), .other)
        XCTAssertEqual(ThreeWords.signInFailure(ThreeWordsError.unavailable), .other)
    }

    func testASuspendedRouteIsNotOffered() {
        XCTAssertTrue(ThreeWords.offers(
            cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true, suspended: false, phraseSaved: true))
        XCTAssertFalse(ThreeWords.offers(
            cachedUsername: "alice", phraseOnDevice: true, keysOnDevice: true, suspended: true, phraseSaved: true))
    }

    func testSuspendingClearsTheBudgetAndTheMarkerIsPerAccountAndSurvivesARelaunch() throws {
        let (store, defaults) = try store()
        store.save(ThreeWords.Budget(misses: 3, pausedUntilMs: nil), accountIdB64: "A")
        store.save(ThreeWords.Budget(misses: 2, pausedUntilMs: nil), accountIdB64: "B")
        XCTAssertFalse(store.isSuspended(accountIdB64: "A"))
        store.suspend(accountIdB64: "A")
        XCTAssertEqual(store.load(accountIdB64: "A"), .fresh)
        XCTAssertTrue(ThreeWordsBudgetStore(defaults: defaults).isSuspended(accountIdB64: "A"))
        XCTAssertFalse(store.isSuspended(accountIdB64: "B"))
        XCTAssertEqual(store.load(accountIdB64: "B").misses, 2)
        // Clearing the budget (a success) does not lift the marker.
        store.clear(accountIdB64: "A")
        XCTAssertTrue(store.isSuspended(accountIdB64: "A"))
        store.clearAll(accountIdB64: "A")
        XCTAssertFalse(store.isSuspended(accountIdB64: "A"))
    }

    func testWritingThePhraseStoreAgainLiftsTheSuspension() throws {
        let (store, _) = try store()
        let accountIdB64 = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))
        defer { try? PhraseStore().deletePhrase(accountIdB64: accountIdB64) }
        store.suspend(accountIdB64: accountIdB64)
        // A phrase the core refuses writes nothing and lifts nothing.
        DevicePhrase.store("not a phrase", accountIdB64: accountIdB64, context: "test", threeWords: store)
        XCTAssertTrue(store.isSuspended(accountIdB64: accountIdB64))
        DevicePhrase.store(
            RecoveryMnemonic.generate().phrase(), accountIdB64: accountIdB64, context: "test", threeWords: store)
        XCTAssertFalse(store.isSuspended(accountIdB64: accountIdB64))
    }

    func testTheSettingsRowSaysNoPhraseThenPausedThenWorking() {
        XCTAssertEqual(
            ThreeWords.settingsLineKey(phraseSaved: false, suspended: false),
            "settings.recovery.words.not_on_this_device")
        XCTAssertEqual(
            ThreeWords.settingsLineKey(phraseSaved: false, suspended: true),
            "settings.recovery.words.not_on_this_device")
        XCTAssertEqual(
            ThreeWords.settingsLineKey(phraseSaved: true, suspended: true),
            "settings.recovery.words.suspended")
        XCTAssertEqual(
            ThreeWords.settingsLineKey(phraseSaved: true, suspended: false),
            "settings.recovery.words.on_this_device")
    }

    // MARK: - Show recovery phrase rule

    func testThePhraseIsShownOnlyOnceNoReplaceIsOwed() {
        XCTAssertEqual(PhraseReveal.decide(replaceOwed: true, phraseStored: true), .withheldCredentialPending)
        XCTAssertEqual(PhraseReveal.decide(replaceOwed: true, phraseStored: false), .withheldCredentialPending)
        XCTAssertEqual(PhraseReveal.decide(replaceOwed: false, phraseStored: true), .show)
        XCTAssertEqual(PhraseReveal.decide(replaceOwed: false, phraseStored: false), .notStored)
    }

    // MARK: - The core's check

    func testThePositionsAreThreeDistinctWordNumbersInTheOrderDrawn() {
        var sawUnsorted = false
        for _ in 0..<200 {
            let positions = phraseCheckRandomPositions().map { Int($0) }
            XCTAssertEqual(positions.count, 3)
            XCTAssertEqual(Set(positions).count, 3, "distinct")
            XCTAssertTrue(positions.allSatisfy { (1...12).contains($0) })
            if positions != positions.sorted() { sawUnsorted = true }
        }
        XCTAssertTrue(sawUnsorted, "the positions come in the order drawn, not sorted")
    }

    func testTheRightWordsAtTheDrawnPositionsMatchAndAnyWrongWordDoesNot() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let words = mnemonic.words()
        let positions = Data([7, 3, 12])
        let right = [words[6], words[2], words[11]]
        XCTAssertTrue(try phraseCheckWords(mnemonic: mnemonic, positions: positions, words: right))
        // The core trims and lower-cases.
        XCTAssertTrue(try phraseCheckWords(
            mnemonic: mnemonic, positions: positions,
            words: right.map { "  \($0.uppercased()) " }))
        // Right words in sorted order answer the wrong positions.
        if Set(right).count == 3 {
            XCTAssertFalse(try phraseCheckWords(
                mnemonic: mnemonic, positions: positions, words: [words[2], words[6], words[11]]))
        }
        XCTAssertFalse(try phraseCheckWords(
            mnemonic: mnemonic, positions: positions, words: [right[0], right[1], "notaword"]))
    }

    func testMalformedInputIsAnErrorNotAMiss() {
        let mnemonic = RecoveryMnemonic.generate()
        let words = Array(mnemonic.words().prefix(3))
        for positions in [Data([1, 2]), Data([1, 1, 2]), Data([0, 1, 2]), Data([1, 2, 13])] {
            XCTAssertThrowsError(
                try phraseCheckWords(mnemonic: mnemonic, positions: positions, words: words)
            ) { error in
                guard case .PhraseCheck = error as? FfiError else {
                    return XCTFail("expected PhraseCheck, got \(error)")
                }
            }
        }
        XCTAssertThrowsError(
            try phraseCheckWords(mnemonic: mnemonic, positions: Data([1, 2, 3]), words: ["a", "b"]))
    }

    // MARK: - The word list the form validates against

    func testEveryWordTheCoreGeneratesIsOnTheList() {
        XCTAssertEqual(Bip39Words.count, 2048)
        for _ in 0..<64 {
            for word in RecoveryMnemonic.generate().words() {
                XCTAssertTrue(Bip39Words.contains(word), word)
            }
        }
        XCTAssertTrue(Bip39Words.contains("  ZOO "))
        XCTAssertFalse(Bip39Words.contains("zoology"))
        XCTAssertFalse(Bip39Words.contains(""))
    }
}
