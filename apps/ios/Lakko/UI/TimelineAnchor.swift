// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The one start date every periodic `TimelineView` in the app schedules
/// from.
///
/// `TimelineView(.periodic(from: .now, by: n))` reads `.now` every time the
/// enclosing content is evaluated, so every evaluation hands the view a NEW
/// schedule whose first entry is due at once, which re-renders it, which is
/// itself an evaluation. A periodic schedule only needs a phase, not a
/// start, so one fixed date serves every clock in the app; the first entry
/// after `from` is arithmetic, not iteration, so a distant anchor costs
/// nothing.
///
/// Found while chasing the chat screen's layout feedback loop, and kept; it
/// was not that loop's cause (a `TimelineView` inside a toolbar item was,
/// whatever its schedule; the chat screen no longer has one, see
/// `ChatScreen`'s call buttons).
enum TimelineAnchor {
    static let date = Date(timeIntervalSinceReferenceDate: 0)
}
