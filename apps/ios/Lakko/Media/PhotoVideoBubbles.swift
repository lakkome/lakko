// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import AVKit
import LakkoCore
import SwiftUI
import UIKit

/// UX spec section 10 — photos and videos in the timeline, and their
/// full-screen viewers.
///
/// A bubble is chosen by ``InlineMedia/decision(for:allowlist:)``: an
/// allowlisted image or `video/mp4` with no `kind`. Its box comes from the
/// attachment's `width` / `height` before anything downloads. Bytes are
/// fetched through ``MediaSessionCache`` (photos, thumbs) or
/// ``ChatMediaController/mediaFile(for:)`` (videos), both of which only return
/// bytes the AEAD open authenticated.

private let mediaCorner = LakkoRadius.bubble

private struct MediaFailedLabel: View {
    let text: String

    var body: some View {
        VStack(spacing: 4) {
            Image(systemName: "arrow.clockwise")
                .font(.body.weight(.semibold))
            Text(text)
                .font(.caption)
        }
        .foregroundStyle(Color.white)
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.black.opacity(0.55)))
    }
}

/// ADR-0065 / UX 4.1, 4.2 — what a photo or video shows when it did not
/// open: the gone notice, the limit line, or the kind's own failure.
private struct MediaFailureOverlay: View {
    let failure: MediaLoadFailure
    let genericText: String

    var body: some View {
        switch failure {
        case .gone:
            MediaGoneNotice(onDark: true)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.black.opacity(0.55)))
                .padding(8)
        case .limited:
            MediaFailedLabel(text: Localization.text("chat.attachment.limit.open"))
        case .failed:
            MediaFailedLabel(text: genericText)
        }
    }
}

private struct CornerSpinner: View {
    var body: some View {
        ProgressView()
            .controlSize(.small)
            .tint(Color.white)
            .padding(6)
            .background(Circle().fill(Color.black.opacity(0.5)))
            .padding(8)
            .accessibilityHidden(true)
    }
}

// MARK: - Photo

struct PhotoBubble: View {
    let ref: AttachmentRef
    /// The time pill of a message that is only this media.
    let overlay: AnyView?

    init(ref: AttachmentRef, overlay: AnyView? = nil) {
        self.ref = ref
        self.overlay = overlay
    }

    @Environment(\.displayScale) private var displayScale
    @State private var thumb: UIImage?
    @State private var full: UIImage?
    @State private var fullData: Data?
    @State private var loadingFull = false
    @State private var failed = false
    @State private var failure: MediaLoadFailure = .failed
    @State private var attempt = 0
    @State private var viewerOpen = false

    private var size: CGSize {
        InlineMedia.bubbleSize(
            width: ref.width,
            height: ref.height,
            thumbWidth: ref.thumb?.width,
            thumbHeight: ref.thumb?.height
        )
    }

    private var key: String { MediaSessionCache.key(for: ref) }

    private var isGif: Bool {
        InlineMedia.inlineType(mime: ref.mime) == .image
            && (ref.mime ?? "").lowercased().hasPrefix("image/gif")
    }

    var body: some View {
        Button(action: tap) {
            ZStack {
                Rectangle().fill(Color.lakkoSurfaceSunken)
                if let fullData, isGif {
                    AnimatedImageView(source: .data(fullData))
                } else if let full {
                    Image(uiImage: full)
                        .resizable()
                        .scaledToFill()
                } else if let thumb {
                    Image(uiImage: thumb)
                        .resizable()
                        .scaledToFill()
                        .blur(radius: 6, opaque: true)
                }
            }
            .frame(width: size.width, height: size.height)
            .clipShape(RoundedRectangle(cornerRadius: mediaCorner, style: .continuous))
            .overlay(alignment: .topTrailing) {
                if loadingFull, !failed { CornerSpinner() }
            }
            .overlay {
                if failed {
                    MediaFailureOverlay(failure: failure, genericText: Localization.text("chat.photo.load_failed"))
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if let overlay { overlay }
            }
            .contentShape(RoundedRectangle(cornerRadius: mediaCorner, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            failed
                ? Localization.text("chat.photo.load_failed_retry")
                : Localization.text(full == nil ? "chat.photo.loading" : "chat.media.photo"))
        .accessibilityHint(failed || full == nil ? "" : Localization.text("chat.media.open_full_hint"))
        .task(id: "\(key)-\(attempt)") { await load() }
        .fullScreenCover(isPresented: $viewerOpen) {
            if let fullData {
                PhotoViewer(ref: ref, data: fullData, animated: isGif)
            }
        }
    }

    private func tap() {
        if failed {
            if failure.retries { attempt += 1 }
        } else if fullData != nil {
            viewerOpen = true
        }
    }

    private func load() async {
        let cache = MediaSessionCache.shared
        failed = false
        if full == nil, thumb == nil, let thumbRef = ref.thumb,
           let bytes = await cache.thumbData(thumbRef) {
            thumb = await cache.displayImage(
                key: MediaSessionCache.key(for: thumbRef),
                bytes: bytes,
                maxPixel: PhotoVideoPrep.thumbMaxPixel
            )
        }
        guard full == nil else { return }
        loadingFull = true
        defer { loadingFull = false }
        do {
            let bytes = try await cache.photoData(ref)
            let maxPixel = Int((max(size.width, size.height) * displayScale).rounded(.up))
            guard let image = await cache.displayImage(key: key, bytes: bytes, maxPixel: maxPixel) else {
                failed = true
                return
            }
            fullData = bytes
            full = image
        } catch {
            if !Task.isCancelled {
                failure = MediaLoadFailure.of(error)
                failed = true
            }
        }
    }
}

// MARK: - Video

struct VideoBubble: View {
    let ref: AttachmentRef
    /// The time pill of a message that is only this media.
    let overlay: AnyView?

    init(ref: AttachmentRef, overlay: AnyView? = nil) {
        self.ref = ref
        self.overlay = overlay
    }

    @EnvironmentObject private var media: ChatMediaController
    @State private var poster: UIImage?
    @State private var downloading = false
    @State private var failed = false
    @State private var failure: MediaLoadFailure = .failed
    @State private var playing: PlayableVideo?

    struct PlayableVideo: Identifiable {
        let id = UUID()
        let url: URL
    }

    private var size: CGSize {
        InlineMedia.bubbleSize(
            width: ref.width,
            height: ref.height,
            thumbWidth: ref.thumb?.width,
            thumbHeight: ref.thumb?.height
        )
    }

    private var key: String { MediaSessionCache.key(for: ref) }

    var body: some View {
        Button(action: tap) {
            ZStack {
                Rectangle().fill(Color.lakkoSurfaceSunken)
                if let poster {
                    Image(uiImage: poster)
                        .resizable()
                        .scaledToFill()
                }
                ZStack {
                    Circle().fill(Color.black.opacity(0.5))
                    if downloading {
                        ProgressView().tint(Color.white)
                    } else {
                        Image(systemName: "play.fill")
                            .font(.title2)
                            .foregroundStyle(Color.white)
                            .offset(x: 2)
                    }
                }
                .frame(width: 52, height: 52)
                .opacity(failed ? 0 : 1)
            }
            .frame(width: size.width, height: size.height)
            .clipShape(RoundedRectangle(cornerRadius: mediaCorner, style: .continuous))
            .overlay(alignment: .topLeading) {
                if ref.size > 0 {
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
            .overlay {
                if failed {
                    MediaFailureOverlay(failure: failure, genericText: Localization.text("chat.video.load_failed"))
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if let overlay { overlay }
            }
            .contentShape(RoundedRectangle(cornerRadius: mediaCorner, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibilityLabel)
        .task(id: key) { await loadPoster() }
        .fullScreenCover(item: $playing) { video in
            VideoPlayerScreen(ref: ref, url: video.url, callActive: media.callActive)
        }
    }

    private var accessibilityLabel: String {
        let size = AttachmentBridge.formatSize(ref.size)
        if failed { return Localization.text("chat.video.load_failed_retry") }
        if downloading { return Localization.text("chat.video.loading", texts: ["size": size]) }
        return Localization.text("chat.video.play", texts: ["size": size])
    }

    private func loadPoster() async {
        guard poster == nil, let thumbRef = ref.thumb else { return }
        let cache = MediaSessionCache.shared
        guard let bytes = await cache.thumbData(thumbRef) else { return }
        poster = await cache.displayImage(
            key: MediaSessionCache.key(for: thumbRef),
            bytes: bytes,
            maxPixel: PhotoVideoPrep.thumbMaxPixel
        )
    }

    /// Videos download only on tap (UX spec section 10).
    private func tap() {
        guard !downloading, !(failed && !failure.retries) else { return }
        failed = false
        if let local = MediaSessionCache.shared.videoFile(key) {
            playing = PlayableVideo(url: local)
            return
        }
        downloading = true
        Task {
            defer { downloading = false }
            do {
                let url = try await media.mediaFile(for: ref)
                playing = PlayableVideo(url: url)
            } catch {
                failure = MediaLoadFailure.of(error)
                failed = true
            }
        }
    }
}

// MARK: - Save

/// A copy of a media file under a per-share directory with a name the share
/// sheet recognises ("Save Image" / "Save Video" need the right extension).
/// The name is derived from the sender-chosen `name` through
/// ``AttachmentBridge/safeFileName(_:fallback:)``.
private enum MediaShareCopy {
    static func extensionFor(mime: String?) -> String {
        switch (mime ?? "").lowercased().split(separator: ";").first.map(String.init) ?? "" {
        case "image/png": return "png"
        case "image/gif": return "gif"
        case "image/webp": return "webp"
        case "video/mp4": return "mp4"
        default: return "jpg"
        }
    }

    static func directory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("shared-attachments", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    static func fileName(ref: AttachmentRef, fallback: String) -> String {
        let ext = extensionFor(mime: ref.mime)
        var name = AttachmentBridge.safeFileName(ref.name, fallback: "\(fallback).\(ext)")
        if (name as NSString).pathExtension.lowercased() != ext {
            name = (name as NSString).deletingPathExtension
            if name.isEmpty { name = fallback }
            name += ".\(ext)"
        }
        return name
    }

    static func write(_ data: Data, ref: AttachmentRef) -> URL? {
        guard let dir = try? directory() else { return nil }
        let url = dir.appendingPathComponent(fileName(ref: ref, fallback: "photo"))
        do {
            try data.write(to: url, options: [.atomic, .completeFileProtection])
            return url
        } catch {
            return nil
        }
    }

    static func link(_ source: URL, ref: AttachmentRef) -> URL? {
        guard let dir = try? directory() else { return nil }
        let url = dir.appendingPathComponent(fileName(ref: ref, fallback: "video"))
        if (try? FileManager.default.linkItem(at: source, to: url)) != nil { return url }
        if (try? FileManager.default.copyItem(at: source, to: url)) != nil { return url }
        return nil
    }
}

private struct ViewerBar: View {
    let shareURL: URL?
    let onClose: () -> Void
    /// The file the system's save picker is up for. A file leaving the app
    /// goes through that picker, never the share sheet.
    @State private var exporting: ExportedFile?

    var body: some View {
        HStack {
            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.body.weight(.semibold))
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel(Localization.text("common.close"))
            Spacer()
            if let shareURL {
                Button {
                    exporting = ExportedFile(url: shareURL)
                } label: {
                    Label(Localization.text("common.save"), systemImage: "square.and.arrow.down")
                        .font(.body.weight(.medium))
                        .frame(minHeight: 44)
                }
            }
        }
        .foregroundStyle(Color.white)
        .padding(.horizontal, 8)
        .fileExport($exporting)
    }
}

// MARK: - Photo viewer

struct PhotoViewer: View {
    let ref: AttachmentRef
    let data: Data
    let animated: Bool

    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var shareURL: URL?
    @State private var dragOffset: CGFloat = 0

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            if animated {
                AnimatedImageView(source: .data(data), contentMode: .scaleAspectFit)
                    .ignoresSafeArea()
                    .offset(y: dragOffset)
                    .gesture(
                        DragGesture()
                            .onChanged { dragOffset = max(0, $0.translation.height) }
                            .onEnded { value in
                                if value.translation.height > 120 { dismiss() } else { withAnimation { dragOffset = 0 } }
                            }
                    )
                    .accessibilityLabel(Localization.text("chat.media.photo"))
            } else if let image {
                ZoomableImageView(image: image, onDismiss: { dismiss() })
                    .ignoresSafeArea()
                    .accessibilityLabel(Localization.text("chat.media.photo"))
                    .accessibilityHint(Localization.text("chat.photo.viewer_hint"))
            } else {
                ProgressView().tint(Color.white)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            ViewerBar(shareURL: shareURL) { dismiss() }
        }
        .task {
            if !animated {
                image = await MediaSessionCache.shared.displayImage(
                    key: MediaSessionCache.key(for: ref),
                    bytes: data,
                    maxPixel: Int(InlineMedia.maxWireDimension)
                )
            }
            shareURL = MediaShareCopy.write(data, ref: ref)
        }
        .onDisappear {
            if let shareURL { try? FileManager.default.removeItem(at: shareURL.deletingLastPathComponent()) }
        }
    }
}

/// Pinch and double-tap zoom in a `UIScrollView`; a downward swipe at the
/// resting zoom closes the viewer.
struct ZoomableImageView: UIViewRepresentable {
    let image: UIImage
    let onDismiss: () -> Void

    func makeUIView(context: Context) -> ZoomScrollView {
        ZoomScrollView(image: image, onDismiss: onDismiss)
    }

    func updateUIView(_ view: ZoomScrollView, context: Context) {
        view.onDismiss = onDismiss
    }

    final class ZoomScrollView: UIScrollView, UIScrollViewDelegate, UIGestureRecognizerDelegate {
        private let imageView = UIImageView()
        var onDismiss: () -> Void
        private var lastBounds: CGSize = .zero
        private lazy var dismissPan = UIPanGestureRecognizer(target: self, action: #selector(handleDismissPan(_:)))

        init(image: UIImage, onDismiss: @escaping () -> Void) {
            self.onDismiss = onDismiss
            super.init(frame: .zero)
            delegate = self
            backgroundColor = .clear
            showsVerticalScrollIndicator = false
            showsHorizontalScrollIndicator = false
            contentInsetAdjustmentBehavior = .never
            decelerationRate = .fast
            minimumZoomScale = 1
            maximumZoomScale = 4
            imageView.image = image
            imageView.contentMode = .scaleAspectFit
            addSubview(imageView)
            let doubleTap = UITapGestureRecognizer(target: self, action: #selector(handleDoubleTap(_:)))
            doubleTap.numberOfTapsRequired = 2
            addGestureRecognizer(doubleTap)
            dismissPan.delegate = self
            addGestureRecognizer(dismissPan)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { nil }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.size != lastBounds, bounds.width > 0, bounds.height > 0 else { return }
            lastBounds = bounds.size
            setZoomScale(1, animated: false)
            guard let size = imageView.image?.size, size.width > 0, size.height > 0 else { return }
            let scale = min(bounds.width / size.width, bounds.height / size.height)
            let fitted = CGSize(width: size.width * scale, height: size.height * scale)
            imageView.frame = CGRect(origin: .zero, size: fitted)
            contentSize = fitted
            centerContent()
        }

        private func centerContent() {
            let dx = max(0, (bounds.width - contentSize.width) / 2)
            let dy = max(0, (bounds.height - contentSize.height) / 2)
            contentInset = UIEdgeInsets(top: dy, left: dx, bottom: dy, right: dx)
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }

        func scrollViewDidZoom(_ scrollView: UIScrollView) { centerContent() }

        @objc private func handleDoubleTap(_ recognizer: UITapGestureRecognizer) {
            if zoomScale > minimumZoomScale {
                setZoomScale(minimumZoomScale, animated: true)
            } else {
                let point = recognizer.location(in: imageView)
                let target: CGFloat = 2.5
                let size = CGSize(width: bounds.width / target, height: bounds.height / target)
                zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height), animated: true)
            }
        }

        @objc private func handleDismissPan(_ recognizer: UIPanGestureRecognizer) {
            let translation = recognizer.translation(in: self)
            switch recognizer.state {
            case .changed:
                imageView.transform = CGAffineTransform(translationX: 0, y: max(0, translation.y))
            case .ended, .cancelled:
                let velocity = recognizer.velocity(in: self)
                if translation.y > 120 || velocity.y > 900 {
                    onDismiss()
                } else {
                    UIView.animate(withDuration: 0.2) { self.imageView.transform = .identity }
                }
            default:
                break
            }
        }

        override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard gestureRecognizer === dismissPan else {
                return super.gestureRecognizerShouldBegin(gestureRecognizer)
            }
            guard zoomScale <= minimumZoomScale + 0.01 else { return false }
            let velocity = dismissPan.velocity(in: self)
            return velocity.y > 0 && velocity.y > abs(velocity.x)
        }

        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
        ) -> Bool {
            gestureRecognizer === dismissPan
        }
    }
}

// MARK: - Video player

struct VideoPlayerScreen: View {
    let ref: AttachmentRef
    let url: URL
    let callActive: Bool

    @Environment(\.dismiss) private var dismiss
    @State private var player: AVPlayer?
    @State private var shareURL: URL?
    @State private var audioStarted = false

    var body: some View {
        VStack(spacing: 0) {
            ViewerBar(shareURL: shareURL) { dismiss() }
            if let player {
                PlayerControllerView(player: player)
            } else {
                Spacer()
            }
        }
        .background(Color.black.ignoresSafeArea())
        .gesture(
            DragGesture().onEnded { value in
                if value.translation.height > 120, abs(value.translation.width) < 80 { dismiss() }
            }
        )
        .onAppear {
            // The file is named after the sender's choice, so the container
            // type is stated rather than guessed from the extension.
            let asset = AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"])
            let player = AVPlayer(playerItem: AVPlayerItem(asset: asset))
            self.player = player
            audioStarted = (try? MediaAudioSession.begin(.playback, callActive: callActive)) != nil
            player.play()
            shareURL = MediaShareCopy.link(url, ref: ref)
        }
        .onDisappear {
            player?.pause()
            player = nil
            if audioStarted { MediaAudioSession.end(.playback) }
            if let shareURL { try? FileManager.default.removeItem(at: shareURL.deletingLastPathComponent()) }
        }
    }
}

/// The platform's standard video controls.
private struct PlayerControllerView: UIViewControllerRepresentable {
    let player: AVPlayer

    func makeUIViewController(context: Context) -> AVPlayerViewController {
        let controller = AVPlayerViewController()
        controller.player = player
        controller.allowsPictureInPicturePlayback = false
        return controller
    }

    func updateUIViewController(_ controller: AVPlayerViewController, context: Context) {
        if controller.player !== player { controller.player = player }
    }
}
