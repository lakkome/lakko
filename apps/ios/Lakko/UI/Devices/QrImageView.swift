// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreImage.CIFilterBuiltins
import Foundation
import SwiftUI
import UIKit

/// `QrImageView` — render arbitrary bytes as a QR code via
/// CoreImage's built-in `CIQRCodeGenerator` (Phase 2a3b3-ui).
///
/// Mirror of the `QRCode.toCanvas(...)` call site on the web
/// side. The payload is encoded as URL-safe-no-pad base64
/// before generation so the QR carries ASCII text only — the
/// camera-side scanner on every platform decodes the same
/// shape via [`Devices/PairingCodec/parsePairingQrPayload`].
public struct QrImageView: View {

    public let payload: Data
    public let size: CGFloat

    public init(payload: Data, size: CGFloat = 256) {
        self.payload = payload
        self.size = size
    }

    public var body: some View {
        let encoded = Self.base64UrlEncodeNoPad(payload)
        if let image = Self.render(text: encoded, target: size) {
            Image(uiImage: image)
                .interpolation(.none)
                .resizable()
                .frame(width: size, height: size)
        } else {
            Text("Failed to render QR.")
                .foregroundStyle(.red)
                .font(.footnote)
        }
    }

    // MARK: - Internal

    private static func render(text: String, target: CGFloat) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        // "M" — 15% error correction, the same level the web
        // side asks `qrcode` for. Plenty of slack for camera
        // jitter without inflating the byte capacity.
        filter.correctionLevel = "M"
        guard let baseImage = filter.outputImage else { return nil }
        // Scale up by an integer factor so the nearest-neighbour
        // resize at render time produces crisp squares — the
        // CoreImage output is one pixel per QR module.
        let modulePixels = max(1, ceil(target / baseImage.extent.width))
        let scaled = baseImage.transformed(
            by: CGAffineTransform(scaleX: modulePixels, y: modulePixels)
        )
        let context = CIContext()
        guard let cg = context.createCGImage(scaled, from: scaled.extent) else {
            return nil
        }
        return UIImage(cgImage: cg)
    }

    // MARK: - base64url (no padding)

    private static func base64UrlEncodeNoPad(_ bytes: Data) -> String {
        bytes.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
