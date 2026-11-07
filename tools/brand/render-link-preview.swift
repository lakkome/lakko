// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors
//
// Renders the picture another app shows when a Lakko link is pasted into it
// (Telegram, WhatsApp, iMessage, Slack, X, Facebook, LinkedIn, Discord read
// it from the page's og:image / twitter:image tags). One picture for every
// link kind: the white script "L" on the brand gradient of the design system
// (.claude/skills/ux-ui/design-system.md section 1,
// linear-gradient(135deg, #312e81 0%, #818cf8 100%)). No text: the app
// prints the page title next to the picture, and drawing a word would tie the
// picture to a font whose licence may not allow it.
//
// 1200x630, the size Facebook, LinkedIn and X ask for a large card. The L is
// in the middle, so an app that crops the picture to a square around its
// centre still shows it.
//
// Usage (from the repository root, macOS):
//   swift tools/brand/render-link-preview.swift
//
// Writes apps/web/link-preview/lakko.png. The web build
// (apps/web/scripts/link-previews.mjs) copies it into dist/assets/ under a
// content-hashed name and points the preview tags at it.
//
// The glyph loader repeats the one in render-app-icons.swift (a script run
// with `swift file.swift` cannot import another script); keep the two in
// step if the glyph source changes.

import CoreGraphics
import Foundation
import ImageIO
import zlib

let width = 1200
let height = 630

/// Height of the L on the canvas: about half, so it still reads in the
/// small thumbnail some apps show.
let glyphHeight: CGFloat = 320

let startRGB = (0x31, 0x2e, 0x81)
let endRGB = (0x81, 0x8c, 0xf8)

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

/// The glyph as a white image with alpha, cropped to its bounding box. Stray
/// specks of the old tile are dropped by keeping the largest connected shape.
func loadGlyph(_ path: String) -> CGImage {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let raw = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else { fatalError("cannot read \(path)") }
    let w = raw.width, h = raw.height
    var px = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                        space: sRGB, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(raw, in: CGRect(x: 0, y: 0, width: w, height: h))

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
        let a = px[p * 4 + 3]
        px[p * 4] = a; px[p * 4 + 1] = a; px[p * 4 + 2] = a
        minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
    } }
    let full = ctx.makeImage()!
    return full.cropping(to: CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1))!
}

func render(glyph: CGImage) -> CGImage {
    let w = CGFloat(width), h = CGFloat(height)
    // Background: the brand gradient, top-left to bottom-right, written
    // pixel by pixel without dithering. CoreGraphics dithers its gradients,
    // which made a first version about 590 KB; plain 8-bit steps change
    // every few pixels along a row and compress to a fraction of that (no
    // banding seen when the result was checked by eye).
    let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                        bytesPerRow: width * 4, space: sRGB,
                        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    let px = ctx.data!.bindMemory(to: UInt8.self, capacity: width * height * 4)
    let denom = Double(width * width + height * height)
    func channel(_ a: Int, _ b: Int, _ t: Double) -> UInt8 {
        UInt8((Double(a) + Double(b - a) * t).rounded())
    }
    for y in 0..<height { for x in 0..<width {
        // Memory row 0 is the top of the image.
        let t = Double(x * width + y * height) / denom
        let p = (y * width + x) * 4
        px[p] = channel(startRGB.0, endRGB.0, t)
        px[p + 1] = channel(startRGB.1, endRGB.1, t)
        px[p + 2] = channel(startRGB.2, endRGB.2, t)
        px[p + 3] = 255
    } }
    ctx.interpolationQuality = .high
    ctx.setShouldAntialias(true)

    let glyphWidth = glyphHeight * CGFloat(glyph.width) / CGFloat(glyph.height)
    let glyphRect = CGRect(x: (w - glyphWidth) / 2, y: (h - glyphHeight) / 2,
                           width: glyphWidth, height: glyphHeight)
    ctx.draw(glyph, in: glyphRect)

    return ctx.makeImage()!
}

/// Writes `image` as an 8-bit RGB PNG with the Sub filter on every row. The
/// ImageIO encoder came out at about 215 KB for this picture; the gradient
/// changes by one step every few pixels along a row, which the Sub filter
/// turns into long runs of zeros, under 90 KB.
func writePNG(_ image: CGImage, _ path: String) {
    let w = image.width, h = image.height
    let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                        space: sRGB, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    let px = ctx.data!.bindMemory(to: UInt8.self, capacity: w * h * 4)

    var filtered = [UInt8]()
    filtered.reserveCapacity(h * (1 + w * 3))
    for y in 0..<h {
        filtered.append(1) // Sub
        var prev: (UInt8, UInt8, UInt8) = (0, 0, 0)
        for x in 0..<w {
            let p = (y * w + x) * 4
            let cur = (px[p], px[p + 1], px[p + 2])
            filtered.append(cur.0 &- prev.0)
            filtered.append(cur.1 &- prev.1)
            filtered.append(cur.2 &- prev.2)
            prev = cur
        }
    }

    // zlib at level 9: the lazy matching of the higher levels is what turns
    // the gradient's runs into short output (level 5 gave about 190 KB).
    var zlibLength = compressBound(uLong(filtered.count))
    var zlibStream = [UInt8](repeating: 0, count: Int(zlibLength))
    let status = compress2(&zlibStream, &zlibLength, filtered, uLong(filtered.count), 9)
    guard status == Z_OK else { fatalError("zlib failed: \(status)") }
    zlibStream.removeSubrange(Int(zlibLength)...)

    var ihdr = be32(UInt32(w)) + be32(UInt32(h))
    ihdr += [8, 2, 0, 0, 0] // 8-bit, RGB, deflate, adaptive filtering, no interlace
    var png: [UInt8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
    png += chunk("IHDR", ihdr)
    png += chunk("sRGB", [0]) // perceptual
    png += chunk("IDAT", zlibStream)
    png += chunk("IEND", [])

    try? FileManager.default.createDirectory(atPath: (path as NSString).deletingLastPathComponent,
                                             withIntermediateDirectories: true)
    guard FileManager.default.createFile(atPath: path, contents: Data(png)) else {
        fatalError("cannot write \(path)")
    }
    print("wrote \(path) (\(w)x\(h), \(png.count) bytes)")
}

func be32(_ v: UInt32) -> [UInt8] {
    [UInt8(v >> 24), UInt8((v >> 16) & 0xff), UInt8((v >> 8) & 0xff), UInt8(v & 0xff)]
}

let crcTable: [UInt32] = (0..<256).map { n -> UInt32 in
    var c = UInt32(n)
    for _ in 0..<8 { c = (c & 1) != 0 ? 0xedb8_8320 ^ (c >> 1) : c >> 1 }
    return c
}

func chunk(_ type: String, _ data: [UInt8]) -> [UInt8] {
    let body = Array(type.utf8) + data
    var c: UInt32 = 0xffff_ffff
    for v in body { c = crcTable[Int((c ^ UInt32(v)) & 0xff)] ^ (c >> 8) }
    return be32(UInt32(data.count)) + body + be32(c ^ 0xffff_ffff)
}

let root = FileManager.default.currentDirectoryPath
let glyph = loadGlyph("\(root)/docs/brand/lakko-glyph-512.png")
writePNG(render(glyph: glyph), "\(root)/apps/web/link-preview/lakko.png")
