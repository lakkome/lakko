// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import LakkoCore

/// How a group call's tiles are laid out (`docs/ux/group-call-grid.md`
/// sections 3 to 6).
///
/// Two layouts. **Grid**: everyone the same size, filling the space between
/// the status line and the control bar, with pages instead of a scroll.
/// **Focus**: one large tile (the stage) and everyone else in a strip above
/// it.
///
/// The geometry and the choice of who is on the stage are the shared rules of
/// `crates/lakko-ui-rules` (`call_group`), reached over UniFFI; web reaches
/// the same ones over wasm and Android over its own binding, so the three
/// clients draw the same grid on the same width. What is here is this
/// client's side of it: the order of the tiles, which page a tile is on, and
/// which layout is showing. The twin of `apps/web/src/calls/group-call-layout.ts`.
///
/// Pure: no SwiftUI. The call screen measures its area and draws the answer.

// MARK: - The shared rules

/// The shared rules this file asks. A protocol so a test can hand in the real
/// binding or a recorder alike.
protocol GroupCallRules {
    func callGrid(count: UInt32, width: UInt32, height: UInt32) -> CallGrid
    func callStage(people: [StageCandidate], pinned: UInt32?) -> UInt32?
    func ringbackPlays(groupCall: Bool, phase: CallBarPhase) -> Bool
}

/// The rules as `lakko-ui-rules` answers them. Nothing here re-decides any of
/// the three.
struct SharedGroupCallRules: GroupCallRules {
    func callGrid(count: UInt32, width: UInt32, height: UInt32) -> CallGrid {
        LakkoCore.callGrid(count: count, width: width, height: height)
    }

    func callStage(people: [StageCandidate], pinned: UInt32?) -> UInt32? {
        LakkoCore.callStage(people: people, pinned: pinned)
    }

    func ringbackPlays(groupCall: Bool, phase: CallBarPhase) -> Bool {
        LakkoCore.ringbackPlays(groupCall: groupCall, phase: phase)
    }
}

// MARK: - People

/// One person, as the layout reads them. ``CallUiState/Participant`` conforms.
protocol GroupLayoutPerson {
    var id: String { get }
    var isLocal: Bool { get }
    var camOn: Bool { get }
}

extension CallUiState.Participant: GroupLayoutPerson {}

/// Which of the two layouts is showing.
enum GroupLayoutKind: String, Equatable, Sendable {
    case grid
    case focus
}

/// What the stage rule needs beyond the roster.
struct StageSpeech: Equatable {
    /// The one person holding the floor now, or `nil`.
    var activeSpeakerId: String?
    /// When each person last held the floor, by participant id.
    var lastSpokeAtMs: [String: UInt64]

    static let none = StageSpeech(activeSpeakerId: nil, lastSpokeAtMs: [:])
}

/// One page of the grid.
struct GridPage<Person> {
    let tiles: [Person]
    let page: Int
    let pages: Int
}

/// The strip's tile size and the space between its tiles.
struct StripMetrics: Equatable {
    let tileWidth: CGFloat
    let tileHeight: CGFloat
    let gap: CGFloat
}

enum GroupCallLayout {

    // MARK: Grid

    /// Lay `count` equal tiles into an area: the shared rule's answer. The
    /// area is truncated to whole points, as the rule works in integers.
    static func gridLayout(
        _ rules: GroupCallRules, count: Int, width: CGFloat, height: CGFloat
    ) -> CallGrid {
        rules.callGrid(
            count: UInt32(clamping: max(0, count)),
            width: whole(width),
            height: whole(height))
    }

    private static func whole(_ value: CGFloat) -> UInt32 {
        guard value.isFinite, value > 0 else { return 0 }
        return UInt32(clamping: Int(value.rounded(.down)))
    }

    /// Where each tile of one grid page is drawn inside the call area: `count`
    /// tiles of the grid's size, the block centred in the area, and a last
    /// row that is not full centred too (section 4.2). One frame per tile, in
    /// tile order.
    ///
    /// Frames rather than nested rows, so every tile keeps its place in ONE
    /// list keyed by participant id: a tile is then moved when the grid
    /// changes shape, never rebuilt.
    static func gridFrames(count: Int, grid: CallGrid, area: CGSize) -> [CGRect] {
        let cols = Int(grid.cols)
        guard count > 0, cols > 0 else { return [] }
        let shown = min(count, max(Int(grid.perPage), 1))
        let tile = CGSize(width: CGFloat(grid.tileWidth), height: CGFloat(grid.tileHeight))
        let gap = CGFloat(grid.gap)
        let rows = (shown + cols - 1) / cols
        let blockHeight = CGFloat(rows) * tile.height + CGFloat(rows - 1) * gap
        let top = (area.height - blockHeight) / 2
        return (0..<shown).map { index in
            let row = index / cols
            let inRow = row == rows - 1 ? shown - row * cols : cols
            let rowWidth = CGFloat(inRow) * tile.width + CGFloat(inRow - 1) * gap
            let left = (area.width - rowWidth) / 2
            return CGRect(
                x: left + CGFloat(index % cols) * (tile.width + gap),
                y: top + CGFloat(row) * (tile.height + gap),
                width: tile.width,
                height: tile.height)
        }
    }

    /// How far a drag must travel sideways to turn the page.
    static let pageSwipeDistance: CGFloat = 48

    /// The page a finished drag leads to, or `nil` when the drag was not a
    /// page turn (section 4.5).
    ///
    /// A move that is short, or not clearly more sideways than vertical, is
    /// not a swipe; with one page there is nothing to turn. A drag towards
    /// the leading edge goes to the next page. The answer is clamped, so a
    /// swipe past either end stays where it is.
    static func pageAfterSwipe(translation: CGSize, page: Int, pages: Int) -> Int? {
        guard pages > 1 else { return nil }
        let dx = translation.width
        let dy = translation.height
        guard abs(dx) >= pageSwipeDistance, abs(dx) >= 2 * abs(dy) else { return nil }
        return min(pages - 1, max(0, page + (dx < 0 ? 1 : -1)))
    }

    // MARK: Which layout

    /// What the layout button does when pressed on `kind`.
    ///
    /// It shows the layout a press leads to, not the one on screen. Pinning
    /// someone already means Focus, so while Focus is up the button unpins
    /// and returns to the grid rather than changing a choice the pin
    /// overrides.
    static func afterLayoutSwitch(from kind: GroupLayoutKind) -> (choice: GroupLayoutKind, clearsPin: Bool) {
        switch kind {
        case .focus: return (.grid, true)
        case .grid: return (.focus, false)
        }
    }

    /// The side margin of the call area: 12 pt on a compact width, 16 pt
    /// elsewhere (section 4.3). Tiles do not touch the edge of the screen.
    static func sideMargin(screenWidth: CGFloat) -> CGFloat {
        screenWidth < 600 ? 12 : 16
    }

    /// The layout on screen.
    ///
    /// A pin always means Focus: pinning someone is asking to see them large.
    /// Otherwise the person's own choice for this call, and Grid until they
    /// make one.
    static func kind<P: GroupLayoutPerson>(
        choice: GroupLayoutKind?, pinnedId: String?, people: [P]
    ) -> GroupLayoutKind {
        // Alone, there is nobody to put in a strip.
        if !people.contains(where: { !$0.isLocal }) { return .grid }
        if let pinnedId, people.contains(where: { $0.id == pinnedId && !$0.isLocal }) {
            return .focus
        }
        return choice ?? .grid
    }

    // MARK: Order and pages

    /// The order tiles are drawn in: everyone else in the order they joined,
    /// then this device's own tile last (section 5.3: a fixed place is how a
    /// person finds themself without looking).
    static func tileOrder<P: GroupLayoutPerson>(_ people: [P]) -> [P] {
        people.filter { !$0.isLocal } + people.filter { $0.isLocal }
    }

    /// The tiles of one page of the grid.
    ///
    /// Page 0 always ends with this device's own tile, so it holds one person
    /// fewer than `perPage` when the call spills over; the later pages hold
    /// everyone else in order. `page` is clamped, so a page that emptied
    /// because someone left shows the last page that still has people.
    static func gridPage<P: GroupLayoutPerson>(
        _ people: [P], perPage: Int, page: Int
    ) -> GridPage<P> {
        let ordered = tileOrder(people)
        if perPage <= 0 || ordered.isEmpty { return GridPage(tiles: [], page: 0, pages: 0) }
        if ordered.count <= perPage { return GridPage(tiles: ordered, page: 0, pages: 1) }
        let own = ordered.filter { $0.isLocal }
        let others = ordered.filter { !$0.isLocal }
        let firstCount = max(0, perPage - own.count)
        let rest = Array(others.dropFirst(firstCount))
        let pages = 1 + (rest.count + perPage - 1) / perPage
        let at = min(max(0, page), pages - 1)
        if at == 0 {
            return GridPage(tiles: Array(others.prefix(firstCount)) + own, page: 0, pages: pages)
        }
        let from = (at - 1) * perPage
        return GridPage(
            tiles: Array(rest.dropFirst(from).prefix(perPage)), page: at, pages: pages)
    }

    // MARK: Stage and strip

    /// Who is on the stage in Focus, or `nil` for an empty call.
    ///
    /// The shared rule decides: pinned, else speaking now, else the most
    /// recent speaker, else the first person with a picture, else the first
    /// other person; this device's own tile only when alone.
    static func stagePerson<P: GroupLayoutPerson>(
        _ rules: GroupCallRules, people: [P], pinnedId: String?, speech: StageSpeech
    ) -> P? {
        if people.isEmpty { return nil }
        let candidates = people.map { person in
            StageCandidate(
                isLocal: person.isLocal,
                cameraOn: person.camOn,
                speaking: speech.activeSpeakerId == person.id,
                lastSpokeAtMs: speech.lastSpokeAtMs[person.id] ?? 0)
        }
        let pinnedIndex = pinnedId
            .flatMap { id in people.firstIndex(where: { $0.id == id }) }
            .map { UInt32(clamping: $0) }
        guard let index = rules.callStage(people: candidates, pinned: pinnedIndex) else {
            return nil
        }
        let at = Int(index)
        return people.indices.contains(at) ? people[at] : nil
    }

    /// The strip in Focus: everyone but the stage, in tile order.
    static func stripPeople<P: GroupLayoutPerson>(_ people: [P], stage: P?) -> [P] {
        tileOrder(people).filter { stage == nil || $0.id != stage?.id }
    }

    /// The strip's tile size and gap, by the width of the call area. Chosen,
    /// not measured.
    static func stripMetrics(areaWidth: CGFloat) -> StripMetrics {
        if areaWidth < 600 { return StripMetrics(tileWidth: 104, tileHeight: 78, gap: 8) }
        return StripMetrics(tileWidth: 160, tileHeight: 90, gap: 10)
    }

    /// The stage tile's size: the largest tile of an allowed shape that fits
    /// under the strip. Asked of the same shared rule as the grid, for one
    /// tile.
    static func stageSize(
        _ rules: GroupCallRules, areaWidth: CGFloat, areaHeight: CGFloat
    ) -> CGSize {
        let strip = stripMetrics(areaWidth: areaWidth)
        let height = max(0, areaHeight.rounded(.down) - strip.tileHeight - strip.gap)
        let grid = gridLayout(rules, count: 1, width: areaWidth, height: height)
        return CGSize(width: CGFloat(grid.tileWidth), height: CGFloat(grid.tileHeight))
    }
}
