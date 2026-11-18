// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import UIKit

/// The two glyphs UIKit draws on Lakko's behalf, swapped for the Lakko set
/// (design system section 8): the navigation bar's back chevron and the
/// magnifier in a `.searchable` field. The system back button, its title and
/// the edge swipe stay the system's; only the image changes. List edit-mode
/// controls (delete, reorder) are left to the system.
@MainActor
enum SystemGlyphAppearance {
    /// Box of the back chevron: 28, the top of the navigation band (24 to
    /// 28). Measured on the iPhone Air simulator (iOS 26.4): the system
    /// chevron stood about 18.7 points tall; ours in a 28 box stands about 19.
    static let backBox: CGFloat = 28
    /// Box of the search field's magnifier. Measured on the same simulator:
    /// the system magnifier spans about 16.7 points; ours in a 22 box about
    /// as much (a 20 box drew it about 14 points wide).
    static let searchBox: CGFloat = 22

    private static var directionChanges: AnyCancellable?

    /// Installs both, and keeps the back chevron pointing the way the
    /// interface language reads. Call once, before the first bar is drawn.
    static func install() {
        if let search = icon(.search, box: searchBox) {
            UISearchBar.appearance().setImage(search, for: .search, state: .normal)
        }
        applyBack(rightToLeft: Localization.shared.isRightToLeft)
        directionChanges = Localization.shared.$isRightToLeft
            .removeDuplicates()
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { applyBack(rightToLeft: $0) }
    }

    /// The bar does not mirror a custom back indicator by itself: Lakko's
    /// right-to-left layout comes from the in-app language, which the bar's
    /// trait collection does not carry, so neither the asset's language
    /// direction nor `imageFlippedForRightToLeftLayoutDirection()` nor an
    /// image asset keyed on the layout direction reaches it (each tried on the
    /// iOS 26.4 simulator: the chevron kept pointing left in Arabic). The
    /// drawing is therefore chosen from the language's direction and set
    /// again on every bar already on screen when the direction changes.
    private static func applyBack(rightToLeft: Bool) {
        guard let back = backIndicator(mirrored: rightToLeft) else { return }
        let proxy = UINavigationBar.appearance()
        proxy.backIndicatorImage = back
        proxy.backIndicatorTransitionMaskImage = back
        for scene in UIApplication.shared.connectedScenes {
            guard let windowScene = scene as? UIWindowScene else { continue }
            for window in windowScene.windows { setBack(back, under: window) }
        }
    }

    private static func setBack(_ image: UIImage, under view: UIView) {
        if let bar = view as? UINavigationBar {
            bar.backIndicatorImage = image
            bar.backIndicatorTransitionMaskImage = image
        }
        for subview in view.subviews { setBack(image, under: subview) }
    }

    /// The back chevron cropped to what it draws, so the bar places it where
    /// it places its own (the drawing's empty margin would push it inwards).
    private static func backIndicator(mirrored: Bool) -> UIImage? {
        guard let source = UIImage(named: LkIcon.chevronBack.assetName) else { return nil }
        let scale = backBox / 24
        // `chevron-back` spans x 7.75...14.75 and y 4.75...19.25 on the 24
        // grid; half the 1.75 stroke on each side.
        let ink = CGRect(x: 7.75 - 0.875, y: 4.75 - 0.875, width: 7 + 1.75, height: 14.5 + 1.75)
        let size = CGSize(width: ink.width * scale, height: ink.height * scale)
        let image = UIGraphicsImageRenderer(size: size).image { context in
            if mirrored {
                context.cgContext.translateBy(x: size.width, y: 0)
                context.cgContext.scaleBy(x: -1, y: 1)
            }
            source.draw(in: CGRect(x: -ink.minX * scale, y: -ink.minY * scale, width: backBox, height: backBox))
        }
        return image.withRenderingMode(.alwaysTemplate)
    }

    /// A Lakko icon rendered at `box` points as a template image.
    private static func icon(_ icon: LkIcon, box: CGFloat) -> UIImage? {
        guard let source = UIImage(named: icon.assetName) else { return nil }
        let size = CGSize(width: box, height: box)
        let image = UIGraphicsImageRenderer(size: size).image { _ in
            source.draw(in: CGRect(origin: .zero, size: size))
        }
        return image.withRenderingMode(.alwaysTemplate)
    }
}
