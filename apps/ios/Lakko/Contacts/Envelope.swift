// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// One-byte kind discriminator for messages a Lakko mailbox carries.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/envelope.ts` (1.17e5)
///   * `apps/android/.../contacts/Envelope.kt` (1.15e5)
///
/// Two kinds today:
///
///   * ``EnvelopeKind/contactRequest`` (`0x01`) — body is a
///     `ContactRequest` wire blob produced by
///     `composeContactRequest` (UniFFI).
///   * ``EnvelopeKind/welcome`` (`0x02`) — body is an MLS Welcome
///     wire blob produced by `Group.addMember(...)`'s `welcome`
///     field (consumed by the chat surface in 1.14f).
///
/// ## Why a 1-byte discriminator?
///
/// The DS server is opaque to mailbox payloads (publishes raw
/// bytes); the receiver needs to know which decoder to reach for
/// before handing the body to UniFFI. A single byte is enough —
/// the inner formats already carry their own integrity
/// (`ContactRequest` is Ed25519-signed; MLS `Welcome` is
/// authenticated by the sender's KeyPackage signature). A flipped
/// kind byte just makes the receiver dispatch to the wrong decoder,
/// which then rejects the body cleanly.

/// Envelope discriminator byte.
public enum EnvelopeKind: UInt8 {
    case contactRequest = 0x01
    case welcome = 0x02
    /// 1.9b1 — recipient's accept-reply to a `ContactRequest`.
    /// Body is the wire bytes returned by
    /// `ContactAck.compose(...)` (UniFFI free function in
    /// `LakkoCore`).
    case contactAck = 0x03
}

/// Wrap inner bytes with the given kind discriminator. Returns a
/// fresh `Data` — caller may mutate.
public func wrapEnvelope(kind: EnvelopeKind, body: Data) -> Data {
    var out = Data(capacity: body.count + 1)
    out.append(kind.rawValue)
    out.append(body)
    return out
}

/// Discriminated parse output. Unknown kinds surface as
/// ``ParsedEnvelope/unknown(rawKind:body:)`` so the caller can
/// log + skip rather than throw.
public enum ParsedEnvelope: Equatable {
    case contactRequest(body: Data)
    case welcome(body: Data)
    /// 1.9b1 — recipient's accept-reply.
    case contactAck(body: Data)
    /// Future / unknown kind byte. Caller logs + advances cursor.
    /// `rawKind = -1` is used for the empty-input degenerate case.
    case unknown(rawKind: Int, body: Data)
}

/// Strip the leading kind byte and dispatch. Empty input surfaces
/// as ``ParsedEnvelope/unknown(rawKind:body:)`` with `rawKind = -1`
/// rather than throwing — Phase-1 the DS doesn't lower-bound
/// payload size on read, and we want garbage to skip-and-log on the
/// receiver, not crash.
public func parseEnvelope(_ bytes: Data) -> ParsedEnvelope {
    if bytes.isEmpty {
        return .unknown(rawKind: -1, body: bytes)
    }
    let kind = bytes[bytes.startIndex]
    // `body` is a fresh copy via `Data(...)` to drop the original
    // SubSequence's index offsets — downstream callers treat the
    // result as a self-contained buffer.
    let body = Data(bytes.dropFirst())
    if let known = EnvelopeKind(rawValue: kind) {
        switch known {
        case .contactRequest:
            return .contactRequest(body: body)
        case .welcome:
            return .welcome(body: body)
        case .contactAck:
            return .contactAck(body: body)
        }
    }
    return .unknown(rawKind: Int(kind), body: body)
}
