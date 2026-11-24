// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Combine
import LakkoCore
import os
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

private struct MediaColumnWidthKey: EnvironmentKey {
    static let defaultValue: CGFloat? = nil
}

extension EnvironmentValues {
    /// `docs/ux/feed.md` section 3 — the width of the column a media bubble
    /// sits in, where that can be narrower than the bubble. `nil` in the chat,
    /// whose bubbles take their own boxes.
    var mediaColumnWidth: CGFloat? {
        get { self[MediaColumnWidthKey.self] }
        set { self[MediaColumnWidthKey.self] = newValue }
    }
}

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
    /// ADR-0082 section 4 — the stored message a transcript belongs to;
    /// `nil` for a take still being sent (the button's space stays empty).
    var transcript: TranscriptTarget? = nil
    /// `docs/ux/media-messages.md` section 3 (2026-11-22) — the message's own
    /// time and ticks, drawn on a pill inside a lone video note's circle.
    /// `nil` everywhere else: the caller draws the line itself.
    var noteMeta: AnyView? = nil

    var body: some View {
        switch bubble {
        case .voice:
            VoiceBubble(
                ref: ref, mediaKey: mediaKey, incoming: incoming, upload: upload, onRetry: onRetry,
                transcript: transcript)
        case .videoNote:
            VideoNoteBubble(
                ref: ref, mediaKey: mediaKey, incoming: incoming, upload: upload, onRetry: onRetry,
                transcript: transcript, meta: noteMeta)
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
            Text(Localization.text("chat.media.not_sent_retry"))
                .font(.caption)
                .underline()
        }
        .buttonStyle(.plain)
    }
}

private struct CouldNotPlay: View {
    let onRetry: () -> Void

    var body: some View {
        Button(action: onRetry) {
            Text(Localization.text("chat.media.play_failed"))
                .font(.caption)
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Voice

struct VoiceBubble: View {
    let ref: AttachmentRef
    let mediaKey: String
    let incoming: Bool
    let upload: MediaUploadState?
    let onRetry: () -> Void
    var transcript: TranscriptTarget? = nil

    /// The width of the row: the play disc, the gap and the waveform column
    /// (which follows a feed column, `docs/ux/feed.md` section 3). The
    /// transcript opens under the row at this width, so the bubble does not
    /// widen when it does.
    private var rowWidth: CGFloat { 38 + 10 + waveformWidth }

    @EnvironmentObject private var media: ChatMediaController
    @EnvironmentObject private var live: MediaLiveState
    @Environment(\.mediaColumnWidth) private var columnWidth
    /// When the touch now on the waveform went down — see ``VoiceSeekGate``.
    @State private var waveformPressStart: Date?
    /// `client-redesign.md` — whether the message action menu is on screen, and
    /// therefore whether this press has already been answered. See ``toggle()``.
    @Environment(\.messageMenuPresenter) private var menu

    private var tint: Color { incoming ? Color.lakkoText : Color.lakkoAccentForeground }
    private var isPending: Bool { upload != nil }
    private var waveformWidth: CGFloat { InlineMedia.voiceWaveformWidth(within: columnWidth) }
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
                        Text(active.map { RecordingMachine.formatClock(ms: $0.positionMs) }
                            ?? RecordingMachine.formatTotalClock(ms: durationMs))
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(tint.opacity(0.8))
                        if incoming, !media.isPlayed(mediaKey) {
                            Circle()
                                .fill(Color.lakkoAccent)
                                .frame(width: 6, height: 6)
                                .accessibilityLabel(Localization.text("chat.voice.not_played"))
                        }
                        Spacer(minLength: 0)
                        if active != nil {
                            Button(PlaybackQueue.rateLabel(media.rate)) {
                                // Same rule as ``toggle()``: a press the menu
                                // has already answered ends here.
                                guard menu?.isOpen != true else { return }
                                media.rate = PlaybackQueue.nextRate(after: media.rate)
                            }
                            .font(.caption2.bold())
                            .buttonStyle(.plain)
                            .foregroundStyle(tint)
                            .accessibilityLabel(
                                Localization.text(
                                    "chat.voice.speed", texts: ["rate": PlaybackQueue.rateLabel(media.rate)]))
                        }
                        // ADR-0082 / UX 4.1 — the transcript button at the
                        // trailing end. Its frame is the same in every
                        // state, offered or not, so the row never moves.
                        TranscriptButton(target: transcript, ref: ref, mediaKey: mediaKey, tint: tint)
                    }
                }
                .frame(width: waveformWidth)
            }
            TranscriptBlock(target: transcript, ref: ref, outgoing: !incoming, width: rowWidth)
            if upload == .failed {
                NotSentButton(onRetry: onRetry)
            } else if media.failedKeys.contains(mediaKey) {
                MediaFailureLine(failure: media.failure(mediaKey), onRetry: toggle) {
                    CouldNotPlay { toggle() }
                }
            }
        }
        // The row is in a LazyVStack, so this runs as the bubble scrolls into
        // view — the same moment a photo or a GIF warms itself. Without it the
        // first press paid the download and the AEAD open before any sample
        // could be decoded.
        .task(id: mediaKey) {
            guard VoicePrefetch.shouldWarm(pending: isPending) else { return }
            await media.prefetchVoice(ref: ref, key: mediaKey)
        }
    }

    private var playButton: some View {
        Button(action: toggle) {
            ZStack {
                Circle().fill(tint)
                if media.loadingKeys.contains(mediaKey) {
                    ProgressView().tint(incoming ? Color.lakkoSurfaceSunken : Color.lakkoAccent)
                } else {
                    LkIconView(active?.playing == true ? .pauseFill : .playFill, size: 20, relativeTo: nil)
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
                ? Localization.text("chat.voice.pause")
                : Localization.text(
                    "chat.voice.play",
                    texts: ["duration": RecordingMachine.spokenTotalDuration(ms: durationMs)])
        )
    }

    private var waveform: some View {
        WaveformBars(waveform: ref.waveform, progress: progress, tint: tint)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in
                        if waveformPressStart == nil { waveformPressStart = Date() }
                    }
                    .onEnded { value in
                        let held = waveformPressStart.map { -$0.timeIntervalSinceNow } ?? 0
                        waveformPressStart = nil
                        // A press held long enough to open the message's
                        // context menu was reaching for React or Delete, not
                        // for a point in the audio.
                        guard active != nil, VoiceSeekGate.acceptsSeek(pressSeconds: held) else { return }
                        media.seekVoice(key: mediaKey, fraction: value.location.x / waveformWidth)
                    }
            )
            .accessibilityElement()
            .accessibilityLabel(Localization.text("chat.voice.seek"))
            .accessibilityValue(RecordingMachine.spokenDuration(ms: active?.positionMs ?? 0))
            .accessibilityAdjustableAction { direction in
                guard let active, active.durationMs > 0 else { return }
                let step = 5000.0 / Double(active.durationMs)
                let delta = direction == .increment ? step : -step
                media.seekVoice(key: mediaKey, fraction: progress + delta)
            }
    }

    /// Play or pause, unless the press that arrived here has already been
    /// answered by the message menu.
    ///
    /// `client-redesign.md`: "once the menu has opened, the control underneath
    /// receives nothing more from that press — not a tap on release, and not a
    /// drag." A press held over the play disc opens the menu at
    /// ``MessageLongPress/thresholdSeconds`` with the finger still down; the lift
    /// that follows would otherwise arrive here and start playback nobody asked
    /// for.
    ///
    /// This is the same rule ``VoiceSeekGate`` already gives the waveform,
    /// reached from the other end: the waveform asks how long the press lasted,
    /// and this asks whether the menu took it. The menu's own presence is the
    /// stronger question — it cannot go stale, and it needs no press clock —
    /// which is why the play button is written this way and not with a second
    /// timer. It is deliberately independent of `MessageMenuHost`'s `.disabled`
    /// so that the rule does not rest on SwiftUI cancelling an in-flight button
    /// press when its subtree is disabled.
    private func toggle() {
        if menu?.isOpen == true { return }
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
    typealias Mode = VideoNotePlayback.Mode

    /// How long a downloaded note may take to become playable.
    static let readyTimeout: Duration = .seconds(15)
    /// How long a ready note may take to actually start playing.
    static let startTimeout: Duration = .seconds(10)

    private static let log = Logger(subsystem: "me.lakko.media", category: "video-note")

    @Published var mode: Mode = .poster
    @Published var paused = false
    @Published var progress: Double = 0
    /// A tap is downloading or preparing the note.
    @Published var starting = false
    @Published var failed = false
    @Published var poster: URL?
    @Published private(set) var player: AVPlayer?

    private var timeObserver: Any?
    private var endObserver: NSObjectProtocol?
    private var failedToEndObserver: NSObjectProtocol?
    private var statusObservation: NSKeyValueObservation?
    private var timeControlObservation: NSKeyValueObservation?
    /// Bumped by every teardown, so an await that resumes after the bubble
    /// went away does not start playback on a model nobody shows.
    private var generation = 0
    /// Marks the note played the first time sound playback actually runs.
    private var onSoundStarted: (() -> Void)?
    private var key: String?
    private weak var media: ChatMediaController?

    func loadPoster(_ thumb: ThumbRef?, media: ChatMediaController) async {
        guard poster == nil, let thumb else { return }
        poster = await media.posterFile(for: thumb)
    }

    private func ensurePlayer(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async -> AVPlayer? {
        if let player, player.currentItem?.status != .failed { return player }
        discardPlayer()
        let gen = generation
        let url: URL?
        if pending {
            url = media.localFile(for: key)
        } else {
            url = await media.videoNoteFile(for: ref, key: key)
        }
        guard gen == generation else {
            Self.log.info("video note bubble went away while the file was loading")
            return nil
        }
        if let player { return player }
        guard let url else {
            fail("file-unavailable", error: nil, surface: true)
            return nil
        }
        self.key = key
        self.media = media
        let item = AVPlayerItem(asset: AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
        let player = AVPlayer(playerItem: item)
        player.actionAtItemEnd = .pause
        timeObserver = player.addPeriodicTimeObserver(forInterval: CMTime(value: 1, timescale: 20), queue: .main) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self, let d = player.currentItem?.duration, d.isNumeric, CMTimeGetSeconds(d) > 0 else { return }
                self.progress = CMTimeGetSeconds(time) / CMTimeGetSeconds(d)
            }
        }
        statusObservation = item.observe(\.status, options: [.new]) { [weak self] observed, _ in
            guard observed.status == .failed else { return }
            let error = observed.error as NSError?
            Task { @MainActor in self?.itemFailed(gen: gen, error: error) }
        }
        timeControlObservation = player.observe(\.timeControlStatus, options: [.new]) { [weak self] observed, _ in
            guard observed.timeControlStatus == .playing else { return }
            Task { @MainActor in self?.playbackRunning(gen: gen) }
        }
        endObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.ended(key: key, media: media) }
        }
        failedToEndObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemFailedToPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] note in
            let error = note.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? NSError
            MainActor.assumeIsolated { self?.itemFailed(gen: gen, error: error) }
        }
        self.player = player
        return player
    }

    func autoplayMuted(ref: AttachmentRef, key: String, media: ChatMediaController) async {
        guard mode == .poster, !starting, let player = await ensurePlayer(ref: ref, key: key, pending: false, media: media) else { return }
        guard mode == .poster, !starting else { return }
        player.isMuted = true
        mode = .muted
        paused = false
        player.play()
    }

    func tap(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async {
        let action = VideoNotePlayback.tapAction(mode: mode, paused: paused, starting: starting, hasPlayer: player != nil)
        Self.log.info("video note tap: action=\(String(describing: action), privacy: .public) pending=\(pending, privacy: .public)")
        switch action {
        case .ignore:
            return
        case .pause:
            player?.pause()
            paused = true
            media.releaseClaim(key)
        case .resume:
            media.claim(key) { [weak self] in self?.pauseForOther() }
            player?.play()
            paused = false
        case .start:
            await start(ref: ref, key: key, pending: pending, media: media)
        }
    }

    private func start(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async {
        guard let player = await prepare(ref: ref, key: key, pending: pending, media: media) else { return }
        let gen = generation
        media.stopPlayback()
        media.claim(key) { [weak self] in self?.pauseForOther() }
        _ = try? MediaAudioSession.begin(.playback, callActive: media.callActive)
        player.isMuted = false
        // The item is ready, so the seek completes.
        if CMTimeCompare(player.currentTime(), .zero) != 0 {
            await player.seek(to: .zero)
        }
        guard gen == generation, self.player === player else {
            media.releaseClaim(key)
            MediaAudioSession.end(.playback)
            return
        }
        mode = .sound
        paused = false
        progress = 0
        onSoundStarted = pending ? nil : { [weak media] in media?.markPlayed(key) }
        player.play()
        let observed = await KeyValueWait.first(of: player, \.timeControlStatus, timeout: Self.startTimeout) { $0 == .playing }
        let stillWanted = gen == generation && self.player === player && mode == .sound && !paused
        if VideoNotePlayback.startFailed(observed: observed, stillWanted: stillWanted) {
            let reason = player.reasonForWaitingToPlay?.rawValue ?? "none"
            Self.log.error("video note did not start playing: waiting=\(reason, privacy: .public)")
            fail("start-timeout", error: player.currentItem?.error as NSError?, surface: true)
        } else if observed == .playing {
            playbackRunning(gen: gen)
        }
    }

    /// Downloads if needed and waits until the item is ready to play. Shows the
    /// spinner meanwhile; on failure or timeout sets `failed`.
    private func prepare(ref: AttachmentRef, key: String, pending: Bool, media: ChatMediaController) async -> AVPlayer? {
        starting = true
        failed = false
        defer { starting = false }
        let gen = generation
        guard let player = await ensurePlayer(ref: ref, key: key, pending: pending, media: media),
              let item = player.currentItem else { return nil }
        let status = await KeyValueWait.first(of: item, \.status, timeout: Self.readyTimeout) { $0 != .unknown }
        guard gen == generation, self.player === player else { return nil }
        switch VideoNotePlayback.readiness(status) {
        case .ready:
            Self.log.info("video note ready to play")
            return player
        case .failed:
            fail("item-failed", error: item.error as NSError?, surface: true)
            return nil
        case .timedOut:
            fail("ready-timeout", error: nil, surface: true)
            return nil
        }
    }

    private func playbackRunning(gen: Int) {
        guard gen == generation, mode == .sound, let onSoundStarted else { return }
        self.onSoundStarted = nil
        Self.log.info("video note playback started")
        onSoundStarted()
    }

    private func itemFailed(gen: Int, error: NSError?) {
        guard gen == generation, player != nil else { return }
        // A muted autoplay that fails falls back to the poster quietly; a tap
        // the user is waiting on says so.
        fail("item-failed", error: error, surface: mode == .sound || starting)
    }

    private func fail(_ reason: String, error: NSError?, surface: Bool) {
        let domain = error?.domain ?? "none"
        let code = error?.code ?? 0
        Self.log.error("video note playback failed: reason=\(reason, privacy: .public) domain=\(domain, privacy: .public) code=\(code, privacy: .public)")
        if mode == .sound, let key, let media {
            media.releaseClaim(key)
            MediaAudioSession.end(.playback)
        }
        discardPlayer()
        mode = .poster
        paused = false
        progress = 0
        if surface { failed = true }
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
            player.pause()
            mode = .poster
        }
    }

    private func discardPlayer() {
        player?.pause()
        if let timeObserver { player?.removeTimeObserver(timeObserver) }
        timeObserver = nil
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = nil
        if let failedToEndObserver { NotificationCenter.default.removeObserver(failedToEndObserver) }
        failedToEndObserver = nil
        statusObservation?.invalidate()
        statusObservation = nil
        timeControlObservation?.invalidate()
        timeControlObservation = nil
        onSoundStarted = nil
        player = nil
    }

    /// The bubble left the screen. A download already under way keeps going
    /// in ``ChatMediaController``; the next tap finds the file.
    func tearDown(key: String, media: ChatMediaController) {
        generation += 1
        if mode == .sound {
            media.releaseClaim(key)
            MediaAudioSession.end(.playback)
        }
        discardPlayer()
        mode = .poster
        paused = false
        progress = 0
        starting = false
    }
}

struct VideoNoteBubble: View {
    static let diameter: CGFloat = 240

    let ref: AttachmentRef
    let mediaKey: String
    let incoming: Bool
    let upload: MediaUploadState?
    let onRetry: () -> Void
    var transcript: TranscriptTarget? = nil
    /// Section 3 (2026-11-22) — the message's time and ticks, on a pill inside
    /// the circle, bottom trailing; `nil` when the caller draws them itself
    /// (a note in a bubble with chrome, a take still being sent).
    var meta: AnyView? = nil

    @EnvironmentObject private var media: ChatMediaController
    @StateObject private var model = VideoNotePlayerModel()
    @AppStorage(MediaPrefs.autoplayKey) private var autoplay = false
    @Environment(\.mediaColumnWidth) private var columnWidth

    private var busy: Bool { model.starting || media.loadingKeys.contains(mediaKey) }
    /// The circle fits the column with the `Aa` control beside it.
    private var side: CGFloat {
        let aside = TranscriptButton.size.width + VideoNoteLayout.asideGap
        return InlineMedia.fit(
            CGSize(width: Self.diameter, height: Self.diameter),
            within: columnWidth.map { max(0, $0 - aside) }
        ).width
    }

    /// Section 3 / translation UX 4.1 — `Aa` beside the circle, vertically
    /// centred, its space reserved in every state.
    private var aside: some View {
        TranscriptButton(target: transcript, ref: ref, mediaKey: mediaKey, tint: Color.lakkoTextMuted)
    }

    /// Section 3 (2026-11-22) — the photo bubble's translucent pill.
    private func pill<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .foregroundStyle(Color.white)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(Color.black.opacity(0.5)))
    }

    @ViewBuilder
    private var durationPill: some View {
        if let label = VideoNoteLayout.durationLabel(
            playingWithSound: model.mode == .sound, progress: model.progress,
            durationMs: ref.durationMs.map { Int64($0) }) {
            pill {
                HStack(spacing: 4) {
                    switch label {
                    case .elapsed(let ms):
                        Text(RecordingMachine.formatClock(ms: ms))
                    case .total(let ms):
                        Text(RecordingMachine.formatTotalClock(ms: ms))
                    }
                    if incoming, !media.isPlayed(mediaKey), case .total = label {
                        Circle().fill(Color.lakkoAccent).frame(width: 6, height: 6)
                    }
                }
                .font(.caption2.monospacedDigit())
            }
            .accessibilityHidden(true)
            .allowsHitTesting(false)
        }
    }

    var body: some View {
        VStack(alignment: incoming ? .leading : .trailing, spacing: 4) {
            HStack(alignment: .center, spacing: VideoNoteLayout.asideGap) {
            if VideoNoteLayout.asideFirst(incoming: incoming) { aside }
            Button {
                Task { await model.tap(ref: ref, key: mediaKey, pending: upload != nil, media: media) }
            } label: {
                ZStack {
                    Circle().fill(Color(.secondarySystemBackground))
                    // The UIKit-backed layers are display only: they must not
                    // take the touch from the button.
                    if let poster = model.poster, model.mode == .poster {
                        AnimatedImageView(source: .file(poster))
                            .clipShape(Circle())
                            .allowsHitTesting(false)
                    }
                    if model.mode != .poster {
                        PlayerLayerView(player: model.player)
                            .clipShape(Circle())
                            .allowsHitTesting(false)
                    }
                    Group {
                        if busy {
                            ProgressView()
                                .tint(.white)
                                .padding(18)
                                .background(Circle().fill(Color.black.opacity(0.45)))
                        } else if model.mode == .poster || (model.mode == .sound && model.paused) {
                            LkIconView(.playFill, size: 33, relativeTo: .title)
                                .foregroundStyle(.white)
                                .padding(18)
                                .background(Circle().fill(Color.black.opacity(0.45)))
                        }
                        if model.mode == .muted {
                            // Section 3 (2026-11-22) — top trailing, clear of
                            // the time pill at the bottom.
                            LkIconView(.speakerSlashFill, size: 14, relativeTo: .caption)
                                .foregroundStyle(.white)
                                .padding(6)
                                .background(Circle().fill(Color.black.opacity(0.45)))
                                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                                .padding(side * VideoNoteLayout.pillBottom)
                        }
                        if model.mode == .sound {
                            ProgressRing(fraction: model.progress, lineWidth: 4, color: Color.lakkoAccent)
                        }
                        if case .uploading(let fraction) = upload {
                            ProgressRing(fraction: fraction, lineWidth: 4, color: Color.lakkoAccent)
                        }
                    }
                    .allowsHitTesting(false)
                }
                .frame(width: side, height: side)
                .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(accessibilityLabel)
            // Section 3 (2026-11-22) — the duration bottom leading and the
            // time with its ticks bottom trailing, inside the circle; nothing
            // on a line under it but the reactions row.
            .overlay(alignment: .bottomLeading) {
                durationPill
                    .padding(.leading, side * VideoNoteLayout.pillInset)
                    .padding(.bottom, side * VideoNoteLayout.pillBottom)
            }
            .overlay(alignment: .bottomTrailing) {
                if let meta {
                    pill { meta }
                        .padding(.trailing, side * VideoNoteLayout.pillInset)
                        .padding(.bottom, side * VideoNoteLayout.pillBottom)
                }
            }
            if !VideoNoteLayout.asideFirst(incoming: incoming) { aside }
            }
            TranscriptBlock(target: transcript, ref: ref, outgoing: false, width: side)
            if upload == .failed {
                NotSentButton(onRetry: onRetry).foregroundStyle(.red)
            } else if model.failed {
                let retry: () -> Void = {
                    Task { await model.tap(ref: ref, key: mediaKey, pending: upload != nil, media: media) }
                }
                MediaFailureLine(failure: media.failure(mediaKey), onRetry: retry) {
                    CouldNotPlay { retry() }
                        .foregroundStyle(.secondary)
                }
            }
        }
        .task(id: mediaKey) {
            if upload == nil { await model.loadPoster(ref.thumb, media: media) }
            if autoplay, upload == nil { await model.autoplayMuted(ref: ref, key: mediaKey, media: media) }
        }
        .onDisappear { model.tearDown(key: mediaKey, media: media) }
    }

    private var accessibilityLabel: String {
        let spoken = RecordingMachine.spokenTotalDuration(ms: Int64(ref.durationMs ?? 0))
        if busy { return Localization.text("chat.video_note.loading") }
        switch model.mode {
        case .sound:
            return model.paused
                ? Localization.text("chat.video_note.play", texts: ["duration": spoken])
                : Localization.text("chat.record.pause_video")
        case .muted, .poster:
            return Localization.text("chat.video_note.play_sound", texts: ["duration": spoken])
        }
    }
}

// MARK: - GIF

struct GifBubble: View {
    static let maxWidth: CGFloat = 240
    /// Between the picture and the line under it (the column's spacing).
    static let lineSpacing: CGFloat = 4

    let ref: AttachmentRef
    let mediaKey: String
    let upload: MediaUploadState?
    let localReady: Bool
    let onRetry: () -> Void

    @EnvironmentObject private var media: ChatMediaController
    @State private var fileURL: URL?
    @State private var failed = false
    @State private var failure: MediaLoadFailure = .failed
    @State private var fullScreen = false
    @Environment(\.mediaColumnWidth) private var columnWidth

    private var size: CGSize {
        let w = Double(ref.width ?? 1)
        let h = Double(ref.height ?? 1)
        let aspect = w > 0 && h > 0 ? h / w : 1
        let width = Self.maxWidth
        return InlineMedia.fit(
            CGSize(width: width, height: min(320, max(80, width * aspect))), within: columnWidth)
    }

    private var inline: InlineMediaType? { InlineMedia.inlineType(mime: ref.mime) }

    var body: some View {
        VStack(alignment: .leading, spacing: Self.lineSpacing) {
            Button { if fileURL != nil { fullScreen = true } } label: {
                ZStack(alignment: .bottomLeading) {
                    RoundedRectangle(cornerRadius: 12).fill(Color(.tertiarySystemFill))
                    if let fileURL {
                        animation(fileURL)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    } else if !failed {
                        ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    if case .uploading(let fraction) = upload {
                        ProgressRing(fraction: fraction, lineWidth: 3, color: .white)
                            .frame(width: 32, height: 32)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                .frame(width: size.width, height: size.height)
            }
            .buttonStyle(.plain)
            // 2026-11-23 — no visible `GIF` label: the moving picture says
            // what it is. The accessibility label still names it a GIF.
            .accessibilityLabel(Localization.text("chat.gif.open_full"))
            if upload == .failed {
                NotSentButton(onRetry: onRetry)
            } else if failed {
                MediaFailureLine(failure: failure, onRetry: { Task { await load() } }) {
                    CouldNotPlay { Task { await load() } }
                }
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
                                Button(Localization.text("common.done")) { fullScreen = false }
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
            failure = MediaLoadFailure.of(error)
            failed = true
        }
    }
}
