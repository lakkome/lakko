// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
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
        /// UX spec section 10 — an allowlisted image with no `kind`.
        case photo
        /// UX spec section 10 — `video/mp4` (the allowlisted video class) with
        /// no `kind`.
        case video
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
        // A plain photo or video carries no kind; an unknown kind stays a file
        // so a future shape is never guessed at.
        case (nil, .image?):
            return Decision(bubble: .photo, inline: inline)
        case (nil, .video?):
            return Decision(bubble: .video, inline: inline)
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

    // MARK: - Photo and video bubble size (UX spec section 10)

    public static let mediaMaxWidth: Double = 260
    public static let mediaMaxHeight: Double = 360
    /// Keeps an extreme panorama or a sliver still tappable.
    public static let mediaMinSide: Double = 72
    /// Sender side: the `width` / `height` carried on the wire fit this box.
    public static let maxWireDimension: UInt32 = 4096

    /// The bubble box for a photo or video, known before any byte downloads:
    /// the attachment's `width` / `height`, else the thumb's, else 4:3; fit
    /// into 260 x 360 with the aspect preserved.
    public static func bubbleSize(
        width: UInt32?,
        height: UInt32?,
        thumbWidth: UInt32? = nil,
        thumbHeight: UInt32? = nil
    ) -> CGSize {
        let aspect: Double
        if let w = width, let h = height, w > 0, h > 0 {
            aspect = Double(h) / Double(w)
        } else if let w = thumbWidth, let h = thumbHeight, w > 0, h > 0 {
            aspect = Double(h) / Double(w)
        } else {
            aspect = 3.0 / 4.0
        }
        var w = mediaMaxWidth
        var h = w * aspect
        if h > mediaMaxHeight {
            h = mediaMaxHeight
            w = h / aspect
        }
        return CGSize(width: max(mediaMinSide, min(mediaMaxWidth, w)), height: max(mediaMinSide, min(mediaMaxHeight, h)))
    }

    /// `docs/ux/feed.md` section 3 — a bubble box in a column narrower than
    /// the box: scaled down with its aspect kept, never up. `nil` (the chat,
    /// which has no column) or a width that says nothing leaves it alone.
    public static func fit(_ box: CGSize, within width: CGFloat?) -> CGSize {
        guard let width, width > 0, box.width > width else { return box }
        let scale = width / box.width
        return CGSize(width: width, height: box.height * scale)
    }

    /// The voice bubble's waveform and its time row.
    public static let voiceWaveformWidth: CGFloat = 170
    /// The play disc and the gap beside the waveform.
    public static let voiceControlsWidth: CGFloat = 48
    /// A waveform shorter than this is no longer something to aim at.
    public static let voiceWaveformMinWidth: CGFloat = 80

    /// The waveform's width in a column of `width`: what the column leaves
    /// beside the play disc, at most the chat's width.
    public static func voiceWaveformWidth(within width: CGFloat?) -> CGFloat {
        guard let width, width > 0 else { return voiceWaveformWidth }
        return max(voiceWaveformMinWidth, min(voiceWaveformWidth, width - voiceControlsWidth))
    }

    /// Sender side: a pixel size scaled down proportionally to fit
    /// ``maxWireDimension``, never up. `nil` for a zero dimension.
    public static func wireDimensions(width: Int, height: Int) -> (width: UInt32, height: UInt32)? {
        guard width > 0, height > 0 else { return nil }
        let longest = max(width, height)
        let limit = Int(maxWireDimension)
        guard longest > limit else { return (UInt32(width), UInt32(height)) }
        let scale = Double(limit) / Double(longest)
        let w = max(1, min(limit, Int((Double(width) * scale).rounded())))
        let h = max(1, min(limit, Int((Double(height) * scale).rounded())))
        return (UInt32(w), UInt32(h))
    }
}
