// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// ADR-0084 — whose profile photo an avatar circle shows.
enum AvatarPhoto: Equatable {
    /// The person holding this 32-byte signature key (a member, a contact).
    case person(Data)
    /// This account's own published photo.
    case own
}

/// The photo half of ``InitialsAvatar``: the person's still (or, in a large
/// view, their video, muted and looped) when this device holds it, else the
/// initials circle `fallback`. No spinner: the picture replaces the initials
/// when it arrives (UX spec section 7).
struct AvatarPhotoView<Fallback: View>: View {
    let photo: AvatarPhoto
    let size: CGFloat
    let playsVideo: Bool
    @ViewBuilder let fallback: () -> Fallback

    @ObservedObject private var images = AvatarImages.shared
    @ObservedObject private var directory = ProfileDirectory.shared

    var body: some View {
        // Reading both revisions subscribes this view to a new profile and to
        // a picture arriving.
        let _ = directory.revision
        let _ = images.revision
        if let avatar = resolvedAvatar {
            if playsVideo, let url = images.videoURL(avatar) {
                LoopingVideoView(url: url)
                    .frame(width: size, height: size)
                    .clipShape(Circle())
                    .accessibilityHidden(true)
            } else if let image = images.still(avatar) {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
                    .frame(width: size, height: size)
                    .clipShape(Circle())
                    .accessibilityHidden(true)
            } else {
                fallback()
            }
        } else {
            fallback()
        }
    }

    private var resolvedAvatar: ProfileAvatar? {
        switch photo {
        case .person(let key): return directory.avatar(signatureKey: key)
        case .own: return directory.ownProfile?.avatar
        }
    }
}

// The muted, looped player is the media one (``LoopingVideoView``).
