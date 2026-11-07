// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import LakkoCore
import os
import UIKit

/// ADR-0047 — the per-chat media runtime: recording, the outgoing media send
/// queue, and playback. The iOS counterpart of the web `media-hub.ts` +
/// `recording-controller.ts` + the media-send jobs in `Chat.tsx`.
///
/// Owned by `ChatScreen` as a `@StateObject`, so it lives exactly as long as
/// the chat is on screen; its temp directory (decrypted media) is removed when
/// it goes away.
@MainActor
public final class ChatMediaController: ObservableObject {

    /// What the controller needs from the chat, as closures over long-lived
    /// objects (the repository), never over the view model, so the controller
    /// holds no reference back to the screen that owns it.
    public struct Port {
        /// Send one attachment as its own message (empty text), as a reply
        /// when a ``ReplyRef`` is handed along (ADR-0052).
        public let sendAttachment: (AttachmentRef, ReplyRef?) async throws -> Void
        public let sessionToken: () -> String?
        /// `docs/ux/message-actions.md` section 3.1 — "recording a voice
        /// message with a reply armed sends the take as the reply; the strip
        /// clears the same way". Called once per media message at the moment
        /// it is committed to send, so the take carries the reply that was
        /// armed then and a later retry re-sends the same reply. `nil` when
        /// nothing is armed.
        public let takeArmedReply: () -> ReplyRef?
        /// ADR-0087 section 13 — the idle life of the uploads (a channel's
        /// 45 days); `nil` for a chat.
        public let idleDays: UInt8?

        public init(
            sendAttachment: @escaping (AttachmentRef, ReplyRef?) async throws -> Void,
            sessionToken: @escaping () -> String?,
            takeArmedReply: @escaping () -> ReplyRef? = { nil },
            idleDays: UInt8? = nil
        ) {
            self.sendAttachment = sendAttachment
            self.sessionToken = sessionToken
            self.takeArmedReply = takeArmedReply
            self.idleDays = idleDays
        }
    }

    /// `docs/ux/feed.md` section 3 — a screen that only plays what was already
    /// sent or received. Nothing there records or uploads, so a send is
    /// refused rather than given a conversation to land in.
    public static var playbackOnlyPort: Port {
        Port(
            sendAttachment: { _, _ in throw MediaCaptureError.startFailed },
            sessionToken: { nil })
    }

    public enum DeniedResource: Equatable {
        case microphone
        case camera

        public var copy: String {
            switch self {
            case .microphone: return Localization.text("chat.record.denied_microphone_app")
            case .camera: return Localization.text("chat.record.denied_camera_app")
            }
        }
    }

    public static var inCallCopy: String { Localization.text("chat.record.in_call") }

    private static let log = Logger(subsystem: "me.lakko.media", category: "chat-media")

    private let port: Port
    public let prefs: MediaPrefs
    public let temp: MediaTempFiles
    /// Copied out of `temp` so the nonisolated `deinit` can remove it.
    private nonisolated let tempRoot: URL
    public lazy var gifClient = GifClient(sessionToken: port.sessionToken)

    // MARK: - Recording state

    @Published public private(set) var recordMode: CaptureKind
    @Published public private(set) var recState: RecState = .idle
    /// High-frequency state (the recording clock and level, the voice playback
    /// position) lives in ``live`` so its 10-20 Hz updates re-render only the
    /// bubbles and the recording bar, never the whole chat screen.
    public let live = MediaLiveState()
    public private(set) var elapsedMs: Int64 {
        get { live.elapsedMs }
        set { live.elapsedMs = newValue }
    }
    public private(set) var level: Double {
        get { live.level }
        set { live.level = newValue }
    }
    /// UX spec §2 "Hearing nothing" — the microphone has given nothing for
    /// ``MicrophoneSilence/warnAfterMs``, so the bar says so in place of its
    /// hint. Decided by ``MicrophoneSilence`` over the samples the live meter is
    /// already taking; nothing about the take changes because of it.
    ///
    /// Published on the controller rather than in ``live`` because it flips at
    /// most twice in a take, not twenty times a second — provided the assignment
    /// is guarded, which is why ``noteLevel(_:atMs:)`` compares before it writes.
    @Published public private(set) var micSilent: Bool = false
    @Published public private(set) var hint: String?
    @Published public private(set) var denied: DeniedResource?
    @Published public private(set) var previewTake: CapturedTake?
    /// The live capture session of a video note, for the round preview.
    @Published public private(set) var videoCaptureSession: AVCaptureSession?
    @Published public var callActive: Bool = false {
        didSet {
            guard callActive, !oldValue else { return }
            if recState != .idle { dispatch(.cancel) }
            stopPlayback()
        }
    }
    public let hasCamera: Bool

    /// UX spec §2 — the run of silence in the take being recorded now.
    private var silence = MicrophoneSilence.Watch()

    private var voiceRecorder: VoiceRecorder?
    private var videoRecorder: VideoNoteRecorder?
    private var ticker: Timer?
    /// UX spec §3.1 — the clock that turns a press into a hold. One per press,
    /// invalidated the moment the press ends however it ends, so a lifted
    /// finger can never open the microphone behind the tap it just performed.
    private var holdTimer: Timer?
    private var hintClear: Task<Void, Never>?

    // MARK: - Send queue

    public struct PendingSend: Identifiable, Equatable {
        public let id: Int
        /// Where the local bytes are filed until the blob id is known.
        public let localKey: String
        /// The ref the bubble renders from: media metadata only, empty blob id
        /// until uploaded.
        public var ref: AttachmentRef
        public var progress: Double
        public var failed: Bool
        /// Whether the local bytes are on disk yet (a GIF's arrive only after
        /// the proxy download), so the bubble knows to (re)load them.
        public var localReady: Bool
    }

    @Published public private(set) var pendingSends: [PendingSend] = []
    /// ADR-0065 / UX 4.2 — the last media send stopped at the sending limit:
    /// the composer says so. Cleared by the next send that starts.
    @Published public private(set) var sendLimited = false
    /// ADR-0065 section 6 — blobs uploaded for a send that has not gone out
    /// yet, per job. Read by `deinit` (the chat closed with the send
    /// abandoned), so it is written on the main actor only.
    private nonisolated(unsafe) var unsentBlobIds: [Int: [Data]] = [:]
    private var jobs: [Int: SendJob] = [:]
    private var nextSendId = 1

    // MARK: - Playback state

    public struct VoicePlayback: Equatable {
        public let key: String
        public var playing: Bool
        public var positionMs: Int64
        public var durationMs: Int64
    }

    public private(set) var voice: VoicePlayback? {
        get { live.voice }
        set { live.voice = newValue }
    }
    @Published public private(set) var loadingKeys: Set<String> = []
    @Published public private(set) var failedKeys: Set<String> = []
    /// ADR-0065 — why a key in ``failedKeys`` failed (gone, limited, or the
    /// generic failure), so a bubble shows the one honest state.
    @Published public private(set) var failureKinds: [String: MediaLoadFailure] = [:]
    @Published public private(set) var playedKeys: Set<String>
    /// The account ``playedKeys`` was loaded for, taken when this controller
    /// was made: its played marks are filed there and nowhere else.
    private let playedAccountIdB64: String?
    @Published public var rate: Float = 1 {
        didSet { if voicePlayer?.rate ?? 0 > 0 { voicePlayer?.rate = rate } }
    }

    private var voicePlayer: AVPlayer?
    private var voiceObserver: Any?
    private var voiceEndObserver: NSObjectProtocol?
    private var inflight: [String: Task<URL, Error>] = [:]
    /// The one thing playing with sound, and how to pause it.
    private var current: (key: String, pause: () -> Void)?
    private var timeline: [ChatMessage] = []

    public init(port: Port, prefs: MediaPrefs = MediaPrefs(), temp: MediaTempFiles? = nil) {
        self.port = port
        self.prefs = prefs
        let files = temp ?? MediaTempFiles()
        self.temp = files
        self.tempRoot = files.root
        self.recordMode = prefs.recordMode
        self.playedKeys = Set(prefs.playedKeys())
        self.playedAccountIdB64 = prefs.boundAccountIdB64
        self.hasCamera = VideoNoteRecorder.hasFrontCamera
        if recordMode == .videoNote, !hasCamera { recordMode = .voice }
    }

    deinit {
        // The whole chat's decrypted media goes with it.
        try? FileManager.default.removeItem(at: tempRoot)
        // ADR-0065 section 6 — an upload whose message was never sent is
        // deleted at once where the device still knows the id.
        let orphans = unsentBlobIds.values.flatMap { $0 }
        if !orphans.isEmpty {
            Task { @MainActor in BlobLifecycle.shared.delete(orphans) }
        }
    }

    /// The chat left the screen: stop what plays and what records.
    public func chatHidden() {
        stopPlayback()
        if recState != .idle { dispatch(.cancel) }
        if case .preview = recState { dispatch(.delete) }
    }

    // MARK: - Keys

    public static func key(for blobId: Data) -> String { AttachmentBridge.hex(blobId) }

    public static func key(for ref: AttachmentRef) -> String { key(for: ref.blobId) }

    public static func thumbKey(for thumb: ThumbRef) -> String { "thumb-" + key(for: thumb.blobId) }

    // MARK: - Gesture entry points

    private var now: Int64 { Int64(ProcessInfo.processInfo.systemUptime * 1000) }

    public func press() {
        guard !callActive else {
            flashHint(Self.inCallCopy)
            return
        }
        dispatch(.press(kind: recordMode, at: now, permission: permissionHint(for: recordMode)))
    }

    public func move(dx: Double, dy: Double) {
        dispatch(.move(dx: dx, dy: dy))
    }

    public func release() {
        dispatch(.release(at: now))
    }

    /// The record button left the view hierarchy with a finger still on it, so
    /// SwiftUI will send no further gesture events.
    ///
    /// A take already running is released exactly as a lifted finger would
    /// release it. A press that had not yet become a take is DROPPED rather
    /// than released: UX spec §3.1 makes a release inside the hold window the
    /// tap that switches mic / camera, and a button vanishing under a thumb is
    /// not a person asking for the other mode.
    public func gestureLost() {
        dispatch(recState.isPressed ? .cancel : .release(at: now))
    }

    /// VoiceOver / Switch Control: a single activation starts a locked take.
    public func activate() {
        guard !callActive else {
            flashHint(Self.inCallCopy)
            return
        }
        dispatch(.activate(kind: recordMode, at: now, permission: permissionHint(for: recordMode)))
    }

    public func stopLocked() { dispatch(.stop(at: now)) }
    public func deleteTake() { dispatch(.delete) }
    public func sendPreview() { dispatch(.send) }
    public func cancelGesture() { dispatch(.cancel) }

    public func toggleMode() {
        let next: CaptureKind = recordMode == .voice ? .videoNote : .voice
        if next == .videoNote, !hasCamera {
            flashHint(Localization.text("chat.record.no_camera"))
            return
        }
        recordMode = next
        prefs.recordMode = next
    }

    public func flipCamera() { videoRecorder?.flip() }

    public func dismissDenied() { denied = nil }

    // MARK: - Machine

    func dispatch(_ event: RecEvent) {
        let step = RecordingMachine.reduce(recState, event)
        let wasCapturing = recState.isCapturing
        recState = step.state
        if step.state.isCapturing, !wasCapturing { startTicker() }
        if !step.state.isCapturing { stopTicker() }
        // The press is over — whichever way it went. Dropping the clock here
        // rather than at each exit means no new exit can forget to.
        if !step.state.isPressed { stopHoldTimer() }
        for effect in step.effects { perform(effect) }
    }

    private func perform(_ effect: RecEffect) {
        switch effect {
        case .armHold:
            armHoldTimer()
        case .requestPermission(let kind):
            Task {
                let cameraOK = kind == .videoNote ? await CameraPermission.ensureGranted() : true
                let micOK = cameraOK ? await MicrophonePermission.ensureGranted() : false
                dispatch(.permissionResult(granted: cameraOK && micOK))
            }
        case .startCapture(let kind):
            startCapture(kind)
        case .finish(let disposition):
            finish(disposition)
        case .hint(let hint):
            flashHint(hint.copy)
        case .showDenied(let kind):
            denied = kind == .videoNote && CameraPermission.systemStatus() == .denied ? .camera : .microphone
        case .toggleMode:
            toggleMode()
        case .sendPreview:
            if let take = previewTake {
                previewTake = nil
                stopPlayback(key: Self.previewKey)
                enqueue(take: take)
            }
        case .discardPreview:
            if let take = previewTake {
                stopPlayback(key: Self.previewKey)
                try? FileManager.default.removeItem(at: take.fileURL)
            }
            previewTake = nil
        }
    }

    public static let previewKey = "preview"

    private func permissionHint(for kind: CaptureKind) -> PermissionHint {
        let mic = MicrophonePermission.systemStatus()
        let camera = kind == .videoNote ? CameraPermission.systemStatus() : .granted
        if mic == .denied || camera == .denied { return .denied }
        if mic == .granted && camera == .granted { return .granted }
        return .prompt
    }

    private var captureDirectory: URL { temp.root.appendingPathComponent("capture", isDirectory: true) }

    private func startCapture(_ kind: CaptureKind) {
        stopPlayback()
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        elapsedMs = 0
        level = 0
        // Per take: the previous take's silence says nothing about this one, and a
        // warning left standing from it would be about a microphone that is no
        // longer being asked anything.
        silence = MicrophoneSilence.Watch()
        micSilent = false
        switch kind {
        case .voice:
            let recorder = VoiceRecorder()
            recorder.onLevel = { [weak self] level, recordedMs in
                self?.noteLevel(level, atMs: recordedMs)
            }
            recorder.onInterrupted = { [weak self] in self?.captureInterrupted() }
            do {
                try recorder.start(directory: captureDirectory)
                voiceRecorder = recorder
            } catch {
                Self.log.warning("voice capture failed to start")
                flashHint(Localization.text("chat.record.start_failed"))
                dispatch(.captureFailed)
            }
        case .videoNote:
            let recorder = VideoNoteRecorder()
            recorder.onInterrupted = { [weak self] in self?.captureInterrupted() }
            videoRecorder = recorder
            videoCaptureSession = recorder.session
            Task {
                do {
                    try await recorder.start(directory: captureDirectory)
                } catch {
                    Self.log.warning("video note capture failed to start")
                    guard self.videoRecorder === recorder else { return }
                    self.videoRecorder = nil
                    self.videoCaptureSession = nil
                    recorder.discard()
                    self.flashHint(Localization.text("chat.record.start_failed"))
                    self.dispatch(.captureFailed)
                }
            }
        }
    }

    /// One level reading from the live recorder, taken at `atMs` of its own
    /// elapsed time: it drives the meter and, through ``MicrophoneSilence``,
    /// §2's "Hearing nothing" warning.
    ///
    /// The `warns` comparison before the assignment is load-bearing: `@Published`
    /// fires on every write, equal or not, and this runs twenty times a second,
    /// so writing unconditionally would re-render the chat screen at the meter's
    /// cadence for a value that changes at most twice in a take.
    private func noteLevel(_ level: Double, atMs: Int64) {
        self.level = level
        silence = MicrophoneSilence.observe(silence, level: level, atMs: atMs)
        if silence.warns != micSilent { micSilent = silence.warns }
    }

#if DEBUG
    /// DEBUG-only seam for the `LakkoUITests` fixture
    /// (``UiTestFixture/micSilence``), and for nothing else.
    ///
    /// Why it exists. `recState` is `private(set)` and its only writer is
    /// ``dispatch(_:)``, which performs the machine's effects — so the only way
    /// into the state where the recording bar is drawn is to open a real
    /// microphone. That left UX spec §2's `No sound from the microphone` notice
    /// unobservable on screen by any test: the alternative is to record for two
    /// seconds and hope the host machine's microphone happens to hear nothing,
    /// which measures the room rather than the app. Android covers the same
    /// notice with a Compose UI test because its bar takes the state as an
    /// argument; this is the iOS equivalent of that argument.
    ///
    /// It performs NO effect, so nothing it does can start a recorder, arm a
    /// timer or ask for a permission, and `#if DEBUG` keeps it out of a Release
    /// build entirely.
    func debugSetRecState(_ state: RecState) {
        recState = state
    }

    /// DEBUG-only seam, as above: feed one level reading through the PRODUCTION
    /// silence fold.
    ///
    /// Deliberately not a setter for `micSilent`. The fixture's samples run
    /// through ``noteLevel(_:atMs:)`` itself, so what a UI test then sees on
    /// screen was decided by ``MicrophoneSilence/observe(_:level:atMs:)`` and
    /// the real `warns`-to-`micSilent` assignment — the one link in that chain
    /// that no unit test can reach.
    func debugNoteLevel(_ level: Double, atMs: Int64) {
        noteLevel(level, atMs: atMs)
    }
#endif

    private func captureInterrupted() {
        guard recState.isCapturing else { return }
        let kind = recState.kind ?? .voice
        let revoked = MicrophonePermission.systemStatus() == .denied
            || (kind == .videoNote && CameraPermission.systemStatus() == .denied)
        dispatch(revoked ? .revoked : .captureFailed)
    }

    private func finish(_ disposition: FinishDisposition) {
        if let recorder = voiceRecorder {
            voiceRecorder = nil
            switch disposition {
            case .discard:
                recorder.discard()
            case .send:
                if let take = recorder.stop() { acceptForSend(take) }
            case .preview:
                if let take = recorder.stop() {
                    previewTake = take
                    dispatch(.previewReady(durationMs: take.durationMs))
                } else {
                    dispatch(.previewFailed)
                }
            }
        } else if let recorder = videoRecorder {
            videoRecorder = nil
            switch disposition {
            case .discard:
                videoCaptureSession = nil
                recorder.discard()
            case .send, .preview:
                Task {
                    let take = await recorder.stop()
                    self.videoCaptureSession = nil
                    if disposition == .send {
                        if let take { self.acceptForSend(take) } else { self.flashHint(RecordingHint.holdToRecord.copy) }
                    } else if let take {
                        self.previewTake = take
                        self.dispatch(.previewReady(durationMs: take.durationMs))
                    } else {
                        self.dispatch(.previewFailed)
                    }
                }
            }
        }
    }

    /// A released take is sent only if the FILE is long enough: the machine
    /// measured the finger, but a camera that took a moment to start may have
    /// written less than that.
    private func acceptForSend(_ take: CapturedTake) {
        guard take.durationMs >= RecordingMachine.minTakeMs else {
            try? FileManager.default.removeItem(at: take.fileURL)
            flashHint(RecordingHint.holdToRecord.copy)
            return
        }
        enqueue(take: take)
    }

    private func startTicker() {
        stopTicker()
        ticker = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let started = self.recState.startedAt else { return }
                let t = self.now
                self.elapsedMs = t - started
                self.dispatch(.tick(at: t))
            }
        }
    }

    private func stopTicker() {
        ticker?.invalidate()
        ticker = nil
    }

    /// UX spec §3.1 — arm the tap / hold clock for the press just registered.
    private func armHoldTimer() {
        stopHoldTimer()
        holdTimer = Timer.scheduledTimer(
            withTimeInterval: Double(RecordingMachine.holdToRecordMs) / 1000,
            repeats: false
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.holdTimer = nil
                // Belt and braces: the timer is invalidated on every exit from
                // `pressing`, so this can only be a press still held down.
                guard self.recState.isPressed else { return }
                self.dispatch(.holdElapsed(at: self.now))
            }
        }
    }

    private func stopHoldTimer() {
        holdTimer?.invalidate()
        holdTimer = nil
    }

    private func flashHint(_ text: String) {
        hint = text
        hintClear?.cancel()
        hintClear = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_800_000_000)
            guard !Task.isCancelled else { return }
            self?.hint = nil
        }
    }

    // MARK: - Sending

    private final class SendJob {
        let id: Int
        let localKey: String
        var name: String
        var mime: String
        var meta: MediaAttachmentMeta
        var take: CapturedTake?
        var gif: GifItem?
        var fileURL: URL?
        var thumb: ThumbRef?
        var posterTried = false
        var uploaded: AttachmentRef?
        /// ADR-0052 — the reply this media message answers, taken from the
        /// composer when the job was committed.
        var reply: ReplyRef?

        init(id: Int, localKey: String, name: String, mime: String, meta: MediaAttachmentMeta) {
            self.id = id
            self.localKey = localKey
            self.name = name
            self.mime = mime
            self.meta = meta
        }
    }

    private static func stamp() -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date())
    }

    private func enqueue(take: CapturedTake) {
        let id = nextSendId
        nextSendId += 1
        let isVoice = take.kind == .voice
        let clamped = UInt32(clamping: min(take.durationMs, RecordingMachine.cap(for: take.kind)))
        var meta = MediaAttachmentMeta(kind: take.kind.rawValue, durationMs: max(1, clamped))
        if isVoice {
            meta.waveform = take.waveform
        } else if let side = take.side {
            meta.width = UInt32(side)
            meta.height = UInt32(side)
        }
        let job = SendJob(
            id: id,
            localKey: "local-\(id)",
            name: isVoice ? "voice-\(Self.stamp()).m4a" : "video-\(Self.stamp()).mp4",
            mime: take.mime,
            meta: meta
        )
        job.take = take
        do {
            job.fileURL = try temp.adopt(take.fileURL, key: job.localKey, name: job.name, fallback: job.name)
        } catch {
            job.fileURL = take.fileURL
        }
        start(job)
    }

    /// UX spec §5 — a tapped GIF is sent immediately: its full rendition is
    /// fetched through the proxy, encrypted and uploaded like any file, so a
    /// recipient only ever fetches the encrypted blob from Lakko storage.
    public func sendGif(_ item: GifItem) {
        let id = nextSendId
        nextSendId += 1
        let job = SendJob(
            id: id,
            localKey: "local-\(id)",
            name: "gif-\(item.id).gif",
            mime: "image/gif",
            meta: MediaAttachmentMeta(kind: "gif", width: UInt32(item.width), height: UInt32(item.height))
        )
        job.gif = item
        start(job)
    }

    private func start(_ job: SendJob) {
        // The reply is taken HERE, when the message is committed, and not
        // when the upload lands: the strip clears at the moment the person
        // sent, as it does for a text message, and a retry of a failed
        // upload keeps quoting what was quoted.
        job.reply = port.takeArmedReply()
        jobs[job.id] = job
        pendingSends.append(PendingSend(
            id: job.id,
            localKey: job.localKey,
            ref: placeholderRef(job),
            progress: 0,
            failed: false,
            localReady: job.fileURL != nil
        ))
        Task { await run(job) }
    }

    public func retry(sendId: Int) {
        guard let job = jobs[sendId] else { return }
        Task { await run(job) }
    }

    private func placeholderRef(_ job: SendJob) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(),
            key: Data(),
            nonce: Data(),
            mime: job.mime,
            size: 0,
            name: job.name,
            kind: job.meta.kind,
            durationMs: job.meta.durationMs,
            waveform: job.meta.waveform,
            width: job.meta.width,
            height: job.meta.height,
            thumb: nil
        )
    }

    private func updatePending(_ id: Int, _ change: (inout PendingSend) -> Void) {
        guard let i = pendingSends.firstIndex(where: { $0.id == id }) else { return }
        change(&pendingSends[i])
    }

    private func run(_ job: SendJob) async {
        updatePending(job.id) {
            $0.failed = false
            $0.progress = 0
        }
        sendLimited = false
        do {
            if job.uploaded == nil {
                if job.fileURL == nil, let gif = job.gif {
                    switch await gifClient.fetchMedia(gif.fullURL, maxBytes: GifClient.maxFullBytes) {
                    case .ok(let media):
                        job.mime = media.mime
                        let ext = media.mime == "image/webp" ? "webp" : media.mime == "video/mp4" ? "mp4" : "gif"
                        job.name = "gif-\(gif.id).\(ext)"
                        job.fileURL = try temp.write(media.bytes, key: job.localKey, name: job.name, fallback: job.name)
                        updatePending(job.id) {
                            $0.ref = placeholderRef(job)
                            $0.localReady = true
                        }
                    case .failure(let failure):
                        throw failure
                    }
                }
                if let poster = job.take?.poster, job.thumb == nil, !job.posterTried {
                    job.posterTried = true
                    do {
                        let side = job.take?.posterSide.map { UInt32($0) }
                        let thumb = try await AttachmentBridge.encryptAndUploadThumb(
                            bytes: poster,
                            mime: "image/jpeg",
                            width: side,
                            height: side
                        )
                        job.thumb = thumb
                        unsentBlobIds[job.id, default: []].append(thumb.blobId)
                        // The poster is ours already: file it so the sent
                        // bubble shows it without downloading its own upload.
                        _ = try? temp.write(poster, key: Self.thumbKey(for: thumb), name: "poster.jpg", fallback: "poster.jpg")
                    } catch {
                        // ADR-0047 open question, current answer: a failed
                        // poster upload sends the note without one; the
                        // bubble shows a neutral circle.
                        Self.log.warning("video note poster upload failed; sending without a poster")
                    }
                }
                guard let fileURL = job.fileURL else { throw MediaCaptureError.startFailed }
                var meta = job.meta
                meta.thumb = job.thumb
                let id = job.id
                job.uploaded = try await AttachmentBridge.encryptAndUpload(
                    fileURL: fileURL,
                    mime: job.mime,
                    name: job.name,
                    media: meta,
                    idleDays: port.idleDays,
                    onProgress: { fraction in
                        Task { @MainActor [weak self] in
                            self?.updatePending(id) { $0.progress = fraction }
                        }
                    }
                )
            }
            guard let uploaded = job.uploaded else { return }
            if !(unsentBlobIds[job.id]?.contains(uploaded.blobId) ?? false) {
                unsentBlobIds[job.id, default: []].append(uploaded.blobId)
            }
            try await port.sendAttachment(uploaded, job.reply)
            unsentBlobIds.removeValue(forKey: job.id)
            // File the local bytes under the real blob id before the pending
            // row goes, so the sent bubble plays them without a download.
            temp.alias(Self.key(for: uploaded), to: job.localKey)
            jobs.removeValue(forKey: job.id)
            pendingSends.removeAll { $0.id == job.id }
        } catch {
            Self.log.warning("media message not sent")
            if case AttachmentBridgeError.uploadLimited = error { sendLimited = true }
            updatePending(job.id) { $0.failed = true }
        }
    }

    // MARK: - Timeline + played

    /// The chat's messages, for auto-continue.
    public func updateTimeline(_ messages: [ChatMessage]) {
        timeline = messages
    }

    public func isPlayed(_ key: String) -> Bool { playedKeys.contains(key) }

    /// Read the played marks again. A chat reads them when it opens; a screen
    /// that outlives the chats opened from it reads them each time it returns.
    public func reloadPlayed() {
        playedKeys = Set(prefs.playedKeys())
    }

    public func markPlayed(_ key: String) {
        guard !playedKeys.contains(key) else { return }
        playedKeys.insert(key)
        // Filed under the account this controller's played set was loaded
        // for, never under whoever is bound when playback ends.
        prefs.markPlayed(key, accountIdB64: playedAccountIdB64)
    }

    // MARK: - Media files

    /// The decrypted media file for `ref`, fetched and authenticated once per
    /// blob however many bubbles ask. Only an allowlisted media class is ever
    /// fetched for inline playback.
    public func mediaFile(for ref: AttachmentRef) async throws -> URL {
        let key = Self.key(for: ref)
        if let url = temp.url(for: key) { return url }
        if let url = MediaSessionCache.shared.videoFile(key) { return url }
        guard InlineMedia.decision(for: ref).bubble != .file else { throw MediaCaptureError.startFailed }
        return try await single(key) { [temp] in
            let bytes = try await AttachmentBridge.downloadAndDecrypt(ref: ref)
            return try temp.write(
                bytes,
                key: key,
                name: ref.name,
                fallback: "media-\(key.prefix(8))"
            )
        }
    }

    /// The decrypted poster for `thumb`, or `nil` when it is not an allowlisted
    /// image or fails to download / authenticate (the bubble then shows a
    /// neutral circle).
    public func posterFile(for thumb: ThumbRef) async -> URL? {
        guard InlineMedia.isImageThumb(thumb) else { return nil }
        let key = Self.thumbKey(for: thumb)
        if let url = temp.url(for: key) { return url }
        return try? await single(key) { [temp] in
            let bytes = try await AttachmentBridge.downloadAndDecrypt(thumb: thumb)
            return try temp.write(bytes, key: key, name: "poster.jpg", fallback: "poster.jpg")
        }
    }

    /// The decrypted file of a received video note, or `nil` when it cannot
    /// be fetched or authenticated. The download is owned by the controller
    /// (``single(_:_:)``), not by the bubble, so it survives the row being
    /// recycled; `loadingKeys` tells any bubble for `key` that it is under way.
    public func videoNoteFile(for ref: AttachmentRef, key: String) async -> URL? {
        if let url = temp.url(for: key) { return url }
        loadingKeys.insert(key)
        defer { loadingKeys.remove(key) }
        do {
            let url = try await mediaFile(for: ref)
            failureKinds.removeValue(forKey: key)
            return url
        } catch {
            Self.log.warning("video note download failed")
            failureKinds[key] = MediaLoadFailure.of(error)
            return nil
        }
    }

    /// ADR-0065 — remember why `key` failed to load (GIF, photo paths that do
    /// not go through the voice / video-note paths above).
    public func noteFailure(_ key: String, error: Error) {
        failureKinds[key] = MediaLoadFailure.of(error)
    }

    public func failure(_ key: String) -> MediaLoadFailure {
        failureKinds[key] ?? .failed
    }

    /// A local key (an outgoing take not yet uploaded) resolves to its file.
    public func localFile(for key: String) -> URL? { temp.url(for: key) }

    /// Fetch and decrypt a received voice message's audio ahead of the press.
    ///
    /// Every other media bubble warms itself when it comes into view; the voice
    /// bubble did not, so the first press paid the storage round trip and the
    /// AEAD open before a sample could be decoded and the message seemed to
    /// open on silence. The download is owned by ``single(_:_:)``, so a press
    /// that lands while this is still running joins it instead of starting a
    /// second one.
    ///
    /// Deliberately silent in both directions: it never touches `loadingKeys`
    /// (a spinner over the play button of every visible bubble would say the
    /// person is waiting when they are not) and never touches `failedKeys` (a
    /// warm that failed has shown nobody anything, and the press retries).
    public func prefetchVoice(ref: AttachmentRef, key: String) async {
        guard temp.url(for: key) == nil else { return }
        _ = try? await mediaFile(for: ref)
    }

    private func single(_ key: String, _ work: @escaping @MainActor () async throws -> URL) async throws -> URL {
        if let task = inflight[key] { return try await task.value }
        let task = Task { @MainActor in try await work() }
        inflight[key] = task
        defer { inflight.removeValue(forKey: key) }
        return try await task.value
    }

    // MARK: - One-at-a-time

    /// Become the only thing playing with sound; the previous player pauses.
    public func claim(_ key: String, pause: @escaping () -> Void) {
        if let prev = current, prev.key != key {
            current = nil
            prev.pause()
        }
        current = (key, pause)
    }

    public func releaseClaim(_ key: String) {
        if current?.key == key { current = nil }
    }

    // MARK: - Voice playback

    /// Toggle the voice message filed under `key`. `ref` is `nil` for a local
    /// file (an outgoing take, the preview).
    public func toggleVoice(key: String, ref: AttachmentRef?, fileURL: URL? = nil) {
        if let v = voice, v.key == key, let player = voicePlayer {
            if v.playing {
                player.pause()
                voice?.playing = false
                releaseClaim(key)
                MediaAudioSession.end(.playback)
            } else {
                resumeVoice(player, key: key)
            }
            return
        }
        Task { await startVoice(key: key, ref: ref, fileURL: fileURL, from: 0) }
    }

    public func seekVoice(key: String, fraction: Double) {
        guard voice?.key == key, let player = voicePlayer, let v = voice, v.durationMs > 0 else { return }
        let ms = Int64(Double(v.durationMs) * min(1, max(0, fraction)))
        player.seek(to: CMTime(value: ms, timescale: 1000), toleranceBefore: .zero, toleranceAfter: .zero)
        voice?.positionMs = ms
    }

    private func resumeVoice(_ player: AVPlayer, key: String) {
        _ = try? MediaAudioSession.begin(.playback, callActive: callActive)
        claim(key) { [weak self] in
            self?.voicePlayer?.pause()
            self?.voice?.playing = false
        }
        player.playImmediately(atRate: rate)
        voice?.playing = true
    }

    private func startVoice(key: String, ref: AttachmentRef?, fileURL: URL?, from: Int64) async {
        stopPlayback()
        failedKeys.remove(key)
        failureKinds.removeValue(forKey: key)
        let url: URL
        if let fileURL {
            url = fileURL
        } else if let local = temp.url(for: key) {
            url = local
        } else if let ref {
            loadingKeys.insert(key)
            defer { loadingKeys.remove(key) }
            do {
                url = try await mediaFile(for: ref)
            } catch {
                failureKinds[key] = MediaLoadFailure.of(error)
                failedKeys.insert(key)
                return
            }
        } else {
            failedKeys.insert(key)
            return
        }
        let asset = AVURLAsset(url: url, options: [AVURLAssetOverrideMIMETypeKey: "audio/mp4"])
        let item = AVPlayerItem(asset: asset)
        let player = AVPlayer(playerItem: item)
        // The source is always a decrypted file already on disk, so there is
        // nothing to buffer ahead of; left on, AVPlayer delays the start to
        // build a buffer it does not need.
        player.automaticallyWaitsToMinimizeStalling = false
        voicePlayer = player
        let knownDuration = ref?.durationMs.map(Int64.init) ?? previewTake?.durationMs ?? 0
        voice = VoicePlayback(key: key, playing: true, positionMs: 0, durationMs: knownDuration)
        voiceObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(value: 1, timescale: 20),
            queue: .main
        ) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self, self.voice?.key == key else { return }
                self.voice?.positionMs = Int64(CMTimeGetSeconds(time) * 1000)
                if let d = player.currentItem?.duration, d.isNumeric, CMTimeGetSeconds(d) > 0 {
                    self.voice?.durationMs = Int64(CMTimeGetSeconds(d) * 1000)
                }
                if player.currentItem?.status == .failed {
                    self.failedKeys.insert(key)
                    self.stopPlayback()
                }
            }
        }
        voiceEndObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.voiceEnded(key: key) }
        }
        if key != Self.previewKey, ref != nil { markPlayed(key) }
        if from > 0 { await player.seek(to: CMTime(value: from, timescale: 1000)) }
        resumeVoice(player, key: key)
    }

    private func voiceEnded(key: String) {
        stopPlayback()
        guard key != Self.previewKey else { return }
        let items = timeline.flatMap { message -> [PlaybackQueue.Item] in
            message.attachments.map { ref in
                let k = Self.key(for: ref)
                return PlaybackQueue.Item(
                    key: k,
                    isVoice: !message.deleted && InlineMedia.decision(for: ref).bubble == .voice,
                    senderKey: message.authorKeyB64,
                    played: playedKeys.contains(k)
                )
            }
        }
        guard let next = PlaybackQueue.nextVoiceToPlay(items, finishedKey: key),
              let ref = timeline.lazy.flatMap(\.attachments).first(where: { Self.key(for: $0) == next }) else {
            return
        }
        Task { await startVoice(key: next, ref: ref, fileURL: nil, from: 0) }
    }

    /// Stop whatever voice message plays (and ask any video to pause).
    public func stopPlayback(key: String? = nil) {
        if let key, voice?.key != key { return }
        if let prev = current {
            current = nil
            prev.pause()
        }
        if let voiceObserver { voicePlayer?.removeTimeObserver(voiceObserver) }
        voiceObserver = nil
        if let voiceEndObserver { NotificationCenter.default.removeObserver(voiceEndObserver) }
        voiceEndObserver = nil
        voicePlayer?.pause()
        voicePlayer = nil
        voice = nil
        MediaAudioSession.end(.playback)
    }
}

/// See ``ChatMediaController/live``.
@MainActor
public final class MediaLiveState: ObservableObject {
    @Published public var elapsedMs: Int64 = 0
    @Published public var level: Double = 0
    @Published public var voice: ChatMediaController.VoicePlayback?
}

extension GifFailure: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notConfigured, .unavailable, .unauthorized: return GifCopy.unavailable
        case .rateLimited: return GifCopy.rateLimited
        }
    }
}
