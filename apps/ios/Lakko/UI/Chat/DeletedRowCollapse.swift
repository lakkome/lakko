// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `docs/ux/message-actions.md` section 9 — the going of a deleted row seen
/// in front of the person: while it holds, the row keeps the height it
/// measured; when it collapses, the height animates to zero (the caller
/// drives the animation, ``DeletedRows/collapseSeconds``). A row that is not
/// held is drawn as it is.
///
/// The height is measured rather than animated from `nil`: SwiftUI cannot
/// interpolate an unconstrained frame, so the collapse would be a jump.
struct DeletedRowCollapse: ViewModifier {

    let phase: DeletedRows.Phase?

    @State private var height: CGFloat = 0

    func body(content: Content) -> some View {
        if let phase {
            content
                .fixedSize(horizontal: false, vertical: true)
                .onGeometryChange(for: CGFloat.self) { proxy in
                    proxy.size.height
                } action: { measured in
                    if phase == .holding { height = measured }
                }
                .frame(
                    height: phase == .collapsing ? 0 : (height > 0 ? height : nil),
                    alignment: .top)
                .clipped()
                .opacity(phase == .collapsing ? 0 : 1)
                .accessibilityHidden(phase == .collapsing)
        } else {
            content
        }
    }
}
