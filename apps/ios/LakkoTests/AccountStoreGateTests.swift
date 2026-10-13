// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The test bundle's principal class. One process runs every test, and the
/// gate is process-wide: an account a test signed out or wiped would stay
/// sealed for all the tests after it. Every test starts with every account
/// open, as a new process does.
@objc(StoreGateTestReset)
final class StoreGateTestReset: NSObject, XCTestObservation {
    override init() {
        super.init()
        XCTestObservationCenter.shared.addTestObserver(self)
    }

    func testCaseWillStart(_ testCase: XCTestCase) {
        AccountStoreGate.resetForTest()
    }
}

final class AccountStoreGateTests: XCTestCase {

    private let account = "gate-test-account"

    func testAnOpenAccountIsWritten() throws {
        let hold = try XCTUnwrap(AccountStoreGate.enter(account))
        hold.leave()
        try AccountStoreGate.enterOrEnd(account).leave()
    }

    func testASealedAccountRefusesABestEffortWriteAndEndsADurableOne() {
        AccountStoreGate.seal(account)
        XCTAssertNil(AccountStoreGate.enter(account))
        XCTAssertThrowsError(try AccountStoreGate.enterOrEnd(account)) { error in
            XCTAssertTrue(error is AccountStoreGate.Sealed)
            XCTAssertTrue(Cancellation.isCancellation(error), "not reported as a failure")
        }
    }

    func testAnotherAccountIsNotSealed() {
        AccountStoreGate.seal(account)
        AccountStoreGate.enter("another-account")?.leave()
        XCTAssertNotNil(AccountStoreGate.enter("another-account").map { $0.leave() })
    }

    func testTheNextLandingOpensTheAccountAgain() {
        AccountStoreGate.seal(account, wiping: true) {}
        AccountStoreGate.open(account)
        XCTAssertFalse(AccountStoreGate.isSealed(account))
        XCTAssertNotNil(AccountStoreGate.enter(account).map { $0.leave() })
    }

    /// The spellings of one id (URL-safe, unpadded) are one account.
    func testEverySpellingOfAnIdIsTheSameAccount() {
        AccountStoreGate.seal("ab+/cd==")
        XCTAssertTrue(AccountStoreGate.isSealed("ab-_cd"))
    }

    /// The wipe's own steps run inside the seal, on the sealing thread.
    func testTheSealingThreadWritesInsideItsOwnSeal() {
        var ran = false
        AccountStoreGate.seal(account, wiping: true) {
            if let hold = AccountStoreGate.enter(account) {
                ran = true
                hold.leave()
            }
        }
        XCTAssertTrue(ran)
        XCTAssertNil(AccountStoreGate.enter(account), "and it is refused once the seal is done")
    }

    /// A sign-out refuses the ordinary writes, not what a fresh identity
    /// still owes its account; a wipe refuses that too.
    func testASignOutLetsAFreshIdentityFinishAndAWipeDoesNot() {
        AccountStoreGate.seal(account)
        XCTAssertNotNil(AccountStoreGate.enterUnlessWiped(account).map { $0.leave() })
        AccountStoreGate.seal(account, wiping: true) {}
        XCTAssertNil(AccountStoreGate.enterUnlessWiped(account))
    }

    /// A handle of the session before is refused even though the account
    /// signed in again and is open: it would write that session's state
    /// over the new one's.
    func testAHandleOfAnEarlierSessionIsRefusedAfterTheAccountSignsInAgain() throws {
        let earlier = AccountStoreGate.session(account)
        try AccountStoreGate.enterOrEnd(session: earlier).leave()

        AccountStoreGate.seal(account)
        AccountStoreGate.open(account)

        XCTAssertNotNil(AccountStoreGate.enter(account).map { $0.leave() }, "the account is open")
        XCTAssertNil(AccountStoreGate.enter(session: earlier))
        XCTAssertThrowsError(try AccountStoreGate.enterOrEnd(session: earlier))
        let current = AccountStoreGate.session(account)
        try AccountStoreGate.enterOrEnd(session: current).leave()
    }

    /// A handle made while the account was signed out (a pairing that began
    /// before the sign-in landed) belongs to the session that then opens.
    func testAHandleMadeBeforeTheLandingBelongsToTheSessionThatOpens() {
        AccountStoreGate.seal(account)
        let handle = AccountStoreGate.session(account)
        XCTAssertNil(AccountStoreGate.enter(session: handle), "sealed: nothing is written yet")
        AccountStoreGate.open(account)
        XCTAssertNotNil(AccountStoreGate.enter(session: handle).map { $0.leave() })
    }

    /// The exclusion itself: a seal waits for a write in flight on another
    /// thread, and the write that starts after it is refused.
    func testASealWaitsForAWriteInFlight() {
        let entered = expectation(description: "the writer holds the gate")
        let release = DispatchSemaphore(value: 0)
        let finished = expectation(description: "the writer finished")
        var order: [String] = []
        let orderLock = NSLock()
        func note(_ event: String) { orderLock.lock(); order.append(event); orderLock.unlock() }

        DispatchQueue.global().async {
            guard let hold = AccountStoreGate.enter(self.account) else { return }
            entered.fulfill()
            release.wait()
            note("write")
            hold.leave()
            finished.fulfill()
        }
        wait(for: [entered], timeout: 5)
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.2) { release.signal() }
        AccountStoreGate.seal(account, wiping: true) { note("wipe") }
        wait(for: [finished], timeout: 5)

        XCTAssertEqual(order, ["write", "wipe"], "the wipe ran after the write in flight")
        XCTAssertNil(AccountStoreGate.enter(account))
    }

    /// A store behind the gate: nothing is written for a sealed account.
    func testAGatedStoreWritesNothingForASealedAccount() {
        let suite = "gate-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let store = GroupNameStore(defaults: defaults)

        store.record(
            accountIdB64: account, groupIdHex: "aa",
            value: StoredGroupName(name: "Before", updatedAtMs: 1))
        XCTAssertNotNil(store.load(accountIdB64: account, groupIdHex: "aa"))

        AccountStoreGate.seal(account, wiping: true) { store.clear(accountIdB64: self.account) }
        store.record(
            accountIdB64: account, groupIdHex: "aa",
            value: StoredGroupName(name: "After", updatedAtMs: 2))
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "aa"), "the late write did not land")
    }
}
