// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation

/// Which call tile shows its controls after a tap
/// (`docs/ux/group-call-grid.md` section 6.2); the twin of
/// `apps/web/src/calls/tile-controls-reveal.ts`.
///
/// A touch screen has no hover, so the pin button of a tile is brought up by
/// a tap on the tile. That tap does nothing else: it never pins. The controls
/// of one tile at a time stay up for a few seconds, and go when the time runs
/// out or the person taps anywhere else.
///
/// Pure: the timer is handed in, so the whole rule runs in a test.
final class TileControlsReveal {

    /// How long a tile's controls stay up after the tap. Chosen, not
    /// measured.
    static let revealMs: Int64 = 3_000

    private let onChange: (String?) -> Void
    private let deps: CallTimerDeps
    private var shown: String?
    private var cancelTimer: (() -> Void)?

    init(deps: CallTimerDeps = .main, onChange: @escaping (String?) -> Void) {
        self.deps = deps
        self.onChange = onChange
    }

    /// The tile whose controls are up, or `nil`.
    var tileId: String? { shown }

    /// A tap on a tile: its controls come up, and the time starts again.
    func tap(_ tileId: String) {
        stopTimer()
        cancelTimer = deps.schedule(Self.revealMs) { [weak self] in
            guard let self else { return }
            self.cancelTimer = nil
            self.set(nil)
        }
        set(tileId)
    }

    /// A press began somewhere: on the tile with this id, or on no tile
    /// (`nil`). Anywhere but the tile whose controls are up hides them.
    func pressBegan(_ tileId: String?) {
        if shown != nil, tileId != shown { hide() }
    }

    /// Hide the controls now.
    func hide() {
        stopTimer()
        set(nil)
    }

    /// Stop the timer; nothing is reported any more.
    func dispose() {
        stopTimer()
        shown = nil
    }

    private func stopTimer() {
        cancelTimer?()
        cancelTimer = nil
    }

    private func set(_ tileId: String?) {
        if shown == tileId { return }
        shown = tileId
        onChange(tileId)
    }
}

/// Whether a tile carries a pin control, and which
/// (`docs/ux/group-call-grid.md` section 6.2). A tile is never itself the
/// control: pinning and unpinning happen only through what this names.
enum TilePinControl: Equatable {
    /// Nothing: this device's own tile, or another person's tile whose
    /// controls are not up.
    case none
    /// The round pin button in the top right corner, up after a tap on the
    /// tile. A press pins.
    case pinButton
    /// The persistent `Pinned` chip of the pinned tile. A press unpins.
    case pinnedChip

    /// - Parameters:
    ///   - isLocal: this device's own tile, which has no pin control: it is
    ///     never on the stage while anyone else is present.
    ///   - isPinned: the tile is the pinned person's.
    ///   - revealed: the tile's controls are up (``TileControlsReveal``).
    static func of(isLocal: Bool, isPinned: Bool, revealed: Bool) -> TilePinControl {
        if isLocal { return .none }
        if isPinned { return .pinnedChip }
        return revealed ? .pinButton : .none
    }

    /// The side of the control's touch target, points (section 6.2: 44 pt).
    static let touchTarget: CGFloat = 44
}

/// Which tile a press inside the call area began on
/// (`docs/ux/group-call-grid.md` section 6.2). The call screen watches every
/// press in the call area, without taking it from whatever it lands on, and
/// hands the answer to ``TileControlsReveal/pressBegan(_:)``: a press on the
/// tile whose controls are up (the pin control lies inside its tile) keeps
/// them, a press anywhere else (another tile, the status line, the control
/// bar, empty space in the grid) hides them at once.
enum TilePressTarget {

    /// The id of the tile whose frame holds `point`, or `nil` when the press
    /// began beside every tile. `frames` and `point` are in one coordinate
    /// space. Tiles do not overlap; were two to, the lowest id is named so
    /// the answer does not depend on dictionary order.
    static func tileId(at point: CGPoint, frames: [String: CGRect]) -> String? {
        frames.filter { $0.value.contains(point) }.keys.min()
    }
}
