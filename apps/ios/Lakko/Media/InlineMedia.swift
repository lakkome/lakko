// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0047 §1 — how a received attachment is rendered, decided in one place.
/// Mirror of `apps/web/src/media/inline-media.ts`.
///
/// Two inputs, two trust levels, and the rule that keeps them apart:
///
///   * `kind` is **sender-chosen** — exactly as trustworthy as the file name.
///     It picks the bubble SHAPE (a voice player, a round video, an animation)
///     and nothing else.
///   * Whether the bytes are handed to a player or an image decoder at all is
///     decided by the MIME type alone, through the core's allowlist
///     (`inlineMediaType(mime:)` in `lakko-attachments`). A `kind: "voice"`
///     carrying `mime: "text/html"` is a file chip.
///
/// So a media bubble needs BOTH: the kind asks for the shape, and the
/// allowlisted MIME type must be of the matching media class. Anything else is
/// a file chip, which may still preview inline if its MIME is allowlisted.
public enum InlineMedia {

    public enum Bubble: Equatable {
        case voice
        case videoNote
        case gif
        case file
    }

    public struct Decision: Equatable {
        public let bubble: Bubble
        /// The allowlisted media class, independent of the bubble.
        public let inline: InlineMediaType?
    }

    /// The core's allowlist. Injected so the decision is testable with a fake.
    public typealias Allowlist = (String) -> InlineMediaType?

    public static let coreAllowlist: Allowlist = { inlineMediaType(mime: $0) }

    /// The allowlisted media class of `mime`, or `nil`. Fails closed: an
    /// absent or empty MIME is never inline.
    public static func inlineType(
        mime: String?,
        allowlist: Allowlist = coreAllowlist
    ) -> InlineMediaType? {
        guard let mime, !mime.isEmpty else { return nil }
        return allowlist(mime)
    }

    public static func decision(
        kind: String?,
        mime: String?,
        allowlist: Allowlist = coreAllowlist
    ) -> Decision {
        let inline = inlineType(mime: mime, allowlist: allowlist)
        switch (kind, inline) {
        case ("voice", .audio?):
            return Decision(bubble: .voice, inline: inline)
        case ("video_note", .video?):
            return Decision(bubble: .videoNote, inline: inline)
        case ("gif", .image?), ("gif", .video?):
            return Decision(bubble: .gif, inline: inline)
        default:
            return Decision(bubble: .file, inline: inline)
        }
    }

    public static func decision(
        for ref: AttachmentRef,
        allowlist: Allowlist = coreAllowlist
    ) -> Decision {
        decision(kind: ref.kind, mime: ref.mime, allowlist: allowlist)
    }

    /// Whether a poster thumb may be decoded as an image at all.
    public static func isImageThumb(_ thumb: ThumbRef, allowlist: Allowlist = coreAllowlist) -> Bool {
        inlineType(mime: thumb.mime, allowlist: allowlist) == .image
    }
}
