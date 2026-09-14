// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The foreground receive of groups whose chat is not open: a bounded round
/// robin that skips the open chat, re-polls a group that yielded rows, backs off
/// a failing one, gives a live call's group a dedicated long poll, and stops
/// taking work while the app is backgrounded.
///
/// Every port is a fake; nothing touches the network or MLS.
@MainActor
final class ForegroundReceiveSchedulerTests: XCTestCase {

    private final class Fake {
        var groups: [String] = []
        var excluded: Set<String> = []
        var callGroup: String?
        var polls: [(group: String, waitSecs: Int)] = []
        /// Groups whose next poll reports consumed rows (consumed once).
        var yieldOnce: Set<String> = []
        var failing: Set<String> = []
        var nowMs: Int64 = 1_000
        /// When set, a poll parks until released — to observe concurrency.
        var parkPolls = false
        var parked: [CheckedContinuation<Void, Never>] = []
        var inFlight = 0
        var maxInFlight = 0
        /// Sleeps park here so the test controls the loop's pace.
        var sleepers: [CheckedContinuation<Void, Never>] = []

        func releaseParked() {
            let p = parked
            parked.removeAll()
            p.forEach { $0.resume() }
        }

        func releaseSleepers() {
            let s = sleepers
            sleepers.removeAll()
            s.forEach { $0.resume() }
        }
    }

    private func makeScheduler(
        _ fake: Fake,
        config: ForegroundReceiveScheduler.Config = .init(concurrency: 1)
    ) -> ForegroundReceiveScheduler {
        ForegroundReceiveScheduler(
            ports: .init(
                groups: { fake.groups },
                isExcluded: { fake.excluded.contains($0) },
                poll: { group, waitSecs in
                    fake.polls.append((group, waitSecs))
                    fake.inFlight += 1
                    fake.maxInFlight = max(fake.maxInFlight, fake.inFlight)
                    defer { fake.inFlight -= 1 }
                    if fake.parkPolls {
                        await withCheckedContinuation { fake.parked.append($0) }
                    } else {
                        await Task.yield()
                    }
                    if fake.failing.contains(group) {
                        throw URLError(.notConnectedToInternet)
                    }
                    return fake.yieldOnce.remove(group) != nil
                },
                callGroup: { fake.callGroup },
                sleepMs: { _ in
                    await withCheckedContinuation { fake.sleepers.append($0) }
                },
                random: { 0.5 },
                nowMs: { fake.nowMs }
            ),
            config: config
        )
    }

    private func waitUntil(
        timeout: TimeInterval = 5,
        _ condition: @MainActor () -> Bool
    ) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline {
                XCTFail("condition not met in time")
                return
            }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
    }

    // MARK: - One sweep

    func testASweepVisitsEveryGroupInOrderAndSkipsTheOpenChat() async {
        let fake = Fake()
        fake.groups = ["a", "b", "c", "d"]
        fake.excluded = ["c"]
        let scheduler = makeScheduler(fake)
        scheduler.setForeground(true)

        await scheduler.runSweep()

        XCTAssertEqual(fake.polls.map(\.group), ["a", "b", "d"], "the open chat polls itself and is skipped")
        XCTAssertEqual(Set(fake.polls.map(\.waitSecs)), [0], "background polls are short polls")
    }

    func testAGroupThatYieldedRowsIsRepolledAheadOfTheRestButCapped() async {
        let fake = Fake()
        fake.groups = ["a", "b", "c"]
        fake.yieldOnce = ["a"]
        let scheduler = makeScheduler(fake)
        scheduler.setForeground(true)

        await scheduler.runSweep()
        XCTAssertEqual(fake.polls.map(\.group), ["a", "a", "b", "c"])

        // A group that keeps yielding cannot starve the others.
        let busy = Fake()
        busy.groups = ["hot", "cold"]
        let capped = ForegroundReceiveScheduler(
            ports: .init(
                groups: { busy.groups },
                isExcluded: { _ in false },
                poll: { group, _ in
                    busy.polls.append((group, 0))
                    return group == "hot"
                },
                callGroup: { nil },
                sleepMs: { _ in },
                random: { 0.5 },
                nowMs: { 0 }
            ),
            config: .init(concurrency: 1, maxImmediateRepolls: 2)
        )
        capped.setForeground(true)
        await capped.runSweep()
        XCTAssertEqual(busy.polls.map(\.group), ["hot", "hot", "hot", "cold"])
    }

    func testConcurrencyIsBounded() async {
        let fake = Fake()
        fake.groups = ["a", "b", "c", "d", "e"]
        fake.parkPolls = true
        let scheduler = makeScheduler(fake, config: .init(concurrency: 2))
        scheduler.setForeground(true)

        let sweep = Task { @MainActor in await scheduler.runSweep() }
        await waitUntil { fake.parked.count == 2 }
        // Give any runaway worker a chance to start a third poll.
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(fake.inFlight, 2)
        while fake.polls.count < 5 || !fake.parked.isEmpty {
            fake.releaseParked()
            await Task.yield()
            if fake.polls.count == 5, fake.parked.isEmpty { break }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
        await sweep.value
        XCTAssertEqual(fake.maxInFlight, 2)
        XCTAssertEqual(Set(fake.polls.map(\.group)), ["a", "b", "c", "d", "e"])
    }

    func testAFailingGroupIsBackedOffAndASuccessClearsIt() async {
        let fake = Fake()
        fake.groups = ["a", "b"]
        fake.failing = ["a"]
        let scheduler = makeScheduler(fake)
        scheduler.setForeground(true)

        await scheduler.runSweep()
        XCTAssertEqual(fake.polls.map(\.group), ["a", "b"])

        fake.polls.removeAll()
        fake.nowMs += 1_000
        await scheduler.runSweep()
        XCTAssertEqual(fake.polls.map(\.group), ["b"], "inside its backoff the failing group is skipped")

        fake.polls.removeAll()
        fake.failing = []
        fake.nowMs += 60_000
        await scheduler.runSweep()
        XCTAssertEqual(fake.polls.map(\.group), ["a", "b"])
    }

    // MARK: - The loops

    func testTheLoopPausesInTheBackgroundAndSweepsOnReturn() async {
        let fake = Fake()
        fake.groups = ["a"]
        let scheduler = makeScheduler(fake)
        scheduler.start()
        defer {
            scheduler.stop()
            fake.releaseSleepers()
            fake.releaseParked()
        }

        for _ in 0..<50 { await Task.yield() }
        XCTAssertTrue(fake.polls.isEmpty, "nothing is polled before the app is in the foreground")

        scheduler.setForeground(true)
        await waitUntil { fake.polls.count == 1 }

        // Backgrounded while the loop sleeps between sweeps: waking it must not
        // start another sweep.
        scheduler.setForeground(false)
        fake.releaseSleepers()
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(fake.polls.count, 1, "no new poll while backgrounded")

        // Returning to the foreground sweeps at once — no interval wait.
        scheduler.setForeground(true)
        await waitUntil { fake.polls.count == 2 }
    }

    func testALiveCallsGroupGetsADedicatedLongPollAndLeavesTheRoundRobin() async {
        let fake = Fake()
        fake.groups = ["a", "call", "b"]
        fake.callGroup = "call"
        let scheduler = makeScheduler(fake)
        scheduler.setForeground(true)

        await scheduler.runSweep()
        XCTAssertEqual(fake.polls.map(\.group), ["a", "b"], "the round robin leaves the call group to its own loop")

        fake.polls.removeAll()
        fake.groups = []
        scheduler.start()
        defer {
            scheduler.stop()
            fake.releaseSleepers()
            fake.releaseParked()
        }
        await waitUntil { fake.polls.contains { $0.group == "call" } }
        XCTAssertEqual(fake.polls.first { $0.group == "call" }?.waitSecs, scheduler.config.callLongPollSecs)
    }

    func testTheCallGroupIsNotLongPolledWhenItsChatIsOpen() async {
        let fake = Fake()
        fake.groups = []
        fake.callGroup = "call"
        fake.excluded = ["call"]
        let scheduler = makeScheduler(fake)
        scheduler.setForeground(true)
        scheduler.start()
        defer {
            scheduler.stop()
            fake.releaseSleepers()
            fake.releaseParked()
        }
        await waitUntil { fake.sleepers.count >= 2 }
        XCTAssertTrue(fake.polls.isEmpty, "the open chat already long-polls the call's group")
    }

    func testThePauseIsTheIntervalPlusOrMinusTheJitter() {
        let config = ForegroundReceiveScheduler.Config()
        XCTAssertEqual(ForegroundReceiveScheduler.pauseMs(config: config, random: 0.5), 4_000)
        XCTAssertEqual(ForegroundReceiveScheduler.pauseMs(config: config, random: 0), 3_000)
        XCTAssertEqual(ForegroundReceiveScheduler.pauseMs(config: config, random: 1), 5_000)
        let tight = ForegroundReceiveScheduler.Config(intervalMs: 100, jitterMs: 500)
        XCTAssertEqual(ForegroundReceiveScheduler.pauseMs(config: tight, random: 0), 0)
    }
}

/// The chat list's unread badge, and the open-chat registry that decides both
/// what the foreground receive skips and what counts as unread.
@MainActor
final class UnreadCountingTests: XCTestCase {

    private func message(_ id: Int64) -> IncomingChatEvent {
        .application(messageId: id, text: "hi", attachments: [], senderKey: nil, createdAt: "")
    }

    func testOnlyIncomingChatMessagesCount() {
        let events: [IncomingChatEvent] = [
            message(1),
            .system(messageId: 2, systemKind: .commit, createdAt: ""),
            .callSignaling(messageId: 3, payloadBytes: Data(), createdAt: "", senderLeaf: nil),
            message(4),
        ]
        XCTAssertEqual(UnreadCounting.incomingMessages(in: events), 2)
        XCTAssertEqual(UnreadCounting.next(current: 3, applied: events, chatOpen: false), 5)
    }

    func testNothingCountsWhileTheChatIsOpen() {
        XCTAssertEqual(UnreadCounting.next(current: 3, applied: [message(1)], chatOpen: true), 0)
    }

    func testABroadcastReadCursorWinsOverTheLocalCount() {
        XCTAssertEqual(UnreadCounting.displayed(local: 4, cursorDerived: nil), 4)
        XCTAssertEqual(UnreadCounting.displayed(local: 4, cursorDerived: 1), 1)
    }

    func testTheOpenChatRegistryIsCounted() {
        let repo = ChatRepository()
        XCTAssertFalse(repo.isChatOpen(groupIdHex: "g"))
        repo.chatScreenOpened(groupIdHex: "g")
        repo.chatScreenOpened(groupIdHex: "g")
        repo.chatScreenClosed(groupIdHex: "g")
        XCTAssertTrue(repo.isChatOpen(groupIdHex: "g"), "one of two screens is still showing the chat")
        repo.chatScreenClosed(groupIdHex: "g")
        XCTAssertFalse(repo.isChatOpen(groupIdHex: "g"))
        repo.chatScreenClosed(groupIdHex: "g")
        XCTAssertFalse(repo.isChatOpen(groupIdHex: "g"), "an unmatched close is harmless")
    }

    func testAnUnbootstrappedRepositoryHasNothingToReceive() {
        let repo = ChatRepository()
        XCTAssertEqual(repo.receivableGroupIds(), [])
        XCTAssertNil(repo.journalCursor(groupIdHex: "g"))
    }
}
