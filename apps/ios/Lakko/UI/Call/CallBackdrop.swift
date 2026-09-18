// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The ground a voice call is drawn on (`docs/ux/calls.md` section 4.2).
///
/// A flat surface reads as an empty page, which is exactly what made the old
/// screen look unfinished. Signal and WhatsApp blur the contact's photo behind
/// the portrait; Telegram builds a gradient out of the contact's colour. Lakko
/// has no photos yet, so this is the second of those: the person's own initials
/// tint, heavily blurred and darkened until it is a ground rather than a
/// picture. When a profile photo exists it belongs here, blurred the same way.
///
/// Deliberately static. An animated gradient that keeps moving for the whole
/// call is decoration with a battery cost (section 4.2).
struct CallBackdrop: View {
    /// What the tint is derived from — the same identity string the avatar
    /// hashes, so a call and a chat row show the same person in the same
    /// colour.
    let identity: String

    var body: some View {
        let tint = InitialsAvatar.tint(for: identity)
        ZStack {
            // The base is the call surface, so a tint that fails to resolve
            // still leaves a dark, legible screen rather than white.
            Color.lakkoBackground
            GeometryReader { geo in
                let side = max(geo.size.width, geo.size.height)
                Circle()
                    .fill(tint.foreground)
                    .frame(width: side * 0.9, height: side * 0.9)
                    .blur(radius: side * 0.35)
                    .offset(x: -side * 0.15, y: -side * 0.25)
                Circle()
                    .fill(tint.background)
                    .frame(width: side * 0.7, height: side * 0.7)
                    .blur(radius: side * 0.3)
                    .offset(x: side * 0.45, y: side * 0.5)
            }
            // Darkened, so white type and the controls read over it at any
            // tint. This is the "and darkened" half of the rule.
            Color.black.opacity(0.55)
        }
        .ignoresSafeArea()
        .accessibilityHidden(true)
    }
}
