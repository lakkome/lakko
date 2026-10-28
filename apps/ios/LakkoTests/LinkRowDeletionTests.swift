// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0079 section 4 — the ledger holds ONE row per invite link, and only
/// the device that revokes the link deletes it, once.
///
/// The defect these pin (seen live on web, same shape here): the post-poll
/// sweep called ``ContactsRepository/revokeInviteLink(blobId:)`` again for
/// every link the register held as revoked, so the revoking device sent the
/// `DELETE` once at revoke and again after every poll (the ledger answered
/// 404), a poll during the revoke's own wait sent a second one concurrently,
/// and every other device of the account sent one too. These count the
/// ledger calls.
@MainActor
final class LinkRowDeletionTests: XCTestCase {

    private let account = "link-row-test-account"
    private var suiteName = ""
    private var owed: OwedLinkRowDeletionStore!
    private var deleter: LinkRowDeleter!
    private var ledger: FakeLedger!

    private let blobId = Data([1, 2, 3, 4, 5, 6])
    private let seed = Data(repeating: 7, count: 32)

    override func setUp() {
        super.setUp()
        suiteName = "link-row-deletions-\(UUID().uuidString)"
        owed = OwedLinkRowDeletionStore(defaults: UserDefaults(suiteName: suiteName)!)
        deleter = LinkRowDeleter(owed: owed)
        ledger = FakeLedger()
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    // MARK: - The revoking device

    func testRevokeThenEveryLaterSweepDeletesTheRowOnce() async throws {
        // The revoke: owed first, then the deletion.
        owed.set(accountIdB64: account, slug: slug, owed: true)
        let outcome = try await deleter.deleteOwed(
            accountIdB64: account, blobId: blobId, seed: seed, delete: ledgerDelete)
        XCTAssertEqual(outcome, .deleted(.revoked))
        XCTAssertFalse(owed.isOwed(accountIdB64: account, slug: slug))

        // Three polls later the register still holds the link as revoked and
        // this device still has its own record of it.
        for _ in 0..<3 { await sweep(ownSeed: seed) }
        XCTAssertEqual(ledger.calls.count, 1, "the row is deleted once, never again after a poll")
    }

    func testAPollDuringTheRevokeJoinsItInsteadOfSendingASecondDelete() async throws {
        owed.set(accountIdB64: account, slug: slug, owed: true)
        ledger.holdNext = true
        let revoke = Task { @MainActor in
            try await self.deleter.deleteOwed(
                accountIdB64: self.account, blobId: self.blobId, seed: self.seed, delete: self.ledgerDelete)
        }
        while !ledger.isHeld { await Task.yield() }

        // A poll lands while the revoke waits for the ledger, and the revoke
        // is pressed again.
        let poll = Task { @MainActor in await self.sweep(ownSeed: self.seed) }
        let again = Task { @MainActor in
            try await self.deleter.deleteOwed(
                accountIdB64: self.account, blobId: self.blobId, seed: self.seed, delete: self.ledgerDelete)
        }
        for _ in 0..<20 { await Task.yield() }
        ledger.release()

        let first = try await revoke.value
        let second = try await again.value
        await poll.value
        XCTAssertEqual(first, .deleted(.revoked))
        XCTAssertEqual(second, .deleted(.revoked), "the second call got the first call's answer")
        XCTAssertEqual(ledger.calls.count, 1)
    }

    func testAnUnreachableLedgerKeepsTheDeletionOwedUntilItAnswers() async throws {
        owed.set(accountIdB64: account, slug: slug, owed: true)
        ledger.answers = [
            .failure(DiscoveryClientError.network(endpoint: "/discovery/invite", underlying: URLError(.notConnectedToInternet))),
            .failure(DiscoveryClientError.network(endpoint: "/discovery/invite", underlying: URLError(.timedOut))),
            .success(.notFoundOrNotOwner),
        ]
        let outcome = try await deleter.deleteOwed(
            accountIdB64: account, blobId: blobId, seed: seed, delete: ledgerDelete)
        XCTAssertEqual(outcome, .unreachable)
        XCTAssertTrue(owed.isOwed(accountIdB64: account, slug: slug))

        await sweep(ownSeed: seed)
        XCTAssertTrue(owed.isOwed(accountIdB64: account, slug: slug))
        XCTAssertTrue(forgotten.isEmpty, "the local record stays while the deletion is owed")

        await sweep(ownSeed: seed)
        XCTAssertFalse(owed.isOwed(accountIdB64: account, slug: slug), "a 404 is an answer: done")
        XCTAssertTrue(forgotten.contains(blobId), "answered: the local traces go")

        await sweep(ownSeed: seed)
        XCTAssertEqual(ledger.calls.count, 3, "nothing is sent once the ledger answered")
    }

    func testAnOwedDeletionOutlivesTheDeleter() async throws {
        // The app stopped between the owed mark and the ledger call: a new
        // process (a new deleter over the same store) still owes it.
        owed.set(accountIdB64: account, slug: slug, owed: true)
        deleter = LinkRowDeleter(owed: OwedLinkRowDeletionStore(defaults: UserDefaults(suiteName: suiteName)!))
        await sweep(ownSeed: nil)
        XCTAssertEqual(ledger.calls.count, 1, "the register's seed deletes it")
        XCTAssertFalse(owed.isOwed(accountIdB64: account, slug: slug))
    }

    func testAnotherRefusalIsRethrownAndKeepsTheDeletionOwed() async {
        owed.set(accountIdB64: account, slug: slug, owed: true)
        ledger.answers = [.failure(DiscoveryClientError.http(status: 500, endpoint: "x", body: ""))]
        do {
            _ = try await deleter.deleteOwed(
                accountIdB64: account, blobId: blobId, seed: seed, delete: ledgerDelete)
            XCTFail("a 500 is rethrown")
        } catch {}
        XCTAssertTrue(owed.isOwed(accountIdB64: account, slug: slug))
    }

    func testANotOwedDeletionSendsNothing() async throws {
        let outcome = try await deleter.deleteOwed(
            accountIdB64: account, blobId: blobId, seed: seed, delete: ledgerDelete)
        XCTAssertEqual(outcome, .alreadyDone)
        XCTAssertTrue(ledger.calls.isEmpty)
    }

    // MARK: - Another device of the account

    func testALinkRevokedElsewhereSendsTheLedgerNothingAndDropsLocalTraces() async {
        // This device minted the link (it holds its own record and seed);
        // another device revoked it and deleted the row.
        await sweep(ownSeed: seed)
        await sweep(ownSeed: seed)
        XCTAssertTrue(ledger.calls.isEmpty)
        XCTAssertEqual(forgotten, [blobId, blobId])
        XCTAssertFalse(owed.isOwed(accountIdB64: account, slug: slug))
    }

    func testASeedlessRecordIsDeletedOnceByTheDeviceHoldingItsOwnSeed() async {
        await sweep(recordSeed: Data(), ownSeed: seed)
        await sweep(recordSeed: Data(), ownSeed: nil)
        XCTAssertEqual(ledger.calls.count, 1)
        XCTAssertEqual(ledger.seeds, [seed])
        XCTAssertFalse(owed.isOwed(accountIdB64: account, slug: slug))
    }

    func testASeedlessRecordWithoutAnOwnSeedSendsNothing() async {
        await sweep(recordSeed: Data(), ownSeed: nil)
        XCTAssertTrue(ledger.calls.isEmpty)
        XCTAssertEqual(forgotten, [blobId])
    }

    // MARK: - Forget device

    func testForgetDeviceDropsTheOwedList() {
        let defaults = UserDefaults(suiteName: suiteName)!
        owed.set(accountIdB64: account, slug: slug, owed: true)
        owed.set(accountIdB64: "other-account", slug: slug, owed: true)
        OwedLinkRowDeletionStore.clear(accountIdB64: account, defaults: defaults)
        XCTAssertTrue(owed.slugs(accountIdB64: account).isEmpty)
        XCTAssertEqual(owed.slugs(accountIdB64: "other-account"), [slug])
    }

    // MARK: - Fixtures

    private var slug: String { base64UrlEncodeNoPad(blobId) }
    private var forgotten: [Data] = []

    private var ledgerDelete: LinkRowDeleter.Delete {
        let ledger = self.ledger!
        return { blobId, seed in try await ledger.delete(blobId, seed) }
    }

    /// One post-poll sweep with the link revoked in the register.
    private func sweep(recordSeed: Data? = nil, ownSeed: Data?) async {
        await LinkRowSweep.run(
            accountIdB64: account,
            revoked: [.init(blobId: blobId, recordSeed: recordSeed ?? seed)],
            ownSeed: { _ in ownSeed },
            deleter: deleter,
            delete: ledgerDelete,
            forget: { self.forgotten.append($0) })
    }
}

/// The ledger's `DELETE /discovery/invite/{id}`, counted.
@MainActor
private final class FakeLedger {
    private(set) var calls: [Data] = []
    private(set) var seeds: [Data] = []
    var answers: [Result<RevokeInviteResult, Error>] = []
    var holdNext = false
    private var held: CheckedContinuation<Void, Never>?

    var isHeld: Bool { held != nil }

    func delete(_ blobId: Data, _ seed: Data) async throws -> RevokeInviteResult {
        calls.append(blobId)
        seeds.append(seed)
        if holdNext {
            holdNext = false
            await withCheckedContinuation { held = $0 }
        }
        return try (answers.isEmpty ? .success(.revoked) : answers.removeFirst()).get()
    }

    func release() {
        held?.resume()
        held = nil
    }
}
