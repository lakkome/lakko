// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import LakkoCore
import SwiftUI
import UIKit

// `docs/ux/media-messages.md` section 11 — the photos and videos of one
// message drawn together as a mosaic, the viewer that pages through them,
// and the chat list row's thumbnail. Where each tile goes is the shared rule
// `media_mosaic` in `lakko-ui-rules` (``mediaMosaic(bubbleWidth:gap:items:)``);
// nothing here re-decides it.

/// Section 10 — the media bubble width (260 pt), narrowed by a column that is
/// narrower (the feed).
func mosaicBubbleWidth(column: CGFloat?) -> CGFloat {
    let full = CGFloat(InlineMedia.mediaMaxWidth)
    guard let column, column > 0 else { return full }
    return min(full, column)
}

/// The photos and videos of `attachments` a mosaic draws: in the order sent,
/// at most the rule's cap (section 11: a received message carrying more shows
/// the first ten and nothing of the rest).
func mosaicItems(_ attachments: [AttachmentRef]) -> [AttachmentRef] {
    let media = attachments.filter {
        let bubble = InlineMedia.decision(for: $0).bubble
        return bubble == .photo || bubble == .video
    }
    return Array(media.prefix(Int(mediaMosaicMaxItems())))
}

/// The size the rule lays an item out at: its own, else its thumbnail's,
/// else unknown (`0`, laid out as 4:3).
func mosaicSize(_ ref: AttachmentRef) -> MediaSize {
    if let w = ref.width, let h = ref.height, w > 0, h > 0 { return MediaSize(width: w, height: h) }
    if let w = ref.thumb?.width, let h = ref.thumb?.height, w > 0, h > 0 { return MediaSize(width: w, height: h) }
    return MediaSize(width: 0, height: 0)
}

/// The shared rule's layout of `refs` at the media bubble width.
func mosaicLayout(_ refs: [AttachmentRef], column: CGFloat?) -> MediaMosaic {
    let width = mosaicBubbleWidth(column: column)
    return mediaMosaic(
        bubbleWidth: UInt32(max(1, width.rounded(.down))), gap: mediaMosaicGap(), items: refs.map(mosaicSize))
}

/// Section 11 — the mosaic of one message's photos and videos.
struct MediaMosaicView: View {
    let refs: [AttachmentRef]
    /// The mosaic is the whole message: its own outer corners are rounded.
    /// Inside a bubble the bubble's clip rounds them.
    var rounded = false
    /// The time pill of a message without text, bottom end on the mosaic.
    var overlay: AnyView?

    @Environment(\.mediaColumnWidth) private var columnWidth
    @EnvironmentObject private var media: ChatMediaController
    @State private var viewer: MediaGalleryStart?

    var body: some View {
        let layout = mosaicLayout(refs, column: columnWidth)
        ZStack(alignment: .topLeading) {
            ForEach(Array(zip(refs.indices, layout.tiles)), id: \.0) { index, tile in
                MosaicTileView(
                    ref: refs[index],
                    size: CGSize(width: CGFloat(tile.width), height: CGFloat(tile.height))
                ) {
                    viewer = MediaGalleryStart(index: index)
                }
                // Measured from the start edge: `.leading` mirrors in a
                // right-to-left layout, as the rule's coordinates do.
                .padding(.leading, CGFloat(tile.x))
                .padding(.top, CGFloat(tile.y))
            }
        }
        .frame(width: CGFloat(layout.width), height: CGFloat(layout.height), alignment: .topLeading)
        .clipShape(RoundedRectangle(cornerRadius: rounded ? LakkoRadius.bubble : 0, style: .continuous))
        .overlay(alignment: .bottomTrailing) {
            if let overlay { overlay }
        }
        .fullScreenCover(item: $viewer) { start in
            MediaGalleryViewer(
                items: refs.map { MediaGalleryItem(ref: $0) },
                start: start.index,
                callActive: media.callActive,
                fetchVideo: { [media] ref in try await media.mediaFile(for: ref) })
        }
    }
}

struct MediaGalleryStart: Identifiable {
    let id = UUID()
    let index: Int
}

/// One tile: section 10's states on their own (thumb, full, failed, video
/// poster with play glyph).
struct MosaicTileView: View {
    let ref: AttachmentRef
    let size: CGSize
    let onOpen: () -> Void

    @Environment(\.displayScale) private var displayScale
    @State private var thumb: UIImage?
    @State private var full: UIImage?
    @State private var loadingFull = false
    @State private var failed = false
    @State private var failure: MediaLoadFailure = .failed
    @State private var attempt = 0

    private var isVideo: Bool { InlineMedia.decision(for: ref).bubble == .video }
    private var key: String { MediaSessionCache.key(for: ref) }

    var body: some View {
        Button(action: tap) {
            ZStack {
                Rectangle().fill(Color.lakkoSurfaceSunken)
                if let full {
                    Image(uiImage: full).resizable().scaledToFill()
                } else if let thumb {
                    Image(uiImage: thumb).resizable().scaledToFill()
                        .blur(radius: isVideo ? 0 : 6, opaque: true)
                }
                if isVideo, !failed {
                    ZStack {
                        Circle().fill(Color.black.opacity(0.5))
                        LkIconView(.playFill, size: playSide * 0.45, relativeTo: nil)
                            .foregroundStyle(Color.white)
                            .offset(x: playSide * 0.04)
                    }
                    .frame(width: playSide, height: playSide)
                }
            }
            .frame(width: size.width, height: size.height)
            .clipped()
            .overlay(alignment: .topLeading) {
                if isVideo, ref.size > 0, size.width >= 120, !failed {
                    Text(AttachmentBridge.formatSize(ref.size))
                        .font(.caption2)
                        .monospacedDigit()
                        .foregroundStyle(Color.white)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(Color.black.opacity(0.5)))
                        .padding(6)
                }
            }
            .overlay(alignment: .topTrailing) {
                if loadingFull, !failed, size.width >= 64 { CornerSpinner() }
            }
            .overlay {
                if failed {
                    MediaFailureOverlay(
                        failure: failure,
                        genericText: Localization.text(isVideo ? "chat.video.load_failed" : "chat.photo.load_failed"))
                        .scaleEffect(size.width < 120 ? 0.8 : 1)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibilityLabel)
        .task(id: "\(key)-\(attempt)") { await load() }
    }

    private var playSide: CGFloat { min(52, max(28, min(size.width, size.height) * 0.4)) }

    private var accessibilityLabel: String {
        if isVideo {
            return failed
                ? Localization.text("chat.video.load_failed_retry")
                : Localization.text("chat.video.play", texts: ["size": AttachmentBridge.formatSize(ref.size)])
        }
        if failed { return Localization.text("chat.photo.load_failed_retry") }
        return Localization.text(full == nil ? "chat.photo.loading" : "chat.media.photo")
    }

    private func tap() {
        if failed {
            if failure.retries { attempt += 1 }
            return
        }
        onOpen()
    }

    /// Photos load by themselves (thumb, then the full photo); a video shows
    /// its poster and downloads only in the viewer (section 10).
    private func load() async {
        let cache = MediaSessionCache.shared
        failed = false
        if full == nil, thumb == nil, let thumbRef = ref.thumb, let bytes = await cache.thumbData(thumbRef) {
            thumb = await cache.displayImage(
                key: MediaSessionCache.key(for: thumbRef), bytes: bytes, maxPixel: PhotoVideoPrep.thumbMaxPixel)
        }
        guard !isVideo, full == nil else { return }
        loadingFull = true
        defer { loadingFull = false }
        do {
            let bytes = try await cache.photoData(ref)
            let maxPixel = Int((max(size.width, size.height) * displayScale).rounded(.up))
            guard let image = await cache.displayImage(key: key, bytes: bytes, maxPixel: maxPixel) else {
                failed = true
                return
            }
            full = image
        } catch {
            if !Task.isCancelled {
                failure = MediaLoadFailure.of(error)
                failed = true
            }
        }
    }
}

// MARK: - Chat list row thumbnail

/// Section 11 — the 20 pt thumbnail at the start of a row's second line: the
/// first photo or video's sent thumb (corner radius 4, cropped to fill), a
/// small play glyph on a video, one or two offset edges behind it for a
/// message of several. Until the thumb is there the row's neutral
/// placeholder; a failed fetch keeps it.
struct PreviewMediaThumbnail: View {
    let media: ConversationPreviewMedia

    static let side: CGFloat = 20
    static let edgeOffset: CGFloat = 2

    @State private var image: UIImage?

    var body: some View {
        let edges = media.stackedEdges
        let off = Self.edgeOffset
        ZStack(alignment: .bottomLeading) {
            ForEach((0..<edges).reversed(), id: \.self) { step in
                let shift = off * CGFloat(step + 1)
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(Color.lakkoTextMuted.opacity(0.45))
                    .frame(width: Self.side, height: Self.side)
                    .padding(.leading, shift)
                    .padding(.bottom, shift)
            }
            ZStack {
                RoundedRectangle(cornerRadius: 4, style: .continuous).fill(Color.lakkoSurfaceSunken)
                if let image {
                    Image(uiImage: image).resizable().scaledToFill()
                        .frame(width: Self.side, height: Self.side)
                        .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
                }
                if media.isVideo {
                    LkIconView(.playFill, size: 9, relativeTo: nil)
                        .foregroundStyle(Color.white)
                        .shadow(color: .black.opacity(0.5), radius: 1)
                }
            }
            .frame(width: Self.side, height: Self.side)
        }
        .frame(
            width: Self.side + off * CGFloat(edges), height: Self.side + off * CGFloat(edges),
            alignment: .bottomLeading)
        .accessibilityHidden(true)
        .task(id: media.thumb.map(MediaSessionCache.key(for:))) {
            guard let thumb = media.thumb else { return }
            let cache = MediaSessionCache.shared
            guard let bytes = await cache.thumbData(thumb) else { return }
            image = await cache.displayImage(
                key: MediaSessionCache.key(for: thumb), bytes: bytes, maxPixel: Int(Self.side * 3))
        }
    }
}

// MARK: - Viewer

/// One page of the viewer: a sent photo or video, or a tray item that has
/// not finished uploading (its local bytes).
@MainActor
struct MediaGalleryItem: Identifiable {
    enum Source {
        case ref(AttachmentRef)
        case localPhoto(Data)
        case localVideo(URL, poster: Data?)
    }

    let id: String
    let source: Source

    init(ref: AttachmentRef) {
        self.id = MediaSessionCache.key(for: ref)
        self.source = .ref(ref)
    }

    init(id: String, source: Source) {
        self.id = id
        self.source = source
    }

    var isVideo: Bool {
        switch source {
        case .ref(let ref): return InlineMedia.decision(for: ref).bubble == .video
        case .localPhoto: return false
        case .localVideo: return true
        }
    }

    var ref: AttachmentRef? {
        if case .ref(let ref) = source { return ref }
        return nil
    }
}

/// Section 10 and 11 — the full-screen viewer over every photo and video of
/// a message (or of the tray): swipe between them, `2 / 5` at the top, Save
/// for the current one, close by the button or a swipe down. A video plays in
/// place with the platform's standard controls.
struct MediaGalleryViewer: View {
    let items: [MediaGalleryItem]
    let callActive: Bool
    let fetchVideo: (AttachmentRef) async throws -> URL

    @Environment(\.dismiss) private var dismiss
    @State private var index: Int
    @State private var exporting: ExportedFile?
    @State private var shareURLs: [String: URL] = [:]
    @State private var dragOffset: CGFloat = 0

    init(
        items: [MediaGalleryItem], start: Int, callActive: Bool,
        fetchVideo: @escaping (AttachmentRef) async throws -> URL
    ) {
        self.items = items
        self.callActive = callActive
        self.fetchVideo = fetchVideo
        _index = State(initialValue: max(0, min(start, items.count - 1)))
    }

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            TabView(selection: $index) {
                ForEach(Array(items.enumerated()), id: \.element.id) { offset, item in
                    MediaGalleryPage(
                        item: item, isCurrent: offset == index, callActive: callActive,
                        fetchVideo: fetchVideo, onDismiss: { dismiss() },
                        onShareURL: { url in shareURLs[item.id] = url })
                        .tag(offset)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .ignoresSafeArea()
            bar
        }
        .fileExport($exporting)
        .onDisappear {
            for url in shareURLs.values { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        }
    }

    private var bar: some View {
        HStack {
            Button { dismiss() } label: {
                LkIconView(.xmark, size: 20).frame(width: 44, height: 44)
            }
            .accessibilityLabel(Localization.text("common.close"))
            Spacer()
            if let item = items[safe: index], let url = shareURLs[item.id] {
                Button {
                    exporting = ExportedFile(url: url)
                } label: {
                    Label(Localization.text("common.save"), icon: .download)
                        .font(.body.weight(.medium))
                        .frame(minHeight: 44)
                }
            }
        }
        .overlay {
            if items.count > 1 {
                Text(Localization.text("chat.viewer.position", texts: [
                    "n": (index + 1).formatted(.number.locale(Localization.shared.locale)),
                    "total": items.count.formatted(.number.locale(Localization.shared.locale)),
                ]))
                .font(.subheadline.weight(.semibold))
                .monospacedDigit()
            }
        }
        .foregroundStyle(Color.white)
        .padding(.horizontal, 8)
        .background(
            LinearGradient(colors: [Color.black.opacity(0.55), .clear], startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea())
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}

/// One page of ``MediaGalleryViewer``.
private struct MediaGalleryPage: View {
    let item: MediaGalleryItem
    let isCurrent: Bool
    let callActive: Bool
    let fetchVideo: (AttachmentRef) async throws -> URL
    let onDismiss: () -> Void
    let onShareURL: (URL) -> Void

    @State private var image: UIImage?
    @State private var photoData: Data?
    @State private var poster: UIImage?
    @State private var failure: MediaLoadFailure?
    @State private var attempt = 0
    @State private var player: AVPlayer?
    @State private var loadingVideo = false
    @State private var audioStarted = false

    private var isGif: Bool {
        guard let ref = item.ref else { return false }
        return (ref.mime ?? "").lowercased().hasPrefix("image/gif")
    }

    var body: some View {
        ZStack {
            if item.isVideo {
                videoBody
            } else {
                photoBody
            }
            if let failure {
                Button { attempt += 1 } label: {
                    MediaFailureOverlay(
                        failure: failure,
                        genericText: Localization.text(item.isVideo ? "chat.video.load_failed" : "chat.photo.load_failed"))
                }
                .buttonStyle(.plain)
                .disabled(!failure.retries)
            }
        }
        .task(id: attempt) { await load() }
        .onChange(of: isCurrent) { _, current in
            if !current { stopVideo() }
        }
        .onDisappear { stopVideo() }
    }

    @ViewBuilder
    private var photoBody: some View {
        if isGif, let photoData {
            AnimatedImageView(source: .data(photoData), contentMode: .scaleAspectFit)
                .accessibilityLabel(Localization.text("chat.media.photo"))
        } else if let image {
            ZoomableImageView(image: image, onDismiss: onDismiss)
                .accessibilityLabel(Localization.text("chat.media.photo"))
                .accessibilityHint(Localization.text("chat.photo.viewer_hint"))
        } else if failure == nil {
            ProgressView().tint(Color.white)
        }
    }

    @ViewBuilder
    private var videoBody: some View {
        if let player {
            PlayerControllerView(player: player)
                .padding(.top, 52)
        } else {
            ZStack {
                if let poster {
                    Image(uiImage: poster).resizable().scaledToFit()
                }
                if failure == nil {
                    Button(action: playVideo) {
                        ZStack {
                            Circle().fill(Color.black.opacity(0.5))
                            if loadingVideo {
                                ProgressView().tint(Color.white)
                            } else {
                                LkIconView(.playFill, size: 30, relativeTo: .title2)
                                    .foregroundStyle(Color.white)
                                    .offset(x: 2)
                            }
                        }
                        .frame(width: 64, height: 64)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(Localization.text("chat.video.play", texts: [
                        "size": AttachmentBridge.formatSize(item.ref?.size ?? 0),
                    ]))
                }
            }
            .gesture(
                DragGesture().onEnded { value in
                    if value.translation.height > 120, abs(value.translation.width) < 80 { onDismiss() }
                }
            )
        }
    }

    private func load() async {
        failure = nil
        let cache = MediaSessionCache.shared
        switch item.source {
        case .localPhoto(let data):
            photoData = data
            image = await cache.displayImage(
                key: "tray-\(item.id)", bytes: data, maxPixel: Int(InlineMedia.maxWireDimension))
        case .localVideo(_, let posterData):
            if let posterData { poster = UIImage(data: posterData) }
        case .ref(let ref):
            if item.isVideo {
                if poster == nil, let thumb = ref.thumb, let bytes = await cache.thumbData(thumb) {
                    poster = await cache.displayImage(
                        key: MediaSessionCache.key(for: thumb), bytes: bytes, maxPixel: PhotoVideoPrep.thumbMaxPixel)
                }
                if let local = cache.videoFile(MediaSessionCache.key(for: ref)),
                   let share = MediaShareCopy.link(local, ref: ref) {
                    onShareURL(share)
                }
                return
            }
            do {
                let bytes = try await cache.photoData(ref)
                photoData = bytes
                image = await cache.displayImage(
                    key: MediaSessionCache.key(for: ref), bytes: bytes, maxPixel: Int(InlineMedia.maxWireDimension))
                if image == nil, !isGif { failure = .failed }
                if let share = MediaShareCopy.write(bytes, ref: ref) { onShareURL(share) }
            } catch {
                if !Task.isCancelled { failure = MediaLoadFailure.of(error) }
            }
        }
    }

    private func playVideo() {
        guard !loadingVideo else { return }
        switch item.source {
        case .localVideo(let url, _):
            start(url)
        case .ref(let ref):
            loadingVideo = true
            Task {
                defer { loadingVideo = false }
                do {
                    let url = try await fetchVideo(ref)
                    if let share = MediaShareCopy.link(url, ref: ref) { onShareURL(share) }
                    start(url)
                } catch {
                    failure = MediaLoadFailure.of(error)
                }
            }
        case .localPhoto:
            break
        }
    }

    private func start(_ url: URL) {
        // The file is named after the sender's choice, so the container type
        // is stated rather than guessed from the extension.
        let asset = AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"])
        let player = AVPlayer(playerItem: AVPlayerItem(asset: asset))
        self.player = player
        audioStarted = (try? MediaAudioSession.begin(.playback, callActive: callActive)) != nil
        player.play()
    }

    private func stopVideo() {
        player?.pause()
        player = nil
        if audioStarted {
            MediaAudioSession.end(.playback)
            audioStarted = false
        }
    }
}
