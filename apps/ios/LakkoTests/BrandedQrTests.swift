// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreImage
import LakkoCore
import UIKit
import XCTest
@testable import Lakko

/// The branded QR code scans (design system section 3, "QR code"): the image
/// `BrandedQr` draws, with the real brand mark in the badge, is read back by
/// CoreImage's QR detector. Payloads: the largest device pairing payload (a
/// real key package and a device card of maximal length), an invite link,
/// the authenticator setup code, and a text of exactly the logo threshold.
final class BrandedQrTests: XCTestCase {

    private static let invite = "https://lakko.me/i/AbCdEfGh#0123456789abcdef"
    private static let otpauth =
        "otpauth://totp/Lakko:valentin_goncharov?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Lakko&algorithm=SHA1&digits=6&period=30"
    private static let threshold = String(repeating: "h", count: BrandedQr.logoMaxBytes)

    private func largestPairingText() throws -> String {
        let identity = try Identity.fromSeed(seed: Data(repeating: 9, count: 32))
        let long = String(repeating: "W", count: 64)
        let card = FfiDeviceCard(
            updatedAtMs: 1, lastActiveMs: 1, platform: .desktop, model: long,
            appVersion: String(repeating: "x", count: 32), os: long, city: long, country: "NL")
        let payload = try buildPairingQr(
            loginHandle: Data(repeating: 1, count: 32),
            keyPackage: identity.createKeyPackage(),
            rendezvousPseudonym: Data(repeating: 2, count: 32),
            deviceCardEnvelope: try composeDeviceCardEnvelope(card: card))
        return QrImageView.base64UrlEncodeNoPad(payload)
    }

    private func decode(_ image: UIImage) -> String? {
        guard let ci = CIImage(image: image),
              let detector = CIDetector(
                ofType: CIDetectorTypeQRCode, context: nil,
                options: [CIDetectorAccuracy: CIDetectorAccuracyHigh])
        else { return nil }
        return detector.features(in: ci)
            .compactMap { ($0 as? CIQRCodeFeature)?.messageString }
            .first
    }

    /// Draw the code on a dark page, the worst case for the card's edge.
    private func onDarkPage(_ image: UIImage) -> UIImage {
        let pad: CGFloat = 24
        let size = CGSize(width: image.size.width + 2 * pad, height: image.size.height + 2 * pad)
        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        return UIGraphicsImageRenderer(size: size, format: format).image { r in
            UIColor(red: 0x0F / 255, green: 0x12 / 255, blue: 0x20 / 255, alpha: 1).setFill()
            r.fill(CGRect(origin: .zero, size: size))
            image.draw(at: CGPoint(x: pad, y: pad))
        }
    }

    private func assertScans(_ text: String, side: CGFloat, scale: CGFloat,
                             file: StaticString = #filePath, line: UInt = #line) throws {
        let image = try XCTUnwrap(BrandedQr.render(text: text, side: side, scale: scale), file: file, line: line)
        dump(image, name: "ios-\(text.count)-\(Int(side))-\(Int(scale))x")
        XCTAssertEqual(decode(onDarkPage(image)), text, file: file, line: line)
    }

    /// With `LAKKO_QR_DUMP_DIR` set, write a rendering as a PNG to look at.
    private func dump(_ image: UIImage, name: String) {
        guard let dir = ProcessInfo.processInfo.environment["LAKKO_QR_DUMP_DIR"], !dir.isEmpty,
              let png = onDarkPage(image).pngData()
        else { return }
        try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }

    func testTheLargestPairingPayloadHasNoLogoAndLevelM() throws {
        let text = try largestPairingText()
        XCTAssertGreaterThan(text.count, 800)
        let plan = try XCTUnwrap(BrandedQr.plan(text: text))
        XCTAssertNil(plan.logo)
        XCTAssertEqual(plan.level, "M")
        XCTAssertEqual(BrandedQr.side(plan, requested: 0), BrandedQr.minSideNoLogo)
    }

    func testShortTextsGetALogoAtLevelH() throws {
        XCTAssertNotNil(BrandedQr.brandMark, "the brand mark must be in the app bundle")
        for text in [Self.invite, Self.otpauth, Self.threshold] {
            let plan = try XCTUnwrap(BrandedQr.plan(text: text))
            XCTAssertEqual(plan.level, "H")
            let logo = try XCTUnwrap(plan.logo)
            XCTAssertLessThanOrEqual(Double(logo.span), Double(plan.size) * 0.2)
            XCTAssertEqual(logo.span % 2, 1)
        }
        XCTAssertNil(BrandedQr.plan(text: Self.threshold + "h")?.logo)
    }

    func testTheModuleMatrixMatchesTheVersion() throws {
        // Version 5 at H (37 modules) for the invite link, version 15 (77) at
        // the threshold: the same symbols the web and Android encoders make.
        XCTAssertEqual(BrandedQr.plan(text: Self.invite)?.size, 37)
        XCTAssertEqual(BrandedQr.plan(text: Self.threshold)?.size, 77)
    }

    func testThePairingPayloadScans() throws {
        let text = try largestPairingText()
        try assertScans(text, side: BrandedQr.minSideNoLogo, scale: 1)
        try assertScans(text, side: BrandedQr.minSideNoLogo, scale: 3)
    }

    func testAnInviteLinkScans() throws {
        try assertScans(Self.invite, side: BrandedQr.minSide, scale: 1)
        try assertScans(Self.invite, side: 264, scale: 3)
    }

    func testTheSetupCodeScans() throws {
        try assertScans(Self.otpauth, side: BrandedQr.minSide, scale: 2)
    }

    func testATextAtTheLogoThresholdScans() throws {
        try assertScans(Self.threshold, side: BrandedQr.minSide, scale: 1)
        try assertScans(Self.threshold, side: BrandedQr.minSide, scale: 3)
    }
}
