// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Combine
import LakkoCore
import SwiftUI

/// ADR-0047 / UX spec §2, §3, §5 — the timeline bubbles for voice messages,
/// video notes and GIFs.
///
/// A bubble is chosen by ``InlineMedia/decision(for:allowlist:)`` — the
/// sender's `kind` AND an allowlisted MIME type of the matching media class —
/// before this file is reached; nothing here re-decides it. Media is fetched
/// and decrypted through ``ChatMediaController``, whose AEAD open fails before
/// any byte is written, so a bubble either plays authenticated bytes or shows
/// `Couldn't play this message`.

/// Upload state of an outgoing media message still in the send queue.
public enum MediaUploadState: Equatable {
    case uploading(Double)
    case failed
}

struct MediaAttachmentView: View {
    let ref: AttachmentRef
    let bubble: InlineMedia.Bubble
    /// Hex blob id for a stored message; the local key for a pending send.
    let mediaKey: String
    let incoming: Bool
    var upload: MediaUploadState?
    /// Pending sends only: whether the local bytes are on disk yet.
    var localReady = true
    var onRetry: () -> Void = {}

    var body: some View {
        switch bubble {
        case .voice:
            VoiceBubble(ref: ref, mediaKey: mediaKey, incoming: incoming, upload: upload, onRetry: onRetry)
        case .videoNote:
            VideoNoteBubble(ref: ref, mediaKey: mediaKey, incoming: incoming, upload: upload, onRetry: onRetry)
        case .gif:
            GifBubble(ref: ref, mediaKey: mediaKey, upload: upload, localReady: localReady, onRetry: onRetry)
        case .photo:
            PhotoBubble(ref: ref)
        case .video:
            VideoBubble(ref: ref)
        case .file:
            EmptyView()
        }
    }
}

private struct NotSentButton: View {
    let onRetry: () -> Void

    var body: some View {
        Button(action: onRetry) {
            Text("Not sent \u{00B7} Retry")
                .font(.caption)
                .underline()
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Not sent. Retry")
    }
}

private struct CouldNotPlay: View {
    let onRetry: () -> Void

    var body: some View {
        Button(action: onRetry) {
            Text("Couldn't play this message")
                .font(.caption)
        }
        .buttonStyle(.plain)
        .accessibilityHint("Tries again")
    }
}

// MARK: - Voice

struct VoiceBubble: View {
    let ref: AttachmentRef
    let mediaKey: String
    let incoming: Bool
    let upload: MediaUploadState?
    let onRetry: () -> Void

    @EnvironmentObject private var media: ChatMediaController
    @EnvironmentObject private var live: MediaLiveState

    private var tint: Color { incoming ? Color.lakkoText : Color.lakkoAccentForeground }
    private var isPending: Bool { upload != nil }
    private var active: ChatMediaController.VoicePlayback? {
        live.voice?.key == mediaKey ? live.voice : nil
    }
    private var durationMs: Int64 {
        if let d = active?.durationMs, d > 0 { return d }
        return Int64(ref.durationMs ?? 0)
    }
    private var progress: Double {
        guard let active, active.durationMs > 0 else { return 0 }
        return Double(active.positionMs) / Double(active.durationMs)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                playButton
                VStack(alignment: .leading, spacing: 4) {
                    waveform
                        .frame(height: 26)
                    HStack(spacing: 6) {
                        Text(RecordingMachine.formatClock(ms: active.map { $0.positionMs } ?? durationMs))
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(tint.opacity(0.8))
                        if incoming, !media.isPlayed(mediaKey) {
                            Circle()
                                .fill(Color.lakkoAccent)
                                .frame(width: 6, height: 6)
                                .accessibilityLabel("Not played")
                        }
                        Spacer(minLength: 0)
                        if active != nil {
                            Button(PlaybackQueue.rateLabel(media.rate)) {
                                media.rate = PlaybackQueue.nextRate(after: media.rate)
                            }
                            .font(.caption2.bold())
                            .buttonStyle(.plain)
                            .foregroundStyle(tint)
                            .accessibilityLabel("Playback speed \(PlaybackQueue.rateLabel(media.rate))")
                        }
                    }
                }
                .frame(width: 170)
            }
            if upload == .failed {
                NotSentButton(onRetry: onRetry)
            } else if media.failedKeys.contains(mediaKey) {
                CouldNotPlay { toggle() }
            }
        }
    }

    private var playButton: some View {
        Button(action: toggle) {
            ZStack {
                Circle().fill(tint)
                if media.loadingKeys.contains(mediaKey) {
                    ProgressView().tint(incoming ? Color.lakkoSurfaceSunken : Color.lakkoAccent)
                } else {
                    Image(systemName: active?.playing == true ? "pause.fill" : "play.fill")
                        .foregroundStyle(incoming ? Color.lakkoSurfaceSunken : Color.lakkoAccent)
                }
                if case .uploading(let fraction) = upload {
                    ProgressRing(fraction: fraction, lineWidth: 3, color: tint)
                        .padding(-4)
                }
            }
            .frame(width: 38, height: 38)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            active?.playing == true
                ? "Pause voice message"
                : "Play voice message, \(RecordingMachine.spokenDuration(ms: durationMs))"
        )
    }

    private var waveform: some View {
        WaveformBars(waveform: ref.waveform, progress: progress, tint: tint)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0).onEnded { value in
                    guard active != nil else { return }
                    media.seekVoice(key: mediaKey, fraction: value.location.x / 170)
                }
            )
            .accessibilityElement()
            .accessibilityLabel("Seek voice message")
            .accessibilityValue(RecordingMachine.spokenDuration(ms: active?.positionMs ?? 0))
            .accessibilityAdjustableAction { direction in
                guard let active, active.durationMs > 0 else { return }
                let step = 5000.0 / Double(active.durationMs)
                let delta = direction == .increment ? step : -step
                media.seekVoice(key: mediaKey, fraction: progress + delta)
            }
    }

    private func toggle() {
        if isPending {
            media.toggleVoice(key: mediaKey, ref: nil, fileURL: media.localFile(for: mediaKey))
        } else {
            media.toggleVoice(key: mediaKey, ref: ref)
        }
    }
}

// MARK: - Video note

@MainActor
final class VideoNotePlayerModel: ObservableObject {
    enum Mode { case poster, muted, sound }

    @Published var mode: Mode = .poster
    @Published var paused = false
    @Published var progress: Double = 0
    @Published var loading = false
    @Published var failed = false
    @Published var poster: URL?
    @Published private(set) var player: AVPlayer?

    private var timeObserver: Any?
    private var endObserver: NSObjectProtocol?
    private var fileURL: URL?

    func loadPoster(_ thumb: ThumbRef?, media: ChatMediaController) async {
        guard poster == nil, let thumb else { return }
        poster = await media.posterFile(for: thumb)
    }

    private func ensurePlayer(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async -> AVPlayer? {
        if let player { return player }
        loading = true
        defer { loading = false }
        let url: URL?
        if pending {
            url = media.localFile(for: key)
        } else {
            url = try? await media.mediaFile(for: ref)
        }
        guard let url else {
            failed = true
            return nil
        }
        failed = false
        fileURL = url
        let item = AVPlayerItem(asset: AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
        let player = AVPlayer(playerItem: item)
        player.actionAtItemEnd = .pause
        timeObserver = player.addPeriodicTimeObserver(forInterval: CMTime(value: 1, timescale: 20), queue: .main) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self, let d = player.currentItem?.duration, d.isNumeric, CMTimeGetSeconds(d) > 0 else { return }
                self.progress = CMTimeGetSeconds(time) / CMTimeGetSeconds(d)
                if player.currentItem?.status == .failed { self.failed = true }
            }
        }
        endObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.ended(key: key, media: media) }
        }
        self.player = player
        return player
    }

    func autoplayMuted(ref: AttachmentRef, key: String, media: ChatMediaController) async {
        guard mode == .poster, let player = await ensurePlayer(ref: ref, key: key, pending: false, media: media) else { return }
        player.isMuted = true
        mode = .muted
        paused = false
        player.play()
    }

    func tap(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async {
        if mode == .sound, let player {
            if paused {
                media.claim(key) { [weak self] in self?.pauseForOther() }
                player.play()
                paused = false
            } else {
                player.pause()
                paused = true
                media.releaseClaim(key)
            }
            return
        }
        guard let player = await ensurePlayer(ref: ref, key: key, pending: pending, media: media) else { return }
        media.stopPlayback()
        media.claim(key) { [weak self] in self?.pauseForOther() }
        _ = try? MediaAudioSession.begin(.playback, callActive: media.callActive)
        player.isMuted = false
        await player.seek(to: .zero)
        mode = .sound
        paused = false
        player.play()
        if !pending { media.markPlayed(key) }
    }

    private func pauseForOther() {
        guard mode == .sound else { return }
        player?.pause()
        paused = true
    }

    private func ended(key: String, media: ChatMediaController) {
        guard let player else { return }
        if mode == .muted {
            player.seek(to: .zero)
            player.play()
            return
        }
        media.releaseClaim(key)
        MediaAudioSession.end(.playback)
        player.seek(to: .zero)
        progress = 0
        paused = false
        if media.prefs.autoplayVideoNotes {
            player.isMuted = true
            mode = .muted
            player.play()
        } else {
            mode = .poster
        }
    }

    func tearDown(key: String, media: ChatMediaController) {
        player?.pause()
        if let timeObserver { player?.removeTimeObserver(timeObserver) }
        timeObserver = nil
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = nil
        if mode == .sound {
            media.releaseClaim(key)
            MediaAudioSession.end(.playback)
        }
        player = nil
        mode = .poster
    }
}

struct VideoNoteBubble: View {
    static let diameter: CGFloat = 240

    let ref: AttachmentRef
    let mediaKey: String
    let incoming: Bool
    let upload: MediaUploadState?
    let onRetry: () -> Void

    @EnvironmentObject private var media: ChatMediaController
    @StateObject private var model = VideoNotePlayerModel()
    @AppStorage(MediaPrefs.autoplayKey) private var autoplay = false

    var body: some View {
        VStack(alignment: incoming ? .leading : .trailing, spacing: 4) {
            Button {
                Task { await model.tap(ref: ref, key: mediaKey, pending: upload != nil, media: media) }
            } label: {
                ZStack {
                    Circle().fill(Color(.secondarySystemBackground))
                    if let poster = model.poster, model.mode == .poster {
                        AnimatedImageView(source: .file(poster))
                            .clipShape(Circle())
                    }
                    if model.mode != .poster {
                        PlayerLayerView(player: model.player)
                            .clipShape(Circle())
                    }
                    if model.loading {
                        ProgressView()
                    } else if model.mode == .poster || (model.mode == .sound && model.paused) {
                        Image(systemName: "play.fill")
                            .font(.title)
                            .foregroundStyle(.white)
                            .padding(18)
                            .background(Circle().fill(Color.black.opacity(0.45)))
                    }
                    if model.mode == .muted {
                        Image(systemName: "speaker.slash.fill")
                            .font(.caption)
                            .foregroundStyle(.white)
                            .padding(6)
                            .background(Circle().fill(Color.black.opacity(0.45)))
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                            .padding(16)
                    }
                    if model.mode == .sound {
                        ProgressRing(fraction: model.progress, lineWidth: 4, color: Color.lakkoAccent)
                    }
                    if case .uploading(let fraction) = upload {
                        ProgressRing(fraction: fraction, lineWidth: 4, color: Color.lakkoAccent)
                    }
                }
                .frame(width: Self.diameter, height: Self.diameter)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(accessibilityLabel)
            HStack(spacing: 6) {
                if let d = ref.durationMs {
                    Text(RecordingMachine.formatClock(ms: Int64(d)))
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if incoming, !media.isPlayed(mediaKey) {
                    Circle().fill(Color.lakkoAccent).frame(width: 6, height: 6)
                }
            }
            if upload == .failed {
                NotSentButton(onRetry: onRetry).foregroundStyle(.red)
            } else if model.failed {
                CouldNotPlay {
                    Task { await model.tap(ref: ref, key: mediaKey, pending: upload != nil, media: media) }
                }
                .foregroundStyle(.secondary)
            }
        }
        .task(id: mediaKey) {
            if upload == nil { await model.loadPoster(ref.thumb, media: media) }
            if autoplay, upload == nil { await model.autoplayMuted(ref: ref, key: mediaKey, media: media) }
        }
        .onDisappear { model.tearDown(key: mediaKey, media: media) }
    }

    private var accessibilityLabel: String {
        let spoken = RecordingMachine.spokenDuration(ms: Int64(ref.durationMs ?? 0))
        switch model.mode {
        case .sound: return model.paused ? "Play video message, \(spoken)" : "Pause video message"
        case .muted, .poster: return "Play video message with sound, \(spoken)"
        }
    }
}

// MARK: - GIF

struct GifBubble: View {
    static let maxWidth: CGFloat = 240

    let ref: AttachmentRef
    let mediaKey: String
    let upload: MediaUploadState?
    let localReady: Bool
    let onRetry: () -> Void

    @EnvironmentObject private var media: ChatMediaController
    @State private var fileURL: URL?
    @State private var failed = false
    @State private var fullScreen = false

    private var size: CGSize {
        let w = Double(ref.width ?? 1)
        let h = Double(ref.height ?? 1)
        let aspect = w > 0 && h > 0 ? h / w : 1
        let width = Self.maxWidth
        return CGSize(width: width, height: min(320, max(80, width * aspect)))
    }

    private var inline: InlineMediaType? { InlineMedia.inlineType(mime: ref.mime) }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button { if fileURL != nil { fullScreen = true } } label: {
                ZStack(alignment: .bottomLeading) {
                    RoundedRectangle(cornerRadius: 12).fill(Color(.tertiarySystemFill))
                    if let fileURL {
                        animation(fileURL)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    } else if !failed {
                        ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    Text("GIF")
                        .font(.caption2.bold())
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(Capsule().fill(Color.black.opacity(0.5)))
                        .foregroundStyle(.white)
                        .padding(6)
                    if case .uploading(let fraction) = upload {
                        ProgressRing(fraction: fraction, lineWidth: 3, color: .white)
                            .frame(width: 32, height: 32)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                .frame(width: size.width, height: size.height)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("GIF")
            .accessibilityHint("Opens it full screen")
            if upload == .failed {
                NotSentButton(onRetry: onRetry)
            } else if failed {
                CouldNotPlay { Task { await load() } }
            }
        }
        // A pending GIF's bytes arrive after the row appears (the proxy
        // download fills in the name and type), so that change reloads too.
        .task(id: "\(mediaKey)-\(upload == nil)-\(localReady)") { await load() }
        .sheet(isPresented: $fullScreen) {
            if let fileURL {
                NavigationStack {
                    animation(fileURL, fit: true)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.black)
                        .toolbar {
                            ToolbarItem(placement: .topBarTrailing) {
                                Button("Done") { fullScreen = false }
                            }
                        }
                }
            }
        }
    }

    @ViewBuilder
    private func animation(_ url: URL, fit: Bool = false) -> some View {
        if inline == .video {
            LoopingVideoView(url: url)
        } else {
            AnimatedImageView(source: .file(url), contentMode: fit ? .scaleAspectFit : .scaleAspectFill)
        }
    }

    private func load() async {
        failed = false
        if let local = media.localFile(for: mediaKey) {
            fileURL = local
            return
        }
        guard upload == nil else { return }
        do {
            fileURL = try await media.mediaFile(for: ref)
        } catch {
            failed = true
        }
    }
}
