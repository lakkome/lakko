// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import SwiftUI
import UIKit

/// ADR-0047 / UX spec §2, §3, §6 — the composer's recording surfaces: the
/// hold-to-record button, the recording bar that stands in for the text field,
/// the round video-note overlay and the permission-denied card. Every rule
/// about what a gesture does lives in ``RecordingMachine``; these views only
/// report the gesture and render ``ChatMediaController`` state.

/// The composer's right-hand button while the field is empty: mic or camera.
///
/// UX spec §3.1 — ONE button answering TWO gestures. A tap switches mic /
/// camera and records nothing; a press and hold records in the mode the glyph
/// is showing. This view does not decide which one happened: it reports the
/// touch down, the travel and the lift, and ``RecordingMachine`` takes the
/// verdict on the gesture — which is the whole point, because no SwiftUI body
/// in this target is reachable from a test.
///
/// Its identity must not change while a finger is down — SwiftUI drops a
/// gesture whose view leaves the hierarchy — so the composer keeps this view
/// mounted through `.idle` / `.pressing` / `.requesting` / `.recording`.
struct RecordButton: View {
    @ObservedObject var media: ChatMediaController
    @State private var pressed = false

    private var isVideo: Bool { media.recordMode == .videoNote }

    var body: some View {
        let recording: Bool = {
            if case .recording = media.recState { return true }
            return false
        }()
        ZStack {
            Circle()
                .fill(recording ? Color.lakkoDanger : Color.lakkoAccent.opacity(media.callActive ? 0.35 : 1))
                .frame(width: recording ? 56 : 40, height: recording ? 56 : 40)
            Image(systemName: isVideo ? "video.fill" : "mic.fill")
                .foregroundStyle(Color.lakkoAccentForeground)
        }
        .frame(width: 44, height: 44)
        // The tap / hold verdict takes `holdToRecordMs` to arrive, so the
        // button acknowledges the touch itself rather than leaving the first
        // moment of a hold looking like nothing happened.
        .scaleEffect(pressed && !recording ? 0.9 : 1)
        .animation(.easeOut(duration: 0.1), value: pressed)
        .overlay(alignment: .top) {
            if recording {
                // §2.5 — the lock target: slide up onto it.
                VStack(spacing: 2) {
                    Image(systemName: "lock.fill")
                    Image(systemName: "chevron.up")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(8)
                .background(Capsule().fill(Color(.secondarySystemBackground)))
                .offset(y: -70)
                .allowsHitTesting(false)
            }
        }
        .contentShape(Circle())
        .gesture(
            DragGesture(minimumDistance: 0, coordinateSpace: .global)
                .onChanged { value in
                    if !pressed {
                        pressed = true
                        media.press()
                    } else {
                        media.move(dx: value.translation.width, dy: value.translation.height)
                    }
                }
                .onEnded { _ in
                    pressed = false
                    media.release()
                }
        )
        .onDisappear {
            if pressed {
                pressed = false
                media.gestureLost()
            }
        }
        .accessibilityElement()
        .accessibilityLabel(isVideo ? "Record video message" : "Record voice message")
        .accessibilityHint(media.callActive ? ChatMediaController.inCallCopy : "Starts a locked recording")
        .accessibilityAddTraits(.isButton)
        // §3.1 — hold and slide are not reachable with VoiceOver or Switch
        // Control, so the two gestures become two actions on the one button:
        // activating it starts a LOCKED take, and the named action below is
        // the only way such a user can reach the other mode at all.
        .accessibilityAction { media.activate() }
        .accessibilityAction(named: isVideo ? "Switch to voice message" : "Switch to video message") {
            media.toggleMode()
        }
    }
}

/// Stands in for the text field while a take is recording, locked, finishing
/// or waiting in preview.
struct RecordingBar: View {
    @ObservedObject var media: ChatMediaController
    @ObservedObject var live: MediaLiveState

    init(media: ChatMediaController) {
        self.media = media
        self.live = media.live
    }

    var body: some View {
        switch media.recState {
        case .recording(_, _, let dx, _):
            HStack(spacing: 8) {
                pulsingDot
                clock
                LevelMeter(level: live.level)
                    .frame(width: 40, height: 18)
                Spacer(minLength: 4)
                HStack(spacing: 4) {
                    Image(systemName: "chevron.left")
                    Text("Slide to cancel")
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
                .offset(x: min(0, dx / 2))
                .opacity(1 + max(-0.8, dx / RecordingMachine.cancelSlidePoints))
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Recording, \(RecordingMachine.spokenDuration(ms: live.elapsedMs))")
        case .locked(let kind, _):
            HStack(spacing: 8) {
                Button(role: .destructive) { media.deleteTake() } label: {
                    Image(systemName: "trash")
                }
                .accessibilityLabel("Delete recording")
                pulsingDot
                clock
                if kind == .voice {
                    LevelMeter(level: live.level).frame(width: 40, height: 18)
                }
                Spacer(minLength: 4)
            }
        case .finalizing:
            HStack(spacing: 8) {
                ProgressView()
                Text("Finishing recording").font(.footnote).foregroundStyle(.secondary)
                Spacer()
            }
        case .preview(let kind, let durationMs):
            HStack(spacing: 10) {
                Button(role: .destructive) { media.deleteTake() } label: {
                    Image(systemName: "trash")
                }
                .accessibilityLabel("Delete recording")
                if kind == .voice, let take = media.previewTake {
                    voicePreview(take: take, durationMs: durationMs)
                } else {
                    Text("Video message \(RecordingMachine.formatTotalClock(ms: durationMs))")
                        .font(.footnote.monospacedDigit())
                    Spacer()
                }
            }
        case .idle, .pressing, .requesting:
            EmptyView()
        }
    }

    private var clock: some View {
        Text(RecordingMachine.formatClock(ms: live.elapsedMs))
            .font(.body.monospacedDigit())
    }

    private var pulsingDot: some View {
        TimelineView(.periodic(from: TimelineAnchor.date, by: 0.5)) { context in
            Circle()
                .fill(Color.red)
                .frame(width: 10, height: 10)
                .opacity(Int(context.date.timeIntervalSinceReferenceDate * 2) % 2 == 0 ? 1 : 0.35)
        }
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func voicePreview(take: CapturedTake, durationMs: Int64) -> some View {
        let active = live.voice?.key == ChatMediaController.previewKey ? live.voice : nil
        Button {
            media.toggleVoice(key: ChatMediaController.previewKey, ref: nil, fileURL: take.fileURL)
        } label: {
            Image(systemName: active?.playing == true ? "pause.fill" : "play.fill")
        }
        .accessibilityLabel(
            active?.playing == true
                ? "Pause recording"
                : "Play recording, \(RecordingMachine.spokenDuration(ms: durationMs))"
        )
        WaveformBars(
            waveform: take.waveform,
            progress: active.map { $0.durationMs > 0 ? Double($0.positionMs) / Double($0.durationMs) : 0 } ?? 0,
            tint: Color.lakkoAccent
        )
        .frame(height: 24)
        .accessibilityHidden(true)
        Text(active.map { RecordingMachine.formatClock(ms: $0.positionMs) }
            ?? RecordingMachine.formatTotalClock(ms: durationMs))
            .font(.footnote.monospacedDigit())
    }
}

/// The live level meter: five bars that rise with the input level.
struct LevelMeter: View {
    let level: Double

    var body: some View {
        HStack(alignment: .center, spacing: 3) {
            ForEach(0..<5, id: \.self) { i in
                let threshold = Double(i) / 5
                Capsule()
                    .fill(level > threshold * 0.6 ? Color.red : Color.red.opacity(0.25))
                    .frame(width: 4, height: 6 + CGFloat(i) * 3)
            }
        }
        .accessibilityHidden(true)
    }
}

/// The large round preview of a video note while it records, locks or waits
/// in preview (§3). Not hit-testable while a finger is down, so the hold
/// gesture on the record button keeps running underneath it.
struct VideoNoteOverlay: View {
    @ObservedObject var media: ChatMediaController
    @ObservedObject var live: MediaLiveState

    init(media: ChatMediaController) {
        self.media = media
        self.live = media.live
    }
    @StateObject private var preview = PreviewPlayer()

    private static let diameter: CGFloat = 280

    var body: some View {
        switch media.recState {
        case .recording(.videoNote, _, _, _), .locked(.videoNote, _), .finalizing(.videoNote),
             .preview(.videoNote, _):
            ZStack {
                // The dimming never takes a touch: the composer's Stop, Delete
                // and Send stay reachable underneath it.
                Color.black.opacity(0.35)
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
                VStack(spacing: 16) {
                    ZStack {
                        Circle().fill(Color.black)
                        if case .preview = media.recState, let take = media.previewTake {
                            PlayerLayerView(player: preview.player(for: take.fileURL))
                                .clipShape(Circle())
                                .onTapGesture { preview.toggleSound(media: media) }
                        } else {
                            CapturePreviewView(session: media.videoCaptureSession)
                                .clipShape(Circle())
                        }
                        ProgressRing(
                            fraction: Double(live.elapsedMs) / Double(RecordingMachine.videoNoteCapMs),
                            lineWidth: 5,
                            color: .red
                        )
                        .opacity(media.recState.isCapturing ? 1 : 0)
                    }
                    .frame(width: Self.diameter, height: Self.diameter)
                    // While a finger holds the record button, nothing here may
                    // take the touch that ends the gesture.
                    .allowsHitTesting(!isRecordingHeld)
                    .accessibilityLabel(isPreview ? "Video message preview" : "Recording video message")
                    if case .recording = media.recState {
                        Text("Slide to cancel")
                            .font(.footnote)
                            .foregroundStyle(.white)
                    }
                    if case .locked = media.recState {
                        Button { media.flipCamera() } label: {
                            Image(systemName: "arrow.triangle.2.circlepath.camera")
                                .font(.title2)
                                .foregroundStyle(.white)
                                .padding(12)
                                .background(Circle().fill(Color.black.opacity(0.5)))
                        }
                        .accessibilityLabel("Flip camera")
                    }
                    if isPreview {
                        Text(preview.withSound ? "Tap to mute" : "Tap to play with sound")
                            .font(.footnote)
                            .foregroundStyle(.white)
                    }
                }
            }
            .onDisappear { preview.stop(media: media) }
        default:
            EmptyView()
        }
    }

    private var isPreview: Bool {
        if case .preview = media.recState { return true }
        return false
    }

    private var isRecordingHeld: Bool {
        if case .recording = media.recState { return true }
        return false
    }

    /// The preview's looping player: muted until tapped.
    @MainActor
    final class PreviewPlayer: ObservableObject {
        @Published private(set) var withSound = false
        private var queue: AVQueuePlayer?
        private var looper: AVPlayerLooper?
        private var url: URL?

        func player(for url: URL) -> AVPlayer? {
            if self.url == url, let queue { return queue }
            let item = AVPlayerItem(asset: AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
            let player = AVQueuePlayer()
            player.isMuted = true
            looper = AVPlayerLooper(player: player, templateItem: item)
            queue = player
            self.url = url
            player.play()
            return player
        }

        func toggleSound(media: ChatMediaController) {
            guard let queue else { return }
            withSound.toggle()
            if withSound {
                _ = try? MediaAudioSession.begin(.playback, callActive: media.callActive)
                queue.seek(to: .zero)
            } else {
                MediaAudioSession.end(.playback)
            }
            queue.isMuted = !withSound
        }

        func stop(media: ChatMediaController) {
            queue?.pause()
            if withSound { MediaAudioSession.end(.playback) }
            withSound = false
            looper = nil
            queue = nil
            url = nil
        }
    }
}

/// UX spec §6 — the inline card shown when the permission a take needs is
/// denied, with Open Settings.
struct MediaDeniedCard: View {
    let resource: ChatMediaController.DeniedResource
    let onDismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: resource == .camera ? "video.slash" : "mic.slash")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 6) {
                Text(resource.copy)
                    .font(.footnote)
                Button("Open Settings") {
                    if let url = URL(string: UIApplication.openSettingsURLString) {
                        UIApplication.shared.open(url)
                    }
                }
                .font(.footnote.bold())
            }
            Spacer(minLength: 0)
            Button(action: onDismiss) {
                Image(systemName: "xmark")
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityLabel("Dismiss")
        }
        .padding(10)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
