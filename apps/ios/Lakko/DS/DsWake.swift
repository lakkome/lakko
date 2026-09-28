// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0054 decision 1 — the push wake a signed group-journal publish asks
/// the Delivery Service to forward.
///
/// A wake used to be a second, separate request: after a publish the sender
/// called `POST /push/notify-group` on push-gw. It is now one header on the
/// publish itself, `x-lakko-wake: <push_gid>;<kind>`, and the DS forwards it to
/// push-gw only after the ADR-0053 proof gate passed and the row was appended,
/// so only a writer of the journal can wake it.
///
/// The header carries exactly two things:
///
///   * `push_gid` — the group's 32-byte opaque push routing id (ADR-0042),
///     base64url WITHOUT padding (43 characters). Never the DS `group_id`: a
///     device that does not hold the group's routing secret yet sends no
///     header at all (the ADR-0042 defer), rather than falling back to the
///     journal key.
///   * `kind` — ``Kind/message`` for a chat message, ``Kind/call`` for a call
///     offer or end (`docs/ux/calls.md` section 2.1). Control envelopes
///     (typing, receipts, beacons, routing secrets, edits, reactions) carry no
///     header and stay silent, exactly as they did under the old wake.
///
/// The DS answers `400` to a malformed header, so it is built here only, from
/// typed parts, and the only way to get a ``DsWake`` is the failable
/// initializer that checks the length.
public struct DsWake: Equatable {

    /// The class of wake. Spelled on the wire exactly as the raw value.
    public enum Kind: String, Equatable {
        /// An ordinary message: the recipient's device shows a notification.
        case message
        /// A call offer or end: the recipient's device rings (or stops).
        case call
    }

    /// The header name the DS reads.
    public static let headerName = "x-lakko-wake"

    /// The `push_gid` length (ADR-0042, a keyed-BLAKE3 output).
    public static let pushGidLength = 32

    /// The 32-byte opaque push routing id.
    public let pushGid: Data
    /// What kind of wake is asked for.
    public let kind: Kind

    /// `nil` unless `pushGid` is exactly 32 bytes, so a malformed id can never
    /// reach the wire (the DS would refuse the whole publish with a `400`).
    public init?(pushGid: Data, kind: Kind) {
        guard pushGid.count == Self.pushGidLength else { return nil }
        self.pushGid = pushGid
        self.kind = kind
    }

    /// The header value: `<push_gid base64url, no padding>;<kind>`.
    public var headerValue: String {
        "\(Self.base64UrlNoPad(pushGid));\(kind.rawValue)"
    }

    /// RFC 4648 section 5 base64url, padding stripped.
    static func base64UrlNoPad(_ bytes: Data) -> String {
        var s = bytes.base64EncodedString()
        s = s.replacingOccurrences(of: "+", with: "-")
        s = s.replacingOccurrences(of: "/", with: "_")
        while s.hasSuffix("=") { s.removeLast() }
        return s
    }
}
