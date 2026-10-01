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
///
/// Its touches come from UIKit (``RecordTouchSurface``) rather than from a
/// SwiftUI `DragGesture`, for the reason written there: only UIKit reports a
/// touch the system TOOK AWAY.
struct RecordButton: View {
    @ObservedObject var media: ChatMediaController
    @State private var pressed = false
    /// Nothing disables this button today — during a call it stays live and
    /// answers with the in-call hint — but a `UIViewRepresentable` does not
    /// inherit `.disabled()` the way a SwiftUI gesture does, so the touch
    /// surface is told, rather than left live behind a disabled face.
    @Environment(\.isEnabled) private var isEnabled

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
        .overlay {
            RecordTouchSurface(
                isEnabled: isEnabled,
                onBegan: {
                    pressed = true
                    media.press()
                },
                onMoved: { dx, dy in
                    media.move(dx: dx, dy: dy)
                },
                onEnded: {
                    pressed = false
                    media.release()
                },
                // The fourth state SwiftUI never reported.
                onCancelled: {
                    pressed = false
                    media.gestureLost()
                }
            )
            .accessibilityHidden(true)
        }
        .onDisappear {
            if pressed {
                pressed = false
                media.gestureLost()
            }
        }
        .accessibilityElement()
        .accessibilityLabel(Localization.text(isVideo ? "chat.record.video" : "chat.record.voice"))
        .accessibilityHint(
            media.callActive ? ChatMediaController.inCallCopy : Localization.text("chat.record.locked_hint"))
        .accessibilityAddTraits(.isButton)
        // §3.1 — hold and slide are not reachable with VoiceOver or Switch
        // Control, so the two gestures become two actions on the one button:
        // activating it starts a LOCKED take, and the named action below is
        // the only way such a user can reach the other mode at all.
        .accessibilityAction { media.activate() }
        .accessibilityAction(
            named: Localization.text(isVideo ? "chat.record.switch_to_voice" : "chat.record.switch_to_video")
        ) {
            media.toggleMode()
        }
    }
}

/// UX spec §3.1 — the record button's touch source, taken from UIKit.
///
/// A touch has FOUR outcomes and SwiftUI's `DragGesture` reports three: it
/// calls `onChanged` and `onEnded`, and it has nowhere to say that the system
/// TOOK the touch away — a Control Centre pull, an incoming call, the app
/// going to the background under a thumb. `onEnded` then never arrives, the
/// press is left in ``RecState/pressing``, the `holdToRecordMs` clock fires
/// anyway, and the press is promoted to a recording that no finger remains to
/// release. UIKit has always had the fourth state, so this one control leaves
/// SwiftUI for it and routes it to ``ChatMediaController/gestureLost()``.
///
/// Touches are taken RAW rather than through a `UIGestureRecognizer`: a
/// recognizer reports `.cancelled` too, but it adds a state machine of its own
/// (`.possible` / `.failed`, `allowableMovement`) between the finger and the
/// one this control already has.
struct RecordTouchSurface: UIViewRepresentable {
    let isEnabled: Bool
    let onBegan: () -> Void
    /// Travel from the press point, in points, on the same axes and with the
    /// same sign as the `DragGesture`'s `translation` this replaced — the
    /// machine's `tapSlopPoints` / `cancelSlidePoints` / `lockSlidePoints` all
    /// read it, so the measurement may not change meaning.
    let onMoved: (Double, Double) -> Void
    let onEnded: () -> Void
    let onCancelled: () -> Void

    final class TouchView: UIView {
        var isTouchEnabled = true
        var onBegan: (() -> Void)?
        var onMoved: ((Double, Double) -> Void)?
        var onEnded: (() -> Void)?
        var onCancelled: (() -> Void)?

        /// The one touch this control is following, and where it landed. In
        /// WINDOW coordinates, not this view's: the press point has to stay
        /// fixed while the button's own circle grows from 40 to 56 pt under
        /// the finger, which is also why the gesture this replaced measured in
        /// `.global`.
        private var tracked: UITouch?
        private var origin: CGPoint = .zero

        override init(frame: CGRect) {
            super.init(frame: frame)
            backgroundColor = .clear
            isMultipleTouchEnabled = false
            // The SwiftUI view above owns the accessibility element and both
            // of its actions; this layer must not become a second one.
            isAccessibilityElement = false
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { nil }

        /// Mirrors the `.contentShape(Circle())` the button carried while the
        /// gesture was SwiftUI's: the corners of the 44 pt frame keep falling
        /// through to the composer underneath, and a disabled button takes no
        /// touch at all.
        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
            guard isTouchEnabled else { return false }
            let radius = min(bounds.width, bounds.height) / 2
            guard radius > 0 else { return false }
            let dx = point.x - bounds.midX
            let dy = point.y - bounds.midY
            return dx * dx + dy * dy <= radius * radius
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard tracked == nil, let touch = touches.first else { return }
            tracked = touch
            origin = touch.location(in: nil)
            onBegan?()
        }

        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard let touch = tracked, touches.contains(touch) else { return }
            let point = touch.location(in: nil)
            onMoved?(point.x - origin.x, point.y - origin.y)
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard let touch = tracked, touches.contains(touch) else { return }
            tracked = nil
            onEnded?()
        }

        /// The whole point of this view.
        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard let touch = tracked, touches.contains(touch) else { return }
            tracked = nil
            onCancelled?()
        }
    }

    func makeUIView(context: Context) -> TouchView {
        let view = TouchView(frame: .zero)
        apply(to: view)
        return view
    }

    func updateUIView(_ view: TouchView, context: Context) {
        apply(to: view)
    }

    private func apply(to view: TouchView) {
        view.isTouchEnabled = isEnabled
        view.onBegan = onBegan
        view.onMoved = onMoved
        view.onEnded = onEnded
        view.onCancelled = onCancelled
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
                // §2 "Hearing nothing" — IN PLACE OF the hint, not beside it: two
                // competing lines in one slot would push the slide affordance
                // around mid-gesture, and the warning is the more urgent of the
                // two.
                if media.micSilent {
                    silentMicNotice
                } else {
                    // The catalog message carries its own leading glyph, so a
                    // right-to-left language points it the other way.
                    Text(Localization.text("chat.record.slide_cancel"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .offset(x: min(0, dx / 2))
                    .opacity(1 + max(-0.8, dx / RecordingMachine.cancelSlidePoints))
                }
            }
            .accessibilityElement(children: .combine)
            // The warning has to be IN the label, not merely on screen: combining
            // the children into one element and giving it an explicit label is
            // what makes VoiceOver read this row, so a `Text` added inside it is
            // never spoken on its own.
            .accessibilityLabel(recordingLabel)
        case .locked(let kind, _):
            HStack(spacing: 8) {
                Button(role: .destructive) { media.deleteTake() } label: {
                    Image(systemName: "trash")
                }
                .accessibilityLabel(Localization.text("chat.record.delete"))
                pulsingDot
                clock
                if kind == .voice {
                    LevelMeter(level: live.level).frame(width: 40, height: 18)
                }
                Spacer(minLength: 4)
                // A locked take is the one most worth warning about: the finger is
                // off the button and it runs until it is stopped, so a silent one
                // can waste minutes rather than seconds. Never reached by a video
                // note, whose level is not sampled at all, so it cannot be
                // accused. Unlike the row above, this one is left to VoiceOver as
                // it stands: combining its children into one label would take the
                // Delete and Stop buttons away.
                if media.micSilent { silentMicNotice }
            }
        case .finalizing:
            HStack(spacing: 8) {
                ProgressView()
                Text(Localization.text("chat.record.finishing")).font(.footnote).foregroundStyle(.secondary)
                Spacer()
            }
        case .preview(let kind, let durationMs):
            HStack(spacing: 10) {
                Button(role: .destructive) { media.deleteTake() } label: {
                    Image(systemName: "trash")
                }
                .accessibilityLabel(Localization.text("chat.record.delete"))
                if kind == .voice, let take = media.previewTake {
                    voicePreview(take: take, durationMs: durationMs)
                } else {
                    Text(Localization.text("chat.media.video_message"))
                        .font(.footnote)
                    Text(RecordingMachine.formatTotalClock(ms: durationMs))
                        .font(.footnote.monospacedDigit())
                    Spacer()
                }
            }
        case .idle, .pressing, .requesting:
            EmptyView()
        }
    }

    /// §2 "Hearing nothing" — the one place the warning is drawn, so the
    /// recording and locked bars cannot word or colour it two ways. An advisory
    /// style rather than a red one: nothing has failed and nothing is being
    /// refused.
    private var silentMicNotice: some View {
        Text(MicrophoneSilence.warningCopy)
            .font(.footnote)
            .foregroundStyle(.secondary)
    }

    /// What VoiceOver reads for the recording row, whose children are combined
    /// into one element — so the warning has to be said here or not at all.
    private var recordingLabel: String {
        let duration = RecordingMachine.spokenDuration(ms: live.elapsedMs)
        let recording = Localization.text("chat.record.recording_duration", texts: ["duration": duration])
        guard media.micSilent else { return recording }
        return Localization.joinList([recording, MicrophoneSilence.warningCopy])
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
                ? Localization.text("chat.record.pause")
                : Localization.text(
                    "chat.record.play", texts: ["duration": RecordingMachine.spokenDuration(ms: durationMs)])
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
                    .accessibilityLabel(
                        Localization.text(isPreview ? "chat.record.video_ready" : "chat.record.bar_video"))
                    if case .recording = media.recState {
                        Text(Localization.text("chat.record.slide_cancel"))
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
                        .accessibilityLabel(Localization.text("chat.record.flip_camera"))
                    }
                    if isPreview {
                        Text(Localization.text(preview.withSound ? "chat.video_note.tap_mute" : "chat.video_note.tap_sound"))
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
                Button(Localization.text("chat.record.open_settings")) {
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
            .accessibilityLabel(Localization.text("common.dismiss"))
        }
        .padding(10)
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
