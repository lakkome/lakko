// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// Delete account: auth first; only after it accepted, the server cleanups
// only this device can do; then the wipe, whatever the cleanups did.
@MainActor
final class AccountDeletionRunTests: XCTestCase {

    private struct Refused: Error {}
    private struct Failed: Error {}

    private func makeRun(
        events: Recorder,
        authFails: Bool = false,
        cleanups: [AccountDeletionCleanup]
    ) -> AccountDeletionRun {
        var run = AccountDeletionRun(
            deleteAtAuth: {
                events.add("auth")
                if authFails { throw Refused() }
            },
            cleanups: cleanups,
            wipe: { events.add("wipe") })
        run.retryDelay = .milliseconds(1)
        run.overallTimeout = .seconds(5)
        run.log = { _ in }
        return run
    }

    func testAuthFirstThenCleanupsThenWipe() async throws {
        let events = Recorder()
        let run = makeRun(events: events, cleanups: [
            AccountDeletionCleanup(name: "push") { events.add("push") },
            AccountDeletionCleanup(name: "backup") { events.add("backup") },
            AccountDeletionCleanup(name: "discovery") { events.add("discovery") },
        ])
        try await run.run()
        XCTAssertEqual(events.items.first, "auth")
        XCTAssertEqual(events.items.last, "wipe")
        XCTAssertEqual(Set(events.items.dropFirst().dropLast()), ["push", "backup", "discovery"])
        XCTAssertEqual(events.items.count, 5)
    }

    func testRefusedAuthDeleteRunsNoCleanupAndNoWipe() async {
        let events = Recorder()
        let run = makeRun(events: events, authFails: true, cleanups: [
            AccountDeletionCleanup(name: "push") { events.add("push") },
            AccountDeletionCleanup(name: "backup") { events.add("backup") },
        ])
        do {
            try await run.run()
            XCTFail("a refused auth delete must throw")
        } catch {
            XCTAssertTrue(error is Refused)
        }
        XCTAssertEqual(events.items, ["auth"])
    }

    func testFailingCleanupIsRetriedAndDoesNotStopTheWipe() async throws {
        let events = Recorder()
        let run = makeRun(events: events, cleanups: [
            AccountDeletionCleanup(name: "backup") {
                events.add("backup")
                throw Failed()
            },
            AccountDeletionCleanup(name: "discovery") { events.add("discovery") },
        ])
        try await run.run()
        XCTAssertEqual(events.items.filter { $0 == "backup" }.count, 3)
        XCTAssertEqual(events.items.filter { $0 == "discovery" }.count, 1)
        XCTAssertEqual(events.items.last, "wipe")
    }

    func testCleanupSucceedingOnRetryStopsRetrying() async throws {
        let events = Recorder()
        var calls = 0
        let run = makeRun(events: events, cleanups: [
            AccountDeletionCleanup(name: "push") {
                calls += 1
                events.add("push")
                if calls < 2 { throw Failed() }
            },
        ])
        try await run.run()
        XCTAssertEqual(events.items, ["auth", "push", "push", "wipe"])
    }

    func testHangingCleanupIsBoundedAndTheWipeStillRuns() async throws {
        let events = Recorder()
        var run = makeRun(events: events, cleanups: [
            AccountDeletionCleanup(name: "push") {
                try await Task.sleep(for: .seconds(60))
                events.add("push")
            },
        ])
        run.overallTimeout = .milliseconds(100)
        let started = ContinuousClock.now
        try await run.run()
        XCTAssertLessThan(ContinuousClock.now - started, .seconds(10))
        XCTAssertEqual(events.items, ["auth", "wipe"])
    }

    func testPrepareRunsBeforeTheAuthDeleteAndEvenWhenItIsRefused() async {
        let events = Recorder()
        var run = makeRun(events: events, authFails: true, cleanups: [
            AccountDeletionCleanup(name: "push") { events.add("push") },
        ])
        run.prepare = { events.add("prepare") }
        _ = try? await run.run()
        XCTAssertEqual(events.items, ["prepare", "auth"])
    }

    func testTokenTopUpHappensBeforeTheAuthDeleteWithTheRightCount() async throws {
        let events = Recorder()
        let issuer = DeletionTokenIssuer(events: events)
        let tokens = ReservedAnonTokens([])
        var pool = [Self.token(), Self.token()]
        var run = makeRun(events: events, cleanups: [])
        let subscriptions = 5
        run.prepare = {
            await tokens.fill(
                .pushRegister,
                count: ReservedAnonTokens.countForDeletion(subscriptions: subscriptions),
                takeAvailable: { pool.popLast() },
                issuer: issuer,
                sessionToken: "session")
        }
        try await run.run()
        // 5 unsubscribes + 2 spare = 7: 2 from the pool, 5 issued.
        XCTAssertEqual(events.items, ["issue 5", "auth", "wipe"])
        XCTAssertEqual(tokens.count, 7)
        XCTAssertTrue(pool.isEmpty)
    }

    func testNoSubscriptionsTakesNoToken() async {
        let events = Recorder()
        let tokens = ReservedAnonTokens([])
        var pool = [Self.token()]
        await tokens.fill(
            .pushRegister,
            count: ReservedAnonTokens.countForDeletion(subscriptions: 0),
            takeAvailable: { pool.popLast() },
            issuer: DeletionTokenIssuer(events: events),
            sessionToken: "session")
        XCTAssertEqual(tokens.count, 0)
        XCTAssertEqual(pool.count, 1)
        XCTAssertEqual(events.items, [])
    }

    func testIssuanceFailureLeavesWhatThePoolHadAndDoesNotThrow() async throws {
        let events = Recorder()
        let issuer = DeletionTokenIssuer(events: events)
        issuer.fails = true
        let tokens = ReservedAnonTokens([])
        var pool = [Self.token()]
        await tokens.fill(
            .pushRegister, count: 4, takeAvailable: { pool.popLast() },
            issuer: issuer, sessionToken: "session")
        XCTAssertEqual(tokens.count, 1)
        _ = try await tokens.take(.pushRegister)
        do {
            _ = try await tokens.take(.pushRegister)
            XCTFail("the reservation is spent")
        } catch {}
    }

    func testCountForDeletionIsCapped() {
        XCTAssertEqual(ReservedAnonTokens.countForDeletion(subscriptions: 1), 3)
        XCTAssertEqual(
            ReservedAnonTokens.countForDeletion(subscriptions: 500), ReservedAnonTokens.maxForDeletion)
    }

    static func token() -> AnonToken {
        AnonToken(context: AnonTokenContext.pushRegister.rawValue, epoch: 1, headerValue: "t")
    }
}

private final class DeletionTokenIssuer: AnonTokenIssuing, @unchecked Sendable {
    let events: Recorder
    var fails = false
    init(events: Recorder) { self.events = events }
    func issue(
        context: AnonTokenContext, epoch: UInt64, count: UInt32, sessionToken: String
    ) async throws -> [AnonToken] {
        await MainActor.run { events.add("issue \(count)") }
        if fails { throw AnonTokenUnavailable.issuanceFailed("test") }
        return (0..<Int(count)).map { _ in
            AnonToken(context: context.rawValue, epoch: epoch, headerValue: "t")
        }
    }
}

@MainActor
private final class Recorder {
    private(set) var items: [String] = []
    func add(_ item: String) { items.append(item) }
}
