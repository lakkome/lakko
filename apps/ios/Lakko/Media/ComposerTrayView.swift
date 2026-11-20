// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

/// `docs/ux/media-messages.md` section 11 — the strip directly above the
/// message field: a 64 pt thumbnail per photo or video (a video's duration
/// on a pill), a 64 pt chip per file, an (x) on every item, a thin progress
/// ring while an item uploads and a retry glyph when it failed. Tapping a
/// photo or video opens the viewer over the tray's photos and videos.
/// Hidden when empty (the caller does not mount it).
struct ComposerTrayView: View {
    @ObservedObject var store: ComposerTrayStore
    /// Whether a call is live (the viewer's audio session).
    var callActive = false
    /// Plays a video in the viewer that is already sent.
    var fetchVideo: (AttachmentRef) async throws -> URL = { _ in throw MediaCaptureError.startFailed }

    @ObservedObject private var l10n = Localization.shared
    @State private var viewer: MediaGalleryStart?
    @State private var viewerItems: [MediaGalleryItem] = []

    static let side: CGFloat = 64

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !store.tray.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(store.tray.items) { item in
                            cell(item)
                        }
                    }
                    // Room for the (x) that sits on the corner.
                    .padding(.top, 6)
                    .padding(.horizontal, 2)
                }
            }
            if let notice = store.limitNotice {
                Text(l10n.t(notice == .media ? "chat.tray.limit_media" : "chat.tray.limit_files"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.15), value: store.limitNotice)
        .fullScreenCover(item: $viewer) { start in
            MediaGalleryViewer(
                items: viewerItems, start: start.index, callActive: callActive, fetchVideo: fetchVideo)
        }
    }

    @ViewBuilder
    private func cell(_ item: ComposerTrayItem) -> some View {
        ZStack(alignment: .topTrailing) {
            Group {
                if item.isMedia {
                    mediaCell(item)
                } else {
                    fileCell(item)
                }
            }
            .padding(.top, 4)
            .padding(.trailing, 4)
            Button { store.remove(item.id) } label: {
                LkIconView(.xmarkCircleFill, size: 20, relativeTo: nil)
                    .foregroundStyle(Color.white)
                    .background(Circle().fill(Color.black.opacity(0.6)).padding(1))
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .offset(x: 4, y: -4)
            .accessibilityLabel(l10n.t("chat.tray.remove", texts: ["name": displayName(item)]))
        }
    }

    private func mediaCell(_ item: ComposerTrayItem) -> some View {
        Button {
            if item.status == .failed {
                store.retry(item.id)
            } else {
                open(item)
            }
        } label: {
            ZStack {
                RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.lakkoSurfaceSunken)
                if let data = item.thumbnail, let image = UIImage(data: data) {
                    Image(uiImage: image).resizable().scaledToFill()
                        .frame(width: Self.side, height: Self.side)
                }
                statusMark(item)
            }
            .frame(width: Self.side, height: Self.side)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(alignment: .bottomLeading) {
                if item.kind == .video, let ms = item.durationMs {
                    Text(Self.duration(ms))
                        .font(.caption2.weight(.medium))
                        .monospacedDigit()
                        .foregroundStyle(Color.white)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 1)
                        .background(Capsule().fill(Color.black.opacity(0.55)))
                        .padding(4)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            item.status == .failed
                ? l10n.t("chat.tray.retry", texts: ["name": displayName(item)])
                : displayName(item))
    }

    private func fileCell(_ item: ComposerTrayItem) -> some View {
        Button {
            if item.status == .failed { store.retry(item.id) }
        } label: {
            HStack(spacing: 8) {
                ZStack {
                    if item.isBusy || item.status == .failed {
                        statusMark(item, onSurface: true)
                    } else {
                        LkIconView(.doc, size: 22, relativeTo: nil)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .frame(width: 28, height: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(displayName(item))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if item.size > 0 {
                        Text(AttachmentBridge.formatSize(item.size))
                            .font(.caption2)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .frame(maxWidth: 140, alignment: .leading)
            }
            .padding(.horizontal, 10)
            .frame(height: Self.side)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.lakkoSurfaceSunken))
        }
        .buttonStyle(.plain)
        // Only a failed file does something on a tap; the rest keep their
        // full colours rather than the dimmed look of a disabled button.
        .allowsHitTesting(item.status == .failed)
        .accessibilityLabel(
            item.status == .failed
                ? l10n.t("chat.tray.retry", texts: ["name": displayName(item)])
                : displayName(item))
    }

    /// The thin progress ring while an item uploads; the retry glyph when it
    /// failed.
    @ViewBuilder
    private func statusMark(_ item: ComposerTrayItem, onSurface: Bool = false) -> some View {
        let tint = onSurface ? Color.lakkoAccent : Color.white
        switch item.status {
        case .preparing:
            ProgressView()
                .controlSize(.small)
                .tint(tint)
                .padding(onSurface ? 0 : 6)
                .background(Circle().fill(Color.black.opacity(onSurface ? 0 : 0.45)))
        case .uploading(let fraction):
            ProgressRing(fraction: fraction, lineWidth: 2.5, color: tint)
                .frame(width: 24, height: 24)
                .padding(onSurface ? 0 : 4)
                .background(Circle().fill(Color.black.opacity(onSurface ? 0 : 0.45)))
        case .failed:
            LkIconView(.arrowClockwise, size: 18, relativeTo: nil)
                .foregroundStyle(onSurface ? Color.lakkoDanger : Color.white)
                .frame(width: 30, height: 30)
                .background(Circle().fill(onSurface ? Color.clear : Color.lakkoDanger.opacity(0.85)))
        case .ready:
            EmptyView()
        }
    }

    private func displayName(_ item: ComposerTrayItem) -> String {
        switch item.kind {
        case .photo: return l10n.t("chat.media.photo")
        case .video: return l10n.t("chat.media.video")
        case .file: return item.name ?? l10n.t("chat.media.file")
        }
    }

    private func open(_ item: ComposerTrayItem) {
        let items = store.galleryItems()
        let id = item.ref.map(MediaSessionCache.key(for:)) ?? item.id.uuidString
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        viewerItems = items
        viewer = MediaGalleryStart(index: index)
    }

    /// `m:ss`, the duration pill.
    static func duration(_ ms: UInt32) -> String {
        let total = Int((Double(ms) / 1000).rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}
