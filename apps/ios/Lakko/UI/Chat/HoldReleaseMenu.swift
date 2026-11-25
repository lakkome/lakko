// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit
import UIKit.UIGestureRecognizerSubclass

/// The long-press menu of a row the list can also drag (a pinned
/// conversation, `docs/ux/navigation.md` section 2.2.1): hold and lift
/// without moving opens the menu; hold and move drags the row.
///
/// Why not ``View/lakkoLongPressMenu(_:)``. The list's own reorder lifts the
/// row on a long press, and that lift cancels every SwiftUI gesture on the
/// row, so the ordinary long press never fires on a row that can be dragged;
/// and a SwiftUI gesture that is active while the finger is down keeps the
/// lift from happening at all (both seen in `PinReorderUITests`,
/// 2026-11-25). So the row carries no gesture of its own. It only reports
/// where it is, and one watch-only UIKit recogniser on the window (it never
/// begins, so it blocks nothing, the lift included, and holds no touch back)
/// hears each touch end or be cancelled. A touch held past the long-press
/// threshold that ENDS without moving is a hold without a drag, and the
/// registered row under the finger opens its menu - only when nothing is
/// presented over the list and the finger went down inside a list cell. A
/// hold that turned into a drag is cancelled by the drag session and opens
/// nothing.
extension View {
    func lakkoHoldReleaseMenu(_ items: @escaping () -> [LakkoMenuItem]) -> some View {
        modifier(HoldReleaseMenu(items: items))
    }
}

private struct HoldReleaseMenu: ViewModifier {
    let items: () -> [LakkoMenuItem]
    @State private var shown: [LakkoMenuItem]?
    @State private var token = UUID()
    /// The menu as of the latest drawing: the row's state (muted, pinned)
    /// can change without its frame changing.
    @State private var latest = LatestItems()

    func body(content: Content) -> some View {
        let _ = (latest.items = items)
        content
            .disabled(shown != nil)
            // No SwiftUI gesture here: one that is active while the finger is
            // down keeps the list's drag from lifting the row (seen in
            // `PinReorderUITests`). The row only reports where it is.
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { frame in
                register(frame)
            }
            .onDisappear { HoldReleaseObserver.shared.unregister(token) }
            .sheet(isPresented: Binding(get: { shown != nil }, set: { if !$0 { shown = nil } })) {
                LakkoMenuSheet(items: shown ?? []) { shown = nil }
            }
    }

    private func register(_ frame: CGRect) {
        let latest = latest
        HoldReleaseObserver.shared.register(token, frame: frame) {
            let list = latest.items()
            if !list.isEmpty { shown = list }
        }
    }
}

/// A reference the menu closure reads through, so a hold opens the menu the
/// row would offer now.
@MainActor
private final class LatestItems {
    var items: () -> [LakkoMenuItem] = { [] }
}

/// The window-wide hold observer behind ``View/lakkoHoldReleaseMenu(_:)``.
@MainActor
final class HoldReleaseObserver: NSObject, UIGestureRecognizerDelegate {
    static let shared = HoldReleaseObserver()

    /// How far the finger may wander between the press and the lift and
    /// still count as "without moving" (UIKit's own long-press allowance).
    static let allowedMovement: CGFloat = 10

    private struct Target {
        var frame: CGRect
        let open: () -> Void
    }

    private var targets: [UUID: Target] = [:]
    private var observers: [ObjectIdentifier: TouchObserver] = [:]

    func register(_ token: UUID, frame: CGRect, open: @escaping () -> Void) {
        targets[token] = Target(frame: frame, open: open)
        installIfNeeded()
    }

    func unregister(_ token: UUID) {
        targets[token] = nil
    }

    /// Whether a press held for `held` seconds from `start` and lifted at
    /// `end` (window coordinates) is a hold without a drag: held past the
    /// long-press threshold, and the finger stayed within ``allowedMovement``.
    static func isHoldWithoutMove(start: CGPoint, end: CGPoint, held: TimeInterval) -> Bool {
        held >= MessageLongPress.thresholdSeconds
            && hypot(end.x - start.x, end.y - start.y) <= allowedMovement
    }

    /// The registered row a hold at `point` (window coordinates) opens: the
    /// one whose frame holds it, the smallest if frames overlap.
    static func target(at point: CGPoint, frames: [(UUID, CGRect)]) -> UUID? {
        frames
            .filter { $0.1.contains(point) }
            .min { $0.1.width * $0.1.height < $1.1.width * $1.1.height }?
            .0
    }

    private func installIfNeeded() {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
        for window in windows where observers[ObjectIdentifier(window)] == nil {
            let observer = TouchObserver { [weak self, weak window] start, end, held, startView in
                guard let window else { return }
                self?.lifted(start: start, end: end, held: held, startView: startView, window: window)
            }
            observer.delegate = self
            window.addGestureRecognizer(observer)
            observers[ObjectIdentifier(window)] = observer
        }
    }

    private func lifted(
        start: CGPoint, end: CGPoint, held: TimeInterval, startView: UIView?, window: UIWindow
    ) {
        guard Self.isHoldWithoutMove(start: start, end: end, held: held),
              // Nothing presented over the list (a sheet, a full-screen
              // cover): a hold there is not a hold on a row underneath.
              window.rootViewController?.presentedViewController == nil,
              // The finger went down inside a list cell, not on a bar or a
              // field drawn over rows that scrolled under it.
              startView.map(Self.isInsideListCell) == true,
              let token = Self.target(at: start, frames: targets.map { ($0.key, $0.value.frame) })
        else { return }
        targets[token]?.open()
    }

    private static func isInsideListCell(_ view: UIView) -> Bool {
        var current: UIView? = view
        while let v = current {
            if v is UICollectionViewCell || v is UITableViewCell { return true }
            current = v.superview
        }
        return false
    }

    func gestureRecognizer(
        _ gestureRecognizer: UIGestureRecognizer,
        shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        true
    }
}

/// A recogniser that only watches: it never begins, so it never prevents
/// another recogniser (the list's drag lift above all), and reports a single
/// touch that ENDED, with where it started, where it lifted and how long it
/// was held. A touch the system cancels (a drag session taking it) reports
/// nothing.
private final class TouchObserver: UIGestureRecognizer {
    private let onLift: (CGPoint, CGPoint, TimeInterval, UIView?) -> Void
    private var start: CGPoint?
    private var startTime: TimeInterval = 0
    private weak var startView: UIView?

    init(onLift: @escaping (CGPoint, CGPoint, TimeInterval, UIView?) -> Void) {
        self.onLift = onLift
        super.init(target: nil, action: nil)
        cancelsTouchesInView = false
        delaysTouchesBegan = false
        delaysTouchesEnded = false
    }

    override func canPrevent(_ preventedGestureRecognizer: UIGestureRecognizer) -> Bool { false }
    override func canBePrevented(by preventingGestureRecognizer: UIGestureRecognizer) -> Bool { false }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        guard start == nil, touches.count == 1, let touch = touches.first,
              (event.allTouches?.count ?? 1) == 1 else {
            start = nil
            return
        }
        start = touch.location(in: nil)
        startTime = touch.timestamp
        startView = touch.view
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
        if let start, let touch = touches.first {
            onLift(start, touch.location(in: nil), touch.timestamp - startTime, startView)
        }
        start = nil
        state = .failed
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
        start = nil
        state = .failed
    }

    override func reset() {
        start = nil
    }
}
