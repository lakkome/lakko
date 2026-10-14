// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The SFU's ceiling on this device's camera senders (`PublishBudget.swift`;
/// ADR-0068, amendment of 2026-10-11): the frame's figures, the one mapping
/// from the frame to a sender's ceiling, and its application to a sender.
/// The same list as the web `publish-budget.test.ts`.
final class PublishBudgetRuleTests: XCTestCase {

    // MARK: - The frame

    func testTheTwoFiguresBecomeTheTwoCeilings() {
        XCTAssertEqual(
            PublishBudgetRule.read(mainKbps: 2500, middleKbps: 500),
            PublishBudget(mainKbps: 2500, middleKbps: 500))
        XCTAssertEqual(
            PublishBudgetRule.read(mainKbps: 1, middleKbps: PublishBudgetRule.maxKbps),
            PublishBudget(mainKbps: 1, middleKbps: PublishBudgetRule.maxKbps))
    }

    func testAFigureOutsideTheProtocolsBoundsIsNoBudget() {
        XCTAssertEqual(PublishBudgetRule.maxKbps, 20_000)
        XCTAssertNil(PublishBudgetRule.read(mainKbps: 0, middleKbps: 500))
        XCTAssertNil(PublishBudgetRule.read(mainKbps: 900, middleKbps: 0))
        XCTAssertNil(
            PublishBudgetRule.read(mainKbps: PublishBudgetRule.maxKbps + 1, middleKbps: 500))
        XCTAssertNil(PublishBudgetRule.read(mainKbps: 900, middleKbps: 4_000_000_000))
    }

    func testARefusedFrameThatSaysItIsAPublishBudgetOneIsRecognised() {
        XCTAssertTrue(
            PublishBudgetRule.claimsPublishBudget(#"{"kind":"publish-budget","main_kbps":0}"#))
        XCTAssertFalse(
            PublishBudgetRule.claimsPublishBudget(
                #"{"kind":"video-needed","main":true,"middle":true}"#))
        XCTAssertFalse(PublishBudgetRule.claimsPublishBudget("not json"))
        XCTAssertFalse(PublishBudgetRule.claimsPublishBudget("null"))
        XCTAssertFalse(PublishBudgetRule.claimsPublishBudget(#""publish-budget""#))
    }

    // MARK: - The mapping

    /// Before the first frame nothing is written: each sender keeps what it
    /// was created with (main none, middle 500, light 150 kbit/s).
    func testNothingIsWrittenBeforeTheFirstFrame() {
        for kind in CameraSenderKind.allCases {
            XCTAssertNil(PublishBudgetRule.senderCeiling(kind, nil))
        }
        let plan = CameraSender.plan(middleActive: true)
        XCTAssertNil(plan.first { $0.kind == .main }?.encoding.maxBitrateBps)
        XCTAssertEqual(plan.first { $0.kind == .middle }?.encoding.maxBitrateBps, 500_000)
        XCTAssertEqual(plan.first { $0.kind == .light }?.encoding.maxBitrateBps, 150_000)
    }

    func testTheCeilingIsTheFramesFigureInBitsASecond() {
        let budget = PublishBudget(mainKbps: 1200, middleKbps: 483)
        XCTAssertEqual(PublishBudgetRule.senderCeiling(.main, budget), 1_200_000)
        XCTAssertEqual(PublishBudgetRule.senderCeiling(.middle, budget), 483_000)
    }

    func testTheLightSenderIsNeverWritten() {
        XCTAssertNil(
            PublishBudgetRule.senderCeiling(.light, PublishBudget(mainKbps: 200, middleKbps: 100)))
        XCTAssertEqual(PublishBudgetRule.budgetedKinds, [.main, .middle])
    }

    /// Whatever the SFU says within the protocol's bounds is applied: no
    /// client-side clamp or share.
    func testTheServersFigureIsAppliedAsItStands() {
        XCTAssertEqual(
            PublishBudgetRule.senderCeiling(
                .middle, PublishBudget(mainKbps: 1, middleKbps: PublishBudgetRule.maxKbps)),
            Int(PublishBudgetRule.maxKbps) * 1000)
        XCTAssertEqual(
            PublishBudgetRule.senderCeiling(.main, PublishBudget(mainKbps: 1, middleKbps: 1)),
            1000)
    }

    // MARK: - The lines

    func testAChangedBudgetGetsOneLineAndARepeatedOneNone() {
        let budget = PublishBudget(mainKbps: 1200, middleKbps: 483)
        XCTAssertEqual(
            PublishBudgetRule.describeChange(before: nil, budget),
            "call publish budget main=1200 middle=483")
        XCTAssertNil(PublishBudgetRule.describeChange(before: budget, budget))
        XCTAssertEqual(
            PublishBudgetRule.describeChange(
                before: budget, PublishBudget(mainKbps: 1200, middleKbps: 300)),
            "call publish budget main=1200 middle=300")
    }

    func testTheCeilingTailIsInKilobitsOrADash() {
        XCTAssertEqual(PublishBudgetRule.describeSenderCeiling(1_200_000), " ceiling=1200")
        XCTAssertEqual(PublishBudgetRule.describeSenderCeiling(483_999), " ceiling=483")
        XCTAssertEqual(PublishBudgetRule.describeSenderCeiling(nil), " ceiling=-")
    }

    func testTheSendVideoLineEndsWithTheCeilingWhenGivenOne() {
        let now = CallMediaReading()
        // Not given: the line is as it was.
        XCTAssertEqual(
            CallMediaCounters.describeOutbound(
                "call media", now: now, existing: [.main]),
            ["call media send video mode=- size=- fps=- limited_by=- kbps=- sender=main"])
        XCTAssertEqual(
            CallMediaCounters.describeOutbound(
                "call media", now: now,
                activityOf: { _ in OutboundActivity(needed: nil, active: true) },
                existing: [.main, .light],
                ceilingOf: { $0 == .light ? 150_000 : nil }),
            [
                "call media send video mode=- size=- fps=- limited_by=- kbps=- sender=main"
                    + " needed=- active=yes ceiling=-",
                "call media send video mode=- size=- fps=- limited_by=- kbps=- sender=light"
                    + " needed=- active=yes ceiling=150",
            ])
        XCTAssertEqual(
            CallMediaCounters.describeOutbound(
                "call media", now: now,
                activityOf: { _ in OutboundActivity(needed: true, active: true) },
                existing: [.main],
                ceilingOf: { _ in 483_000 }),
            [
                "call media send video mode=- size=- fps=- limited_by=- kbps=- sender=main"
                    + " needed=yes active=yes ceiling=483"
            ])
    }

    // MARK: - One read-modify-set

    func testNothingIsSetWhenTheEncodingAlreadyHoldsTheCeiling() {
        XCTAssertEqual(
            SenderCeilingWrite.step(hasEncoding: true, current: 500_000, wanted: 500_000),
            .done(.applied))
    }

    func testASenderWithNoCeilingOrAnotherOneIsSet() {
        XCTAssertEqual(
            SenderCeilingWrite.step(hasEncoding: true, current: nil, wanted: 1_200_000),
            .set(1_200_000))
        XCTAssertEqual(
            SenderCeilingWrite.step(hasEncoding: true, current: 500_000, wanted: 483_000),
            .set(483_000))
    }

    func testASenderWithNoEncodingIsReported() {
        XCTAssertEqual(
            SenderCeilingWrite.step(hasEncoding: false, current: nil, wanted: 1000),
            .done(.noEncoding))
    }

    /// The binding's setter reports nothing: what the encoding holds when
    /// read back is the answer.
    func testAWriteThatDidNotTakeIsARefusal() {
        XCTAssertEqual(SenderCeilingWrite.outcome(readBack: 900_000, wanted: 900_000), .applied)
        XCTAssertEqual(SenderCeilingWrite.outcome(readBack: nil, wanted: 900_000), .refused)
        XCTAssertEqual(SenderCeilingWrite.outcome(readBack: 500_000, wanted: 900_000), .refused)
    }
}

/// A sender as the ceiling keeper sees it: what its encoding holds, every
/// set made, and what the next calls answer. It decides as the production
/// sender does (``SenderCeilingWrite``).
@MainActor
private final class FakeCeilingSender: CeilingSender {
    var maxBitrateBps: Int?
    var hasEncoding = true
    /// Every ceiling a set carried, in order.
    var sets: [Int] = []
    /// Every call, whether or not it set anything.
    var calls: [Int] = []
    var refuse = false
    /// Calls under way at the same time, at most.
    var maxPending = 0
    private var pending = 0
    /// Suspends the next call until released, to ask faster than it answers.
    var gate: CheckedContinuation<Void, Never>?
    var holdNext = false

    init(maxBitrateBps: Int? = nil) {
        self.maxBitrateBps = maxBitrateBps
    }

    func applyEncodingCeiling(_ wanted: Int) async -> SenderCeilingOutcome {
        calls.append(wanted)
        pending += 1
        maxPending = max(maxPending, pending)
        defer { pending -= 1 }
        switch SenderCeilingWrite.step(
            hasEncoding: hasEncoding, current: maxBitrateBps, wanted: wanted)
        {
        case let .done(outcome):
            return outcome
        case let .set(bps):
            sets.append(bps)
            if holdNext {
                holdNext = false
                await withCheckedContinuation { gate = $0 }
            }
            if !refuse { maxBitrateBps = bps }
            return SenderCeilingWrite.outcome(readBack: maxBitrateBps, wanted: bps)
        }
    }
}

@MainActor
final class SenderCeilingTests: XCTestCase {

    func testTheCeilingIsSetAndSetAgainWhenItChanges() async {
        var warnings: [String] = []
        let ceiling = SenderCeiling(name: "main", warn: { warnings.append($0) })
        let sender = FakeCeilingSender()
        ceiling.bind(sender)
        ceiling.want(1_200_000)
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 1_200_000)
        ceiling.want(683_000)
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 683_000)
        XCTAssertEqual(sender.sets, [1_200_000, 683_000])
        // Asking again for what it holds sets nothing.
        ceiling.want(683_000)
        await ceiling.settled()
        XCTAssertEqual(sender.sets.count, 2)
        XCTAssertTrue(warnings.isEmpty)
    }

    /// Before the first word nothing is written: the plan's own ceiling
    /// stands.
    func testNothingIsWrittenBeforeTheFirstWord() async {
        let ceiling = SenderCeiling(name: "middle", warn: { _ in })
        let sender = FakeCeilingSender(maxBitrateBps: CameraSender.middleMaxBitrateBps)
        ceiling.bind(sender)
        ceiling.want(nil)
        await ceiling.settled()
        XCTAssertTrue(sender.calls.isEmpty)
        XCTAssertEqual(sender.maxBitrateBps, 500_000)
        XCTAssertNil(ceiling.lastWanted)
    }

    /// Asked faster than the binding answers, the last word wins, the calls
    /// never overlap, and the word in between is never set.
    func testTheLastWordWins() async {
        let ceiling = SenderCeiling(name: "main", warn: { _ in })
        let sender = FakeCeilingSender()
        sender.holdNext = true
        ceiling.bind(sender)
        ceiling.want(2_000_000)
        // Let the first step reach the sender and park there.
        while sender.gate == nil { await Task.yield() }
        ceiling.want(1_000_000)
        ceiling.want(700_000)
        sender.gate?.resume()
        sender.gate = nil
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 700_000)
        XCTAssertEqual(sender.maxPending, 1)
        XCTAssertEqual(sender.sets, [2_000_000, 700_000])
    }

    /// A write that did not take is asked again at every later look, and
    /// said once.
    func testARefusedWriteIsAskedAgainAndSaidOnce() async {
        var warnings: [String] = []
        let ceiling = SenderCeiling(name: "main", warn: { warnings.append($0) })
        let sender = FakeCeilingSender()
        sender.refuse = true
        ceiling.bind(sender)
        ceiling.want(900_000)
        await ceiling.settled()
        XCTAssertNil(sender.maxBitrateBps)
        XCTAssertEqual(
            warnings,
            [
                "camera sender ceiling: the main camera sender's bitrate ceiling "
                    + "could not be set, asking again"
            ])
        // The next tick asks again; still refused, not said again.
        ceiling.want(900_000)
        await ceiling.settled()
        XCTAssertEqual(sender.sets.count, 2)
        XCTAssertEqual(warnings.count, 1)
        // The binding gives in.
        sender.refuse = false
        ceiling.want(900_000)
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 900_000)
    }

    /// A renegotiation reset the encoding: asking again puts the ceiling
    /// back.
    func testTheCeilingIsPutBackAfterTheBindingLostIt() async {
        let ceiling = SenderCeiling(name: "main", warn: { _ in })
        let sender = FakeCeilingSender()
        ceiling.bind(sender)
        ceiling.want(900_000)
        await ceiling.settled()
        sender.maxBitrateBps = nil
        ceiling.want(900_000)
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 900_000)
    }

    /// A sender created again gets the ceiling in force, not the one it was
    /// created with.
    func testANewSenderGetsTheCeilingInForce() async {
        var warnings: [String] = []
        let ceiling = SenderCeiling(name: "middle", warn: { warnings.append($0) })
        let first = FakeCeilingSender(maxBitrateBps: 500_000)
        ceiling.bind(first)
        ceiling.want(300_000)
        await ceiling.settled()
        XCTAssertEqual(first.maxBitrateBps, 300_000)
        // The middle sender is removed and added again with the plan's
        // ceiling.
        let second = FakeCeilingSender(maxBitrateBps: 500_000)
        ceiling.bind(second)
        ceiling.want(300_000)
        await ceiling.settled()
        XCTAssertEqual(second.maxBitrateBps, 300_000)
        XCTAssertEqual(first.sets.count, 1)
        XCTAssertTrue(warnings.isEmpty)
    }

    func testNothingIsDoneWithoutASenderAndTheLastWordIsAppliedOnceThereIsOne() async {
        let ceiling = SenderCeiling(name: "main", warn: { _ in })
        ceiling.want(800_000)
        await ceiling.settled()
        XCTAssertFalse(ceiling.bound)
        let sender = FakeCeilingSender()
        ceiling.bind(sender)
        ceiling.want(ceiling.lastWanted)
        await ceiling.settled()
        XCTAssertEqual(sender.maxBitrateBps, 800_000)
    }

    /// A sender replaced while the binding answered is not warned about: the
    /// new one is asked by the next look.
    func testAReplacedSendersRefusalIsNotSaid() async {
        var warnings: [String] = []
        let ceiling = SenderCeiling(name: "main", warn: { warnings.append($0) })
        let old = FakeCeilingSender()
        old.refuse = true
        old.holdNext = true
        ceiling.bind(old)
        ceiling.want(900_000)
        while old.gate == nil { await Task.yield() }
        let fresh = FakeCeilingSender()
        ceiling.bind(fresh)
        old.gate?.resume()
        old.gate = nil
        await ceiling.settled()
        XCTAssertTrue(warnings.isEmpty)
        XCTAssertTrue(fresh.calls.isEmpty)
        ceiling.want(900_000)
        await ceiling.settled()
        XCTAssertEqual(fresh.maxBitrateBps, 900_000)
    }

    func testEverythingIsForgottenAtTheEndOfTheCall() async {
        let ceiling = SenderCeiling(name: "main", warn: { _ in })
        let sender = FakeCeilingSender()
        ceiling.bind(sender)
        ceiling.want(900_000)
        await ceiling.settled()
        ceiling.reset()
        XCTAssertFalse(ceiling.bound)
        XCTAssertNil(ceiling.lastWanted)
        // The next call's sender is not written before that call's first
        // frame.
        let next = FakeCeilingSender()
        ceiling.bind(next)
        ceiling.want(nil)
        await ceiling.settled()
        XCTAssertTrue(next.calls.isEmpty)
    }
}
