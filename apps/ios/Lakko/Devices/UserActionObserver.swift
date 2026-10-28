// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UIKit

/// ADR-0080 section 3: the person's touches on this device are what refreshes
/// its device card; never a timer. A tap recogniser on every window of the
/// app's scenes that cancels nothing and recognises beside every other
/// gesture, so no control behaves differently. It only tells
/// ``DeviceCardCenter`` that something happened; the center decides whether a
/// card is due (at most one every 30 minutes, or on a changed field).
@MainActor
enum UserActionObserver {

    private final class Recognizer: UITapGestureRecognizer, UIGestureRecognizerDelegate {
        init() {
            super.init(target: nil, action: nil)
            cancelsTouchesInView = false
            delaysTouchesBegan = false
            delaysTouchesEnded = false
            delegate = self
        }

        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
        ) -> Bool { true }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
            // Any touch counts, not only a completed tap (a scroll is the
            // person using the device too).
            DeviceCardCenter.shared.noteAction()
            super.touchesBegan(touches, with: event)
        }
    }

    /// Put the recogniser on every window that has none yet. Called each time
    /// the app becomes active, so a window created later is covered too.
    static func install() {
        for scene in UIApplication.shared.connectedScenes {
            guard let windowScene = scene as? UIWindowScene else { continue }
            for window in windowScene.windows
            where !(window.gestureRecognizers ?? []).contains(where: { $0 is Recognizer }) {
                window.addGestureRecognizer(Recognizer())
            }
        }
    }
}
