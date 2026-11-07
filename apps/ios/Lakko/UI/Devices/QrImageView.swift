// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// `QrImageView` — arbitrary bytes as a branded QR code (design system
/// section 3, "QR code"; drawn by ``BrandedQr``).
///
/// The payload is encoded as URL-safe-no-pad base64 before generation so the
/// QR carries ASCII text only — the camera-side scanner on every platform
/// decodes the same shape via [`Devices/PairingCodec/parsePairingQrPayload`].
/// `size` is the requested card side, raised to the design-system minimum;
/// the card (white, with its quiet zone) is part of the view.
public struct QrImageView: View {

    public let payload: Data
    public let size: CGFloat

    public init(payload: Data, size: CGFloat = 288) {
        self.payload = payload
        self.size = size
    }

    public var body: some View {
        BrandedQrImage(text: Self.base64UrlEncodeNoPad(payload), size: size)
    }

    // MARK: - base64url (no padding)

    static func base64UrlEncodeNoPad(_ bytes: Data) -> String {
        bytes.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// RAW-TEXT QR (no base64 wrapping) — for payloads that are already
/// text a third-party scanner must read verbatim, e.g. an invite link or the
/// `otpauth://` provisioning URI an authenticator app consumes
/// (ADR-0018 factor (c)). The pairing flow keeps the byte + base64url
/// path of ``QrImageView``.
public struct QrTextImageView: View {

    public let text: String
    public let size: CGFloat

    public init(text: String, size: CGFloat = 240) {
        self.text = text
        self.size = size
    }

    public var body: some View {
        BrandedQrImage(text: text, size: size)
    }
}
