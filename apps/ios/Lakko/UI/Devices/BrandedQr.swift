// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import CoreImage.CIFilterBuiltins
import Foundation
import SwiftUI
import UIKit

/// The branded QR code (design system section 3, "QR code"), the same rules
/// as the web `ui/branded-qr.ts` and Android `ui/devices/BrandedQr.kt`:
/// CoreImage encodes, the app draws the modules itself.
///
/// The encoded text is never changed by the styling; only the error
/// correction level depends on whether a logo is drawn.
enum BrandedQr {
    /// A logo is drawn only for a text of at most this many UTF-8 bytes: it
    /// then fits QR version 15 at level H in byte mode on every encoder.
    static let logoMaxBytes = 220
    /// The quiet zone, in modules, on every side of the code.
    static let quietModules = 4
    /// The smallest card side for a code with a logo, and for one without.
    static let minSide: CGFloat = 240
    static let minSideNoLogo: CGFloat = 288
    static let cardRadius: CGFloat = 12

    /// Fixed colours: the card is always light, whatever the theme.
    static let cardColor = UIColor.white
    static let dataInk = UIColor(red: 0x31 / 255, green: 0x2E / 255, blue: 0x81 / 255, alpha: 1)
    static let finderInk = UIColor(red: 0x43 / 255, green: 0x38 / 255, blue: 0xCA / 255, alpha: 1)

    private static let dotDiameter: CGFloat = 1.0
    private static let badgeRadius: CGFloat = 0.22
    private static let markSide: CGFloat = 0.76

    struct Plan {
        /// Modules per side, without the quiet zone.
        let size: Int
        /// Row-major, `true` = dark.
        let dark: [Bool]
        let level: String
        /// The badge, in modules: first row / column covered and the span.
        let logo: (from: Int, span: Int)?
    }

    /// The badge span for a code of `size` modules: the largest odd number
    /// not above 20% of the side, or 0 when that is under 5.
    static func logoSpan(_ size: Int) -> Int {
        var k = Int((Double(size) * 0.2).rounded(.down))
        if k % 2 == 0 { k -= 1 }
        return k >= 5 ? k : 0
    }

    /// Encode `text` and decide the level and the logo.
    static func plan(text: String) -> Plan? {
        let wantsLogo = text.utf8.count <= logoMaxBytes
        let level = wantsLogo ? "H" : "M"
        guard let matrix = matrix(text: text, level: level) else { return nil }
        let span = wantsLogo ? logoSpan(matrix.size) : 0
        let logo = span == 0 ? nil : (from: (matrix.size - span) / 2, span: span)
        return Plan(size: matrix.size, dark: matrix.dark, level: level, logo: logo)
    }

    /// The card side a caller's `requested` side becomes.
    static func side(_ plan: Plan, requested: CGFloat) -> CGFloat {
        max(requested, plan.logo == nil ? minSideNoLogo : minSide)
    }

    /// The module matrix CoreImage produced, one pixel per module, read back
    /// and cropped to the dark modules' bounds (the symbol always has dark
    /// modules in its first and last row and column: the finders).
    private static func matrix(text: String, level: String) -> (size: Int, dark: [Bool])? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = level
        guard let image = filter.outputImage,
              let cg = CIContext().createCGImage(image, from: image.extent)
        else { return nil }
        let w = cg.width, h = cg.height
        var px = [UInt8](repeating: 255, count: w * h)
        let drew: Bool = px.withUnsafeMutableBytes { buf in
            guard let ctx = CGContext(
                data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)
            else { return false }
            ctx.interpolationQuality = .none
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drew else { return nil }
        var minX = w, minY = h, maxX = -1, maxY = -1
        for y in 0..<h {
            for x in 0..<w where px[y * w + x] < 128 {
                minX = min(minX, x); maxX = max(maxX, x)
                minY = min(minY, y); maxY = max(maxY, y)
            }
        }
        let n = maxX - minX + 1
        guard n > 0, maxY - minY + 1 == n else { return nil }
        var dark = [Bool](repeating: false, count: n * n)
        for y in 0..<n {
            for x in 0..<n {
                dark[y * n + x] = px[(minY + y) * w + minX + x] < 128
            }
        }
        return (n, dark)
    }

    private static func inFinder(_ x: Int, _ y: Int, _ n: Int) -> Bool {
        (x < 7 && y < 7) || (x >= n - 7 && y < 7) || (x < 7 && y >= n - 7)
    }

    private static func roundRect(_ ctx: CGContext, _ rect: CGRect, _ radius: CGFloat, _ color: UIColor) {
        let r = min(radius, rect.width / 2, rect.height / 2)
        ctx.setFillColor(color.cgColor)
        ctx.addPath(CGPath(roundedRect: rect, cornerWidth: r, cornerHeight: r, transform: nil))
        ctx.fillPath()
    }

    /// Draw `plan` on a card of side `side` (top-left origin).
    static func paint(_ plan: Plan, side: CGFloat, in ctx: CGContext, mark: UIImage?) {
        let n = plan.size
        let m = side / CGFloat(n + 2 * quietModules)
        func at(_ module: Int) -> CGFloat { CGFloat(module + quietModules) * m }

        roundRect(ctx, CGRect(x: 0, y: 0, width: side, height: side), cardRadius, cardColor)

        for (fx, fy) in [(0, 0), (n - 7, 0), (0, n - 7)] {
            let ox = at(fx), oy = at(fy)
            roundRect(ctx, CGRect(x: ox, y: oy, width: 7 * m, height: 7 * m), m, finderInk)
            roundRect(ctx, CGRect(x: ox + m, y: oy + m, width: 5 * m, height: 5 * m), m / 2, cardColor)
            roundRect(ctx, CGRect(x: ox + 2 * m, y: oy + 2 * m, width: 3 * m, height: 3 * m), m / 2, finderInk)
        }

        let r = dotDiameter * m / 2
        ctx.setFillColor(dataInk.cgColor)
        for y in 0..<n {
            for x in 0..<n where plan.dark[y * n + x] && !inFinder(x, y, n) {
                if let logo = plan.logo,
                   (logo.from..<(logo.from + logo.span)).contains(x),
                   (logo.from..<(logo.from + logo.span)).contains(y) { continue }
                ctx.addEllipse(in: CGRect(x: at(x) + m / 2 - r, y: at(y) + m / 2 - r, width: 2 * r, height: 2 * r))
            }
        }
        ctx.fillPath()

        if let logo = plan.logo {
            let bx = at(logo.from)
            let badge = CGFloat(logo.span) * m
            roundRect(ctx, CGRect(x: bx, y: bx, width: badge, height: badge), badgeRadius * badge, cardColor)
            let s = markSide * badge
            if let mark {
                UIGraphicsPushContext(ctx)
                mark.draw(in: CGRect(x: bx + (badge - s) / 2, y: bx + (badge - s) / 2, width: s, height: s))
                UIGraphicsPopContext()
            }
        }
    }

    /// The brand tile, the same mark the auth screens show.
    static var brandMark: UIImage? { UIImage(named: "LakkoBrandMark") }

    /// Render `text` as an image of `side` points at `scale` pixels per point;
    /// `nil` when the text does not fit a QR code.
    static func render(text: String, side requested: CGFloat, scale: CGFloat) -> UIImage? {
        let key = "\(requested)|\(scale)|\(text)" as NSString
        if let cached = cache.object(forKey: key) { return cached }
        guard let plan = plan(text: text) else { return nil }
        let side = side(plan, requested: requested)
        let format = UIGraphicsImageRendererFormat()
        format.scale = scale
        format.opaque = false
        let mark = plan.logo == nil ? nil : brandMark
        let image = UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { r in
            paint(plan, side: side, in: r.cgContext, mark: mark)
        }
        cache.setObject(image, forKey: key)
        return image
    }

    /// Rendered codes, so a view update does not encode and draw again.
    private static let cache: NSCache<NSString, UIImage> = {
        let c = NSCache<NSString, UIImage>()
        c.countLimit = 8
        return c
    }()
}

/// One branded QR image for `text`, at the screen's scale.
struct BrandedQrImage: View {
    let text: String
    let size: CGFloat
    var label: String?
    @Environment(\.displayScale) private var displayScale
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let image = BrandedQr.render(text: text, side: size, scale: displayScale) {
            Image(uiImage: image)
                .resizable()
                .interpolation(.high)
                .frame(width: image.size.width, height: image.size.height)
                .accessibilityElement()
                .accessibilityLabel(label ?? l10n.t("common.qr_code"))
                .accessibilityAddTraits(.isImage)
        } else {
            Text(l10n.t("common.error.qr_render"))
                .foregroundStyle(.red)
                .font(.footnote)
        }
    }
}
