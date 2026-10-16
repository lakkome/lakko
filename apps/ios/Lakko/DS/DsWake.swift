// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0054 decision 1 / ADR-0078 section 3 — the push wake a signed
/// group-journal publish asks the Delivery Service to forward.
///
/// A wake used to be a second, separate request: after a publish the sender
/// called `POST /push/notify-group` on push-gw. It is one header on the
/// publish itself, `x-lakko-wake`, and the DS forwards it to push-gw only
/// after the ADR-0053 proof gate passed and the row was appended.
///
/// Since ADR-0078 the value is a wake **sealed to push-gw's X25519 key**
/// (`LakkoCore.pushWakeHeaderValue`): the group's `push_gid`, the kind, a
/// nonce, a timestamp, the group's unlock key and a signature by the group's
/// wake key are all inside, as base64url without padding (312 characters).
/// The DS checks the shape and forwards it unread, so it no longer learns
/// which group is woken or whether the wake is a call. A device that does not
/// hold the group's routing secret sends no header (the ADR-0042 defer).
///
/// The DS answers `400` to a malformed header, so the only way to get a
/// ``DsWake`` is the failable initializer that checks the shape.
public struct DsWake: Equatable {

    /// The class of wake, sealed inside the header.
    public enum Kind: String, Equatable {
        /// An ordinary message: the recipient's device shows a notification.
        case message
        /// A call offer or end: the recipient's device rings (or stops).
        case call
    }

    /// The header name the DS reads.
    public static let headerName = "x-lakko-wake"

    /// The header value length (mirror of
    /// `lakko_contacts::push_wake::PUSH_WAKE_HEADER_LEN`).
    public static let headerValueLength = 312

    /// The sealed wake, base64url without padding.
    public let headerValue: String

    /// `nil` unless `headerValue` is exactly ``headerValueLength`` base64url
    /// characters, so a malformed wake can never reach the wire (the DS would
    /// refuse the whole publish with a `400`).
    public init?(headerValue: String) {
        guard headerValue.count == Self.headerValueLength,
              headerValue.unicodeScalars.allSatisfy({
                  ("A"..."Z").contains($0) || ("a"..."z").contains($0)
                      || ("0"..."9").contains($0) || $0 == "-" || $0 == "_"
              }) else { return nil }
        self.headerValue = headerValue
    }
}
