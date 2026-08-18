// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Readable surfacing of the retired v1 PublisherGroup derivation.
///
/// Swift mirror of `apps/web/src/channels/legacy-v1.ts` — the copy is written
/// once and ported verbatim to all three clients.
///
/// `crates/lakko-mls/src/publisher_group.rs` narrowed the PublisherGroup MLS
/// group-id prefix from `b"lakko/publisher-group/v1/"` (41 bytes total) to
/// `b"lakko/pubgrp/v2/"` (32 bytes) so the id fits the Delivery Service's
/// 32-byte pseudonym width. An MLS `group_id` is bound into `GroupContext` and
/// every epoch secret derived from it, so v1 state **cannot** be re-keyed in
/// place — `PublisherGroup::load` / `::create` refuse loudly instead, and
/// already-minted `/g/` co-publisher links carry a v1 id that no longer
/// resolves to a channel.
///
/// Both refusals are correct and neither is repairable. This enum turns them
/// from raw Rust error strings into copy a channel owner can act on, and — the
/// load-bearing part — does so **narrowly**, so an unrelated failure (network,
/// server rejection, a bad channel-id hex) still reads as itself.
///
/// ## What each detection keys on
///
/// - **The dead invite link** keys on the *bytes*: a 41-byte `group_id`
///   carrying the retired v1 prefix. Structural, and it cannot drift — the v1
///   prefix is a frozen historical constant (nothing new is minted under it)
///   and a v2 id is a different length. See ``isLegacyV1PublisherGroupId(_:)``.
///
/// - **The unopenable channel** keys on a substring of the Rust refusal
///   message. That is genuinely fragile: `lakko_mls::Error::Group(String)` is
///   stringly typed and UniFFI carries the payload through as
///   `FfiError.Group(msg)` with no further discriminant, so there is nothing
///   else to match on. An edit to the message in `refuse_if_legacy_v1_present`
///   would silently fall back to the generic error text with no compile-time
///   signal from this side. The durable fix is a typed error variant in the
///   core; that is a `crates/` change and deliberately not made here.
///   See ``isLegacyPublisherGroupError(_:)``.
public enum LegacyChannelV1 {

    /// The retired v1 group-id prefix, as ASCII bytes. Frozen: nothing mints
    /// under this any more. Mirrors `PUBLISHER_GROUP_ID_PREFIX_V1` in
    /// `crates/lakko-mls/src/publisher_group.rs`.
    static let v1Prefix = Array("lakko/publisher-group/v1/".utf8)

    /// Total byte length of a v1 PublisherGroup id: 25-byte prefix + 16-byte
    /// channel id.
    static let v1GroupIdLength = v1Prefix.count + 16

    /// Distinctive substring of the Rust-side refusal raised by
    /// `refuse_if_legacy_v1_present`. Matched case-sensitively; kept as narrow
    /// as possible while staying unique to that one call site.
    static let rustRefusalMarker = "legacy v1 PublisherGroup state"

    /// User-facing copy for a channel whose MLS state predates the v2
    /// derivation.
    public static let channelMessage =
        "This channel was created by an older version of Lakko and cannot be opened in this build. "
        + "Posts you already published stay readable for your subscribers. "
        + "To publish again, create a new channel — this one cannot be repaired."

    /// User-facing copy for a `/g/` co-publisher link minted under the v1
    /// derivation.
    public static let coPublisherLinkMessage =
        "This co-publisher invite link was created by an older version of Lakko and no longer works. "
        + "Ask the channel owner to send you a new invite link."

    /// True when `groupId` carries the retired 41-byte v1 PublisherGroup shape.
    ///
    /// Structural: length plus the frozen ASCII prefix. A v2 id is 32 bytes,
    /// and a chat / DeviceGroup id is 32 bytes of non-matching content, so this
    /// never fires on anything current.
    public static func isLegacyV1PublisherGroupId(_ groupId: Data) -> Bool {
        guard groupId.count == v1GroupIdLength else { return false }
        return Array(groupId.prefix(v1Prefix.count)) == v1Prefix
    }

    /// Best-effort text of `error`, reaching inside the UniFFI `FfiError`
    /// payload where it carries the Rust message verbatim.
    static func errorText(_ error: Error) -> String {
        if let legacy = error as? LegacyChannelError { return legacy.message }
        if case let FfiError.Group(msg) = error { return msg }
        if let localized = error as? LocalizedError, let described = localized.errorDescription {
            return described
        }
        return String(describing: error)
    }

    /// True when `text` carries the Rust-side refusal marker.
    ///
    /// The single string-matching site on iOS — see the type doc for why no
    /// better handle exists and why this is fragile. Split out as a pure
    /// function over `String` so the (FFI-free) test target can drive it
    /// directly without linking `LakkoCore`.
    public static func isLegacyRefusalText(_ text: String) -> Bool {
        text.contains(rustRefusalMarker)
    }

    /// True when `error` is the Rust-side refusal to open a v1 PublisherGroup.
    public static func isLegacyPublisherGroupError(_ error: Error) -> Bool {
        if let legacy = error as? LegacyChannelError { return legacy.kind == .channel }
        return isLegacyRefusalText(errorText(error))
    }

    /// Map `error` to a ``LegacyChannelError`` when it is the retired-v1
    /// refusal; otherwise return `error` untouched so unrelated failures keep
    /// their own text.
    public static func mapOpenError(_ error: Error) -> Error {
        isLegacyPublisherGroupError(error) ? LegacyChannelError(kind: .channel) : error
    }

    /// Message to show for `error`: the retired-v1 copy when it applies,
    /// otherwise `fallback(error)` so a network / server / codec failure still
    /// reads as itself.
    public static func describe(_ error: Error, fallback: (Error) -> String) -> String {
        if let legacy = error as? LegacyChannelError { return legacy.message }
        if isLegacyPublisherGroupError(error) { return channelMessage }
        return fallback(error)
    }
}

/// Which of the two retired-v1 surfaces a ``LegacyChannelError`` describes.
public enum LegacyChannelKind: Equatable {
    /// A channel whose persisted MLS state predates the v2 derivation.
    case channel
    /// A `/g/` co-publisher link minted under the v1 derivation.
    case inviteLink
}

/// A retired-v1 refusal, already carrying its user-facing message.
///
/// Thrown at the boundaries where the condition is actually detectable, so
/// every UI call site discriminates on the *type* rather than re-doing a
/// string match.
public struct LegacyChannelError: Error, LocalizedError, Equatable {
    /// Which surface refused.
    public let kind: LegacyChannelKind

    public init(kind: LegacyChannelKind) {
        self.kind = kind
    }

    /// The user-facing copy for this refusal.
    public var message: String {
        switch kind {
        case .channel: return LegacyChannelV1.channelMessage
        case .inviteLink: return LegacyChannelV1.coPublisherLinkMessage
        }
    }

    public var errorDescription: String? { message }
}
