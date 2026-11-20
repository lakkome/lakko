// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import LakkoCore
import SwiftUI
import UIKit

/// The QR code of a crypto donation address (`docs/ux/donations.md` section
/// 2.4): drawn by the app from the verified address, error correction H, a
/// quiet zone of 4 modules, square dark modules on a white card (white in dark
/// mode too, so a wallet scans it), and in the centre the coin mark the signed
/// `icon` names, on a white circle that clears the modules beneath it. Never
/// the Lakko logo: the device-pairing ``BrandedQr`` is not used here.
///
/// The marks are the shared artwork `apps/shared/donation-marks/*.svg`, copied
/// byte for byte into the asset catalog as vectors (`DonationMark*`).
enum DonationQr {
    static let level = "H"
    static let quietModules = 4
    /// The white circle under the mark, as a fraction of the symbol's side
    /// (modules without the quiet zone). Every module it touches is left out.
    static let clearFraction: CGFloat = 0.26
    /// The mark's diameter, as a fraction of the symbol's side.
    static let markFraction: CGFloat = 0.22
    static let cardRadius: CGFloat = 12

    /// The card is white in either theme.
    static let cardColor = UIColor.white
    /// The app's text colour as it is on a light surface (`LakkoText`, light
    /// appearance): the card is always light, so the dark-mode text colour
    /// would not scan.
    static let ink: UIColor = {
        let light = UITraitCollection(userInterfaceStyle: .light)
        return UIColor(named: "LakkoText")?.resolvedColor(with: light)
            ?? UIColor(red: 0x1A / 255, green: 0x1D / 255, blue: 0x2B / 255, alpha: 1)
    }()

    struct Plan: Equatable {
        /// Modules per side, without the quiet zone.
        let size: Int
        /// Row-major, `true` = a dark module that is drawn.
        let dark: [Bool]
        /// Row-major, `true` = left out under the centre mark.
        let cleared: [Bool]
        /// The asset of the centre mark, or nil for a plain code.
        let markAsset: String?
    }

    /// The asset catalog name of the mark for `icon`, nil for `.none`.
    static func markAsset(_ icon: DonationIcon) -> String? {
        switch icon {
        case .none: return nil
        case .btc: return "DonationMarkBtc"
        case .eth: return "DonationMarkEth"
        case .ton: return "DonationMarkTon"
        }
    }

    /// Encode `address` at level H; with a mark, the modules the centre circle
    /// touches are cleared. Nil when the text does not fit a QR code.
    static func plan(address: String, icon: DonationIcon) -> Plan? {
        guard let matrix = BrandedQr.matrix(text: address, level: level) else { return nil }
        let n = matrix.size
        let asset = markAsset(icon)
        var cleared = [Bool](repeating: false, count: n * n)
        if asset != nil {
            let c = CGFloat(n) / 2
            let r = clearFraction * CGFloat(n) / 2
            for y in 0..<n {
                for x in 0..<n {
                    // The point of the module's square nearest the centre.
                    let nx = min(max(c, CGFloat(x)), CGFloat(x + 1))
                    let ny = min(max(c, CGFloat(y)), CGFloat(y + 1))
                    let dx = nx - c, dy = ny - c
                    if dx * dx + dy * dy < r * r { cleared[y * n + x] = true }
                }
            }
        }
        let dark = zip(matrix.dark, cleared).map { $0 && !$1 }
        return Plan(size: n, dark: dark, cleared: cleared, markAsset: asset)
    }

    /// Draw `plan` into `context` filling a square of side `side`.
    static func draw(_ plan: Plan, side: CGFloat, in context: GraphicsContext) {
        let n = plan.size
        let m = side / CGFloat(n + 2 * quietModules)
        context.fill(
            Path(roundedRect: CGRect(x: 0, y: 0, width: side, height: side), cornerRadius: cardRadius),
            with: .color(Color(cardColor)))
        var modules = Path()
        for y in 0..<n {
            for x in 0..<n where plan.dark[y * n + x] {
                modules.addRect(CGRect(
                    x: CGFloat(x + quietModules) * m, y: CGFloat(y + quietModules) * m,
                    width: m, height: m))
            }
        }
        context.fill(modules, with: .color(Color(ink)))
        if let asset = plan.markAsset {
            let mid = side / 2
            let clear = clearFraction * CGFloat(n) * m
            context.fill(
                Path(ellipseIn: CGRect(x: mid - clear / 2, y: mid - clear / 2, width: clear, height: clear)),
                with: .color(Color(cardColor)))
            let mark = markFraction * CGFloat(n) * m
            context.draw(
                Image(asset).resizable(),
                in: CGRect(x: mid - mark / 2, y: mid - mark / 2, width: mark, height: mark))
        }
    }
}

/// A crypto address's QR code, as wide as the card and square.
struct DonationQrView: View {
    let address: String
    let icon: DonationIcon
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let plan = DonationQr.plan(address: address, icon: icon) {
            Canvas { context, size in
                DonationQr.draw(plan, side: min(size.width, size.height), in: context)
            }
            .aspectRatio(1, contentMode: .fit)
            .frame(maxWidth: .infinity)
            .accessibilityElement()
            .accessibilityLabel(l10n.t("common.qr_code"))
            .accessibilityAddTraits(.isImage)
        } else {
            Text(l10n.t("common.error.qr_render"))
                .foregroundStyle(Color.lakkoDanger)
                .font(.footnote)
        }
    }
}
