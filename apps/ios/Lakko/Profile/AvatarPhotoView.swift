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
    /// ADR-0087 section 18 / UX 4.5.5 — a group's photo, by its group id
    /// (lowercase hex), when this device holds it.
    case group(String)
    /// A photo named directly (a public group's or channel's record, found by
    /// search), not yet held by any conversation on this device.
    case avatar(ProfileAvatar)
}

/// ADR-0087 section 18 — the group photos the avatar component draws, for the
/// signed-in account. The repository refreshes it whenever a group setting
/// changes.
@MainActor
final class GroupPhotoDirectory: ObservableObject {
    static let shared = GroupPhotoDirectory()

    @Published private(set) var revision: UInt64 = 0
    private var photos: [String: ProfileAvatar] = [:]

    func avatar(groupIdHex: String) -> ProfileAvatar? { photos[groupIdHex.lowercased()] }

    /// Replace the whole map (the store is small: one row per group).
    func replace(_ next: [String: ProfileAvatar]) {
        let lowered = Dictionary(next.map { ($0.key.lowercased(), $0.value) }, uniquingKeysWith: { a, _ in a })
        guard lowered != photos else { return }
        photos = lowered
        revision &+= 1
    }
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
    @ObservedObject private var groupPhotos = GroupPhotoDirectory.shared

    var body: some View {
        // Reading both revisions subscribes this view to a new profile and to
        // a picture arriving.
        let _ = directory.revision
        let _ = images.revision
        let _ = groupPhotos.revision
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
        case .group(let groupIdHex): return groupPhotos.avatar(groupIdHex: groupIdHex)
        case .avatar(let avatar): return avatar
        }
    }
}

// The muted, looped player is the media one (``LoopingVideoView``).
