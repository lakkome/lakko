// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// UX `saved-messages-and-file-lifetime.md` section 4 — why a file did not
/// open. One state for every media kind (photo, video, voice message, video
/// note, GIF, file).
public enum MediaLoadFailure: Equatable, Sendable {
    /// The file is no longer on the server (UX 4.1): no retry helps.
    case gone
    /// The opening allowance is used up (UX 4.2): try again later.
    case limited
    /// Anything else: the existing failure with retry.
    case failed

    public static func of(_ error: Error) -> MediaLoadFailure {
        switch error {
        case AttachmentBridgeError.missing: return .gone
        case AttachmentBridgeError.readLimited: return .limited
        case StorageClientError.limited(.read): return .limited
        default: return .failed
        }
    }

    /// Whether tapping the media may try again.
    public var retries: Bool { self != .gone }
}

/// The two lines of UX 4.1, in place of the media.
struct MediaGoneNotice: View {
    var onDark = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(Localization.text("chat.attachment.gone.title"))
                .font(.caption.weight(.semibold))
            Text(Localization.text("chat.attachment.gone.body"))
                .font(.caption2)
                .foregroundStyle(onDark ? Color.white.opacity(0.8) : Color.lakkoTextMuted)
        }
        .foregroundStyle(onDark ? Color.white : Color.lakkoText)
        .accessibilityElement(children: .combine)
    }
}

/// UX 4.2 opening limit: the line and `Try again`.
struct MediaLimitNotice: View {
    let onRetry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(Localization.text("chat.attachment.limit.open"))
                .font(.caption)
            Button(Localization.text("common.try_again"), action: onRetry)
                .font(.caption.weight(.semibold))
                .buttonStyle(.plain)
                .foregroundStyle(Color.lakkoAccent)
        }
    }
}

/// The inline failure line under a voice message, a video note or a GIF:
/// gone, limited, or the kind's own `generic` line.
struct MediaFailureLine<Generic: View>: View {
    let failure: MediaLoadFailure
    let onRetry: () -> Void
    @ViewBuilder let generic: () -> Generic

    var body: some View {
        switch failure {
        case .gone: MediaGoneNotice()
        case .limited: MediaLimitNotice(onRetry: onRetry)
        case .failed: generic()
        }
    }
}
