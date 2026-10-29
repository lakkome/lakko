// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Renders the Lakko app icons for iOS, Android and the web from the brand
// glyph in docs/brand/lakko-glyph-512.png and the brand gradient of the
// design system (.claude/skills/ux-ui/design-system.md section 1):
// linear-gradient(135deg, #312e81 0%, #818cf8 100%), the white script "L".
//
// Usage (from the repository root):
//   swift tools/brand/render-app-icons.swift
//
// Writes:
//   apps/ios/Lakko/Assets.xcassets/AppIcon.appiconset/   light, dark, tinted 1024
//   apps/android/app/src/main/res/mipmap-*/              adaptive foreground per density
//   apps/android/app/src/main/res/drawable-*/            notification icon (ic_stat_lakko)
//   apps/web/public/                                     favicon, 96/192/512, maskable, apple-touch, badge
//   apps/desktop/src-tauri/icons/icon.png                macOS app icon (1024, on the Apple icon grid)
//
// The glyph is a raster cut from the original icon (the design system asks
// for a vector trace before a store submission); the 1024 iOS icon is
// therefore an upscale. Stray specks of the old tile in the glyph file are
// dropped by keeping the largest connected shape only.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

// MARK: - Composition

/// Where the L sits on a tile of side 1, measured from the original icon:
/// its bounding box is 0.693 of the tile wide, centred horizontally and a
/// little above the middle.
let glyphWidthOnTile: CGFloat = 0.693
let glyphCentreOnTile = CGPoint(x: 0.502, y: 0.440) // top-left origin

/// The brand gradient stops, 135 degrees: top-left to bottom-right.
let gradientStart = rgb(0x31, 0x2e, 0x81)
let gradientEnd = rgb(0x81, 0x8c, 0xf8)

func rgb(_ r: Int, _ g: Int, _ b: Int, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: a)
}

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

// MARK: - Glyph

struct Glyph {
    /// The glyph as a white image with alpha, cropped to its bounding box.
    let image: CGImage
}

func loadGlyph(_ path: String) -> Glyph {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let raw = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else { fatalError("cannot read \(path)") }
    let w = raw.width, h = raw.height
    var px = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                        space: sRGB, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(raw, in: CGRect(x: 0, y: 0, width: w, height: h))

    // Label shapes of alpha > 0 (8-connected) and keep the largest one.
    var label = [Int32](repeating: 0, count: w * h)
    var sizes: [Int] = [0]
    var stack: [Int] = []
    for start in 0..<(w * h) where px[start * 4 + 3] > 0 && label[start] == 0 {
        let id = Int32(sizes.count)
        var count = 0
        label[start] = id
        stack.append(start)
        while let p = stack.popLast() {
            count += 1
            let x = p % w, y = p / w
            for dy in -1...1 { for dx in -1...1 {
                let nx = x + dx, ny = y + dy
                guard nx >= 0, ny >= 0, nx < w, ny < h else { continue }
                let q = ny * w + nx
                if px[q * 4 + 3] > 0 && label[q] == 0 { label[q] = id; stack.append(q) }
            } }
        }
        sizes.append(count)
    }
    let keep = Int32(sizes.indices.max { sizes[$0] < sizes[$1] }!)

    var minX = w, minY = h, maxX = 0, maxY = 0
    for y in 0..<h { for x in 0..<w {
        let p = y * w + x
        if label[p] != keep {
            px[p * 4] = 0; px[p * 4 + 1] = 0; px[p * 4 + 2] = 0; px[p * 4 + 3] = 0
            continue
        }
        // White with the source alpha (premultiplied).
        let a = px[p * 4 + 3]
        px[p * 4] = a; px[p * 4 + 1] = a; px[p * 4 + 2] = a
        minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
    } }
    let full = ctx.makeImage()!
    // CGImage cropping uses a top-left origin for raster data.
    let crop = full.cropping(to: CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1))!
    return Glyph(image: crop)
}

// MARK: - Drawing

enum Background {
    case brandGradient
    case none
    case solid(CGColor)
}

enum GlyphFill {
    case white
    case color(CGColor)
    case gradient(CGColor, CGColor)
}

/// Draws one icon of `size` pixels. The background body is `bodyFraction`
/// of the canvas, centred, with a soft drop shadow when it is smaller than
/// the canvas; the tile (the area the composition is measured on) is
/// `tileFraction` of the body, centred; `cornerFraction` rounds the body
/// (0 = square).
func render(size: Int, glyph: Glyph, background: Background, fill: GlyphFill,
            bodyFraction: CGFloat = 1, tileFraction: CGFloat = 1, cornerFraction: CGFloat = 0,
            opaque: Bool) -> CGImage {
    let s = CGFloat(size)
    let info = opaque ? CGImageAlphaInfo.noneSkipLast.rawValue : CGImageAlphaInfo.premultipliedLast.rawValue
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
                        space: sRGB, bitmapInfo: info)!
    ctx.interpolationQuality = .high
    ctx.setShouldAntialias(true)
    // Work in a top-left origin.
    ctx.translateBy(x: 0, y: s)
    ctx.scaleBy(x: 1, y: -1)

    let canvas = CGRect(x: 0, y: 0, width: s, height: s)
    let bodySide = s * bodyFraction
    let body = CGRect(x: (s - bodySide) / 2, y: (s - bodySide) / 2, width: bodySide, height: bodySide)
    if bodyFraction < 1, !opaque {
        ctx.clear(canvas)
        // The shadow of the body; drawn in an unflipped sense, so a positive
        // y offset in the flipped context reads as "below".
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.010), blur: s * 0.020,
                      color: rgb(0, 0, 0, 0.30))
        ctx.setFillColor(gradientStart)
        fillRounded(ctx, body, cornerFraction)
        ctx.restoreGState()
    }
    switch background {
    case .none:
        ctx.clear(canvas)
    case .solid(let c):
        ctx.setFillColor(c)
        fillRounded(ctx, body, cornerFraction)
    case .brandGradient:
        ctx.saveGState()
        addRounded(ctx, body, cornerFraction)
        ctx.clip()
        let g = CGGradient(colorsSpace: sRGB, colors: [gradientStart, gradientEnd] as CFArray, locations: [0, 1])!
        ctx.drawLinearGradient(g, start: CGPoint(x: body.minX, y: body.minY),
                               end: CGPoint(x: body.maxX, y: body.maxY), options: [])
        ctx.restoreGState()
    }

    let tile = bodySide * tileFraction
    let origin = (s - tile) / 2
    let gw = tile * glyphWidthOnTile
    let gh = gw * CGFloat(glyph.image.height) / CGFloat(glyph.image.width)
    let cx = origin + tile * glyphCentreOnTile.x
    let cy = origin + tile * glyphCentreOnTile.y
    let rect = CGRect(x: cx - gw / 2, y: cy - gh / 2, width: gw, height: gh)

    ctx.saveGState()
    // Draw the glyph upright in the flipped context.
    ctx.translateBy(x: rect.minX, y: rect.maxY)
    ctx.scaleBy(x: 1, y: -1)
    let local = CGRect(origin: .zero, size: rect.size)
    switch fill {
    case .white:
        ctx.draw(glyph.image, in: local)
    case .color(let c):
        ctx.clip(to: local, mask: glyph.image)
        ctx.setFillColor(c)
        ctx.fill(local)
    case .gradient(let a, let b):
        ctx.clip(to: local, mask: glyph.image)
        let g = CGGradient(colorsSpace: sRGB, colors: [a, b] as CFArray, locations: [0, 1])!
        // Top-left to bottom-right of the glyph, in the unflipped local space.
        ctx.drawLinearGradient(g, start: CGPoint(x: 0, y: local.height), end: CGPoint(x: local.width, y: 0), options: [])
    }
    ctx.restoreGState()
    return ctx.makeImage()!
}

func addRounded(_ ctx: CGContext, _ r: CGRect, _ fraction: CGFloat) {
    if fraction <= 0 { ctx.addRect(r); return }
    let radius = r.width * fraction
    ctx.addPath(CGPath(roundedRect: r, cornerWidth: radius, cornerHeight: radius, transform: nil))
}

func fillRounded(_ ctx: CGContext, _ r: CGRect, _ fraction: CGFloat) {
    addRounded(ctx, r, fraction)
    ctx.fillPath()
}

func writePNG(_ image: CGImage, _ path: String) {
    try? FileManager.default.createDirectory(atPath: (path as NSString).deletingLastPathComponent,
                                             withIntermediateDirectories: true)
    guard let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL,
                                                     UTType.png.identifier as CFString, 1, nil)
    else { fatalError("cannot write \(path)") }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fatalError("cannot write \(path)") }
    print("wrote \(path) (\(image.width)x\(image.height))")
}

// MARK: - Outputs

let root = FileManager.default.currentDirectoryPath
let glyph = loadGlyph("\(root)/docs/brand/lakko-glyph-512.png")

// iOS: one 1024 image per appearance. The system masks the corners.
let ios = "\(root)/apps/ios/Lakko/Assets.xcassets/AppIcon.appiconset"
writePNG(render(size: 1024, glyph: glyph, background: .brandGradient, fill: .white, opaque: true),
         "\(ios)/AppIcon-1024.png")
// Dark: no background (the system draws its dark one), the L in light indigo.
writePNG(render(size: 1024, glyph: glyph, background: .none,
                fill: .gradient(rgb(0xc7, 0xd2, 0xfe), rgb(0x81, 0x8c, 0xf8)), opaque: false),
         "\(ios)/AppIcon-1024-dark.png")
// Tinted: a grayscale image the system tints; the L in white on no background.
writePNG(render(size: 1024, glyph: glyph, background: .none, fill: .white, opaque: false),
         "\(ios)/AppIcon-1024-tinted.png")

// Android adaptive icon: a 108dp layer of which 72dp shows after the mask,
// so the composition tile is 72/108 of the canvas. The background layer is
// a vector gradient (res/drawable/ic_launcher_background.xml); the
// foreground PNG doubles as the monochrome layer.
let android = "\(root)/apps/android/app/src/main/res"
for (density, scale) in [("mdpi", 1), ("hdpi", 1.5), ("xhdpi", 2), ("xxhdpi", 3), ("xxxhdpi", 4)] as [(String, Double)] {
    let px = Int(108 * scale)
    writePNG(render(size: px, glyph: glyph, background: .none, fill: .white,
                    tileFraction: 72.0 / 108.0, opaque: false),
             "\(android)/mipmap-\(density)/ic_launcher_foreground.png")
}

// Web: rounded tiles for browsers and the PWA "any" purpose; a full-bleed
// square for "maskable" (safe zone: the same 72/108 tile) and for the
// apple-touch icon (iOS rounds it itself).
let web = "\(root)/apps/web/public"
let corner: CGFloat = 0.225
for (name, size) in [("favicon.png", 32), ("icon-96.png", 96), ("icon-192.png", 192), ("icon-512.png", 512)] {
    writePNG(render(size: size, glyph: glyph, background: .brandGradient, fill: .white,
                    cornerFraction: corner, opaque: false),
             "\(web)/\(name)")
}
writePNG(render(size: 512, glyph: glyph, background: .brandGradient, fill: .white,
                tileFraction: 72.0 / 108.0, opaque: true),
         "\(web)/icon-maskable-512.png")
writePNG(render(size: 180, glyph: glyph, background: .brandGradient, fill: .white, opaque: true),
         "\(web)/apple-touch-icon.png")

// Notification icons are drawn by their alpha only: the white L alone, no
// tile, nearly filling the canvas. Android status bar: 24dp per density.
// Web Push badge: 96px.
let statTile: CGFloat = 1.2
for (density, scale) in [("mdpi", 1), ("hdpi", 1.5), ("xhdpi", 2), ("xxhdpi", 3), ("xxxhdpi", 4)] as [(String, Double)] {
    writePNG(render(size: Int(24 * scale), glyph: glyph, background: .none, fill: .white,
                    tileFraction: statTile, opaque: false),
             "\(android)/drawable-\(density)/ic_stat_lakko.png")
}
writePNG(render(size: 96, glyph: glyph, background: .none, fill: .white,
                tileFraction: statTile, opaque: false),
         "\(web)/badge-96.png")

// macOS (the desktop app, ADR-0085): macOS draws an app icon as given, with
// no mask, and its own icons sit on the Apple icon grid: an 824-point body
// on a 1024-point canvas, the rest transparent. A full-bleed tile reads
// larger than every other icon in the Dock.
writePNG(render(size: 1024, glyph: glyph, background: .brandGradient, fill: .white,
                bodyFraction: 824.0 / 1024.0, cornerFraction: corner, opaque: false),
         "\(root)/apps/desktop/src-tauri/icons/icon.png")
