// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/group-call-grid.md` sections 3 to 6, asked through the REAL shared
/// rules over UniFFI (`lakko-ui-rules::call_group`): the grid fits its area
/// without scrolling, the order keeps this device's own tile last, and the
/// stage follows the pin and the speaker. The same list as the web
/// `group-call-layout.test.ts`.
final class GroupCallLayoutTests: XCTestCase {

    private struct Person: GroupLayoutPerson, Equatable {
        let id: String
        var isLocal = false
        var camOn = false
    }

    private let rules = SharedGroupCallRules()
    private let me = Person(id: "me", isLocal: true)

    private func person(_ id: String, camOn: Bool = false) -> Person {
        Person(id: id, camOn: camOn)
    }

    // MARK: - The grid

    func testTheGridFitsOnePageIntoItsAreaWhateverTheCount() {
        for (width, height) in [(CGFloat(366), CGFloat(560)), (760, 250), (1200, 600)] {
            for count in 1...24 {
                let g = GroupCallLayout.gridLayout(rules, count: count, width: width, height: height)
                let shown = min(UInt32(count), g.perPage)
                XCTAssertGreaterThanOrEqual(g.cols * g.rows, shown)
                XCTAssertLessThanOrEqual(
                    CGFloat(g.cols * g.tileWidth + (g.cols - 1) * g.gap), width)
                XCTAssertLessThanOrEqual(
                    CGFloat(g.rows * g.tileHeight + (g.rows - 1) * g.gap), height)
            }
        }
    }

    func testFourPeopleAreTwoByTwoOnAPhoneAndThreeAreInARowOnALaptop() {
        let phone = GroupCallLayout.gridLayout(rules, count: 4, width: 366, height: 560)
        XCTAssertEqual([phone.cols, phone.rows], [2, 2])
        let laptop = GroupCallLayout.gridLayout(rules, count: 3, width: 1200, height: 600)
        XCTAssertEqual([laptop.cols, laptop.rows], [3, 1])
    }

    func testAPhonePagesInsteadOfScrollingOnceItHoldsMoreThanSix() {
        let g = GroupCallLayout.gridLayout(rules, count: 8, width: 366, height: 560)
        XCTAssertEqual(g.perPage, 6)
        XCTAssertEqual(g.pages, 2)
    }

    /// The area is measured in fractions of a point; the rule works in whole
    /// ones, and a negative or absent area is an empty grid, not a crash.
    func testAMeasuredAreaIsTruncatedAndANonsenseAreaIsEmpty() {
        let a = GroupCallLayout.gridLayout(rules, count: 4, width: 366.9, height: 560.4)
        let b = GroupCallLayout.gridLayout(rules, count: 4, width: 366, height: 560)
        XCTAssertEqual(a, b)
        XCTAssertEqual(GroupCallLayout.gridLayout(rules, count: 4, width: -1, height: 560).cols, 0)
        XCTAssertEqual(
            GroupCallLayout.gridLayout(rules, count: 4, width: .nan, height: 560).cols, 0)
    }

    /// Every tile inside the area, none overlapping, the block centred, and a
    /// last row that is not full centred too (section 4.2).
    func testTheFramesFillTheAreaWithoutOverlapAndCentreTheLastRow() {
        let area = CGSize(width: 366, height: 560)
        for count in 1...6 {
            let g = GroupCallLayout.gridLayout(rules, count: count, width: area.width, height: area.height)
            let frames = GroupCallLayout.gridFrames(count: count, grid: g, area: area)
            XCTAssertEqual(frames.count, count)
            for (i, frame) in frames.enumerated() {
                XCTAssertEqual(frame.width, CGFloat(g.tileWidth))
                XCTAssertEqual(frame.height, CGFloat(g.tileHeight))
                XCTAssertGreaterThanOrEqual(frame.minX, -0.01, "count \(count)")
                XCTAssertGreaterThanOrEqual(frame.minY, -0.01, "count \(count)")
                XCTAssertLessThanOrEqual(frame.maxX, area.width + 0.01, "count \(count)")
                XCTAssertLessThanOrEqual(frame.maxY, area.height + 0.01, "count \(count)")
                for other in frames[(i + 1)...] {
                    XCTAssertFalse(frame.intersects(other), "count \(count)")
                }
            }
            // The block is centred both ways.
            let union = frames.reduce(CGRect.null) { $0.union($1) }
            XCTAssertEqual(union.midY, area.height / 2, accuracy: 0.51, "count \(count)")
            // Each row is centred, the last one included.
            let rows = Dictionary(grouping: frames, by: { $0.minY })
            for row in rows.values {
                let span = row.reduce(CGRect.null) { $0.union($1) }
                XCTAssertEqual(span.midX, area.width / 2, accuracy: 0.51, "count \(count)")
            }
        }
    }

    func testFivePeopleOnAPhoneLeaveOneCentredTileInTheLastRow() {
        let area = CGSize(width: 366, height: 560)
        let g = GroupCallLayout.gridLayout(rules, count: 5, width: area.width, height: area.height)
        XCTAssertEqual([g.cols, g.rows], [2, 3])
        let frames = GroupCallLayout.gridFrames(count: 5, grid: g, area: area)
        XCTAssertEqual(frames[4].midX, area.width / 2, accuracy: 0.51)
    }

    func testAnEmptyGridHasNoFrames() {
        let g = GroupCallLayout.gridLayout(rules, count: 0, width: 366, height: 560)
        XCTAssertTrue(GroupCallLayout.gridFrames(count: 0, grid: g, area: CGSize(width: 366, height: 560)).isEmpty)
        XCTAssertTrue(GroupCallLayout.gridFrames(count: 3, grid: g, area: .zero).isEmpty)
    }

    // MARK: - The order of the tiles

    func testEveryoneElseKeepsJoinOrderAndTheOwnTileIsLast() {
        let people = [me, person("a"), person("b")]
        XCTAssertEqual(GroupCallLayout.tileOrder(people).map(\.id), ["a", "b", "me"])
    }

    func testTheOwnTileStaysOnTheFirstPageWhenTheCallSpillsOver() {
        let people = [me] + ["a", "b", "c", "d", "e", "f", "g"].map { person($0) }
        let first = GroupCallLayout.gridPage(people, perPage: 6, page: 0)
        XCTAssertEqual(first.pages, 2)
        XCTAssertEqual(first.tiles.map(\.id), ["a", "b", "c", "d", "e", "me"])
        XCTAssertEqual(
            GroupCallLayout.gridPage(people, perPage: 6, page: 1).tiles.map(\.id), ["f", "g"])
    }

    func testAPageThatEmptiedFallsBackToTheLastPageThatStillHasPeople() {
        let people = [me, person("a"), person("b")]
        XCTAssertEqual(GroupCallLayout.gridPage(people, perPage: 6, page: 3).page, 0)
        let many = [me] + (0..<12).map { person("p\($0)") }
        XCTAssertEqual(GroupCallLayout.gridPage(many, perPage: 6, page: 9).page, 2)
        XCTAssertEqual(GroupCallLayout.gridPage(many, perPage: 6, page: -4).page, 0)
    }

    func testTheClientPagesAgreeWithTheSharedRulesPageCount() {
        for count in 1...30 {
            let people = [me] + (1..<count).map { person("p\($0)") }
            let g = GroupCallLayout.gridLayout(rules, count: count, width: 366, height: 560)
            let page = GroupCallLayout.gridPage(people, perPage: Int(g.perPage), page: 0)
            XCTAssertEqual(page.pages, Int(g.pages), "count \(count)")
            XCTAssertEqual(page.tiles.last?.id, "me", "the own tile closes page 1, count \(count)")
        }
    }

    // MARK: - A swipe turns the page; a tap does not

    func testAHorizontalSwipeTurnsThePage() {
        XCTAssertEqual(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: -80, height: 6), page: 0, pages: 3), 1)
        XCTAssertEqual(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: 80, height: -6), page: 1, pages: 3), 0)
    }

    func testASwipePastEitherEndStaysWhereItIs() {
        XCTAssertEqual(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: 80, height: 0), page: 0, pages: 3), 0)
        XCTAssertEqual(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: -80, height: 0), page: 2, pages: 3), 2)
    }

    /// A short move, a mostly vertical one, or any move with one page is not a
    /// page turn, so a tap on a tile still pins it and a downward swipe still
    /// puts the call away.
    func testAMoveThatIsNotAPageSwipeTurnsNothing() {
        XCTAssertNil(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: -30, height: 0), page: 0, pages: 3))
        XCTAssertNil(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: -60, height: 50), page: 0, pages: 3))
        XCTAssertNil(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: 0, height: 120), page: 0, pages: 3))
        XCTAssertNil(
            GroupCallLayout.pageAfterSwipe(
                translation: CGSize(width: -200, height: 0), page: 0, pages: 1))
    }

    // MARK: - Which layout shows

    func testItIsTheGridUntilThePersonChoosesOtherwise() {
        let people = [me, person("a"), person("b")]
        XCTAssertEqual(GroupCallLayout.kind(choice: nil, pinnedId: nil, people: people), .grid)
        XCTAssertEqual(GroupCallLayout.kind(choice: .focus, pinnedId: nil, people: people), .focus)
    }

    func testItIsFocusWhileSomeoneIsPinnedWhateverWasChosen() {
        let people = [me, person("a"), person("b")]
        XCTAssertEqual(GroupCallLayout.kind(choice: .grid, pinnedId: "a", people: people), .focus)
    }

    func testItIsTheGridWhenAloneSinceThereIsNobodyForAStrip() {
        XCTAssertEqual(GroupCallLayout.kind(choice: .focus, pinnedId: nil, people: [me]), .grid)
    }

    func testAPinOnSomeoneWhoHasLeftIsIgnored() {
        let people = [me, person("a"), person("b")]
        XCTAssertEqual(GroupCallLayout.kind(choice: nil, pinnedId: "gone", people: people), .grid)
    }

    /// This device's own tile cannot be pinned (section 5.3).
    func testAPinOnTheOwnTileIsNotAPin() {
        let people = [me, person("a")]
        XCTAssertEqual(GroupCallLayout.kind(choice: nil, pinnedId: "me", people: people), .grid)
    }

    /// The layout button shows the layout a press leads to. From Focus it
    /// returns to the grid and unpins, since a pin is what forced Focus.
    func testTheLayoutButtonSwitchesAndUnpins() {
        let toFocus = GroupCallLayout.afterLayoutSwitch(from: .grid)
        XCTAssertEqual(toFocus.choice, .focus)
        XCTAssertFalse(toFocus.clearsPin)
        let toGrid = GroupCallLayout.afterLayoutSwitch(from: .focus)
        XCTAssertEqual(toGrid.choice, .grid)
        XCTAssertTrue(toGrid.clearsPin)
    }

    // MARK: - The stage

    private var stagePeople: [Person] {
        [me, person("a"), person("b", camOn: true), person("c")]
    }

    func testTheStageHoldsThePinnedPerson() {
        XCTAssertEqual(
            GroupCallLayout.stagePerson(rules, people: stagePeople, pinnedId: "c", speech: .none)?.id,
            "c")
    }

    func testTheStageHoldsThePersonSpeakingAndKeepsThemAfterTheyStop() {
        XCTAssertEqual(
            GroupCallLayout.stagePerson(
                rules, people: stagePeople, pinnedId: nil,
                speech: StageSpeech(activeSpeakerId: "a", lastSpokeAtMs: ["a": 10]))?.id,
            "a")
        XCTAssertEqual(
            GroupCallLayout.stagePerson(
                rules, people: stagePeople, pinnedId: nil,
                speech: StageSpeech(activeSpeakerId: nil, lastSpokeAtMs: ["a": 10, "c": 5]))?.id,
            "a")
    }

    func testTheStageFallsBackToTheFirstPersonWithAPictureBeforeAnyoneHasSpoken() {
        XCTAssertEqual(
            GroupCallLayout.stagePerson(rules, people: stagePeople, pinnedId: nil, speech: .none)?.id,
            "b")
    }

    func testTheStageNeverGoesToThisDevicesOwnVoiceWhileOthersArePresent() {
        XCTAssertEqual(
            GroupCallLayout.stagePerson(
                rules, people: stagePeople, pinnedId: nil,
                speech: StageSpeech(activeSpeakerId: "me", lastSpokeAtMs: ["me": 10]))?.id,
            "b")
    }

    func testAloneTheStageIsTheOwnTileAndAnEmptyCallHasNone() {
        XCTAssertEqual(
            GroupCallLayout.stagePerson(rules, people: [me], pinnedId: nil, speech: .none)?.id, "me")
        XCTAssertNil(
            GroupCallLayout.stagePerson(rules, people: [Person](), pinnedId: nil, speech: .none))
    }

    func testEveryoneButTheStageIsInTheStripOwnTileLast() {
        let stage = GroupCallLayout.stagePerson(
            rules, people: stagePeople, pinnedId: "a", speech: .none)
        XCTAssertEqual(
            GroupCallLayout.stripPeople(stagePeople, stage: stage).map(\.id), ["b", "c", "me"])
    }

    func testTheStripIsSmallOnAPhoneAndLargerElsewhere() {
        XCTAssertEqual(
            GroupCallLayout.stripMetrics(areaWidth: 366),
            StripMetrics(tileWidth: 104, tileHeight: 78, gap: 8))
        XCTAssertEqual(
            GroupCallLayout.stripMetrics(areaWidth: 600),
            StripMetrics(tileWidth: 160, tileHeight: 90, gap: 10))
    }

    func testTheStageFitsUnderTheStrip() {
        let size = GroupCallLayout.stageSize(rules, areaWidth: 366, areaHeight: 560)
        XCTAssertLessThanOrEqual(size.width, 366)
        XCTAssertLessThanOrEqual(size.height, 560 - 78 - 8)
        XCTAssertGreaterThan(size.width, 0)
    }

    func testTheSideMarginIsTwelveOnAPhoneAndSixteenElsewhere() {
        XCTAssertEqual(GroupCallLayout.sideMargin(screenWidth: 402), 12)
        XCTAssertEqual(GroupCallLayout.sideMargin(screenWidth: 834), 16)
    }

    // MARK: - The outgoing tone

    func testTheToneNeverPlaysInAGroupCallAndPlaysInAOneToOneOnlyUntilItIsAnswered() {
        for phase in [CallBarPhase.preparing, .calling, .connecting, .connected] {
            XCTAssertFalse(rules.ringbackPlays(groupCall: true, phase: phase))
        }
        XCTAssertTrue(rules.ringbackPlays(groupCall: false, phase: .calling))
        XCTAssertFalse(rules.ringbackPlays(groupCall: false, phase: .connecting))
        XCTAssertFalse(rules.ringbackPlays(groupCall: false, phase: .connected))
    }
}

/// The layout state a call carries (`docs/ux/group-call-grid.md` sections 3
/// and 6.2): the person's choice lasts for the call, and a pin goes with the
/// person it was on.
@MainActor
final class GroupCallLayoutStateTests: XCTestCase {

    private func tile(_ id: String, isLocal: Bool = false) -> CallUiState.Participant {
        CallUiState.Participant(
            id: id, displayName: id, isLocal: isLocal,
            micOn: true, camOn: false, handRaised: false,
            quality: .good, speaking: false, screenSharing: false)
    }

    func testTheChoiceLastsForTheCallAndTheNextCallStartsFromTheDefault() {
        let state = CallUiState()
        XCTAssertNil(state.layoutChoice)
        state.setLayoutChoice(.focus)
        XCTAssertEqual(state.layoutChoice, .focus)
        state.reset()
        XCTAssertNil(state.layoutChoice)
    }

    func testIfThePinnedPersonLeavesThePinIsCleared() {
        let state = CallUiState()
        state.setParticipants([tile("me", isLocal: true), tile("a"), tile("b")])
        state.pin("a")
        XCTAssertEqual(
            GroupCallLayout.kind(
                choice: state.layoutChoice, pinnedId: state.pinnedId, people: state.participants),
            .focus)
        state.participantLeft("a")
        XCTAssertNil(state.pinnedId)
        XCTAssertEqual(
            GroupCallLayout.kind(
                choice: state.layoutChoice, pinnedId: state.pinnedId, people: state.participants),
            .grid)
    }
}
