// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// One icon of the Lakko set (design system section 8) in a square box.
///
/// The drawing never changes per call site: one stroke width, only the box
/// changes (bands: 14 marks in a row or bubble, 20 rows / menus / composer /
/// inline buttons, 24 to 28 navigation and toolbar, 28 call controls, 40 to
/// 48 empty states). A box of about 1.19 x P matches an SF Symbol at point
/// size P, which is how each site kept its previous visual size.
///
/// With a text style (the default, `.body`) the box follows Dynamic Type the
/// way an SF Symbol in a text-style font did; `relativeTo: nil` keeps it
/// fixed, for sites whose symbol used a fixed `.system(size:)` font.
///
/// Without `label` the image is hidden from VoiceOver (an asset image would
/// otherwise be read by its file name) and the control around it carries the
/// label. An icon that stands alone as information (a sent mark, a muted mark)
/// passes `label`, which is what VoiceOver reads for it. A label added with
/// `.accessibilityLabel` after the view does not reach a hidden image.
struct LkIconView: View {
    private let icon: LkIcon
    private let size: CGFloat
    private let scales: Bool
    private let label: String?
    @ScaledMetric private var scale: CGFloat

    init(_ icon: LkIcon, size: CGFloat, relativeTo style: Font.TextStyle? = .body, label: String? = nil) {
        self.icon = icon
        self.size = size
        self.scales = style != nil
        self.label = label
        _scale = ScaledMetric(wrappedValue: 1, relativeTo: style ?? .body)
    }

    var body: some View {
        let side = scales ? size * scale : size
        let image = icon.image
            .scaledToFit()
            .frame(width: side, height: side)
        if let label {
            image.accessibilityLabel(label)
        } else {
            image.accessibilityHidden(true)
        }
    }
}

extension LkIcon {
    /// The icon at its drawn 24-point size, not resizable: for a place that
    /// takes an `Image` and lays it out itself (the tab bar).
    var fixedImage: Image {
        Image(assetName).renderingMode(.template)
    }
}

extension Label where Title == Text, Icon == LkIconView {
    /// A label with an icon of the Lakko set, in place of
    /// `Label(_:systemImage:)`. The default box (20) is the settings and menu
    /// row band.
    init(_ title: String, icon: LkIcon, size: CGFloat = 20, relativeTo style: Font.TextStyle? = .body) {
        self.init {
            Text(title)
        } icon: {
            LkIconView(icon, size: size, relativeTo: style)
        }
    }
}
