// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import AVKit
import Combine
import CoreMedia
import CoreVideo
import SwiftUI
import UIKit
import WebRTC

/// The system's own floating window for a call while the app is not on the
/// screen (`docs/ux/calls.md` section 4.5.4): iOS's video-call
/// picture-in-picture, started by itself when the app goes to the background
/// during a call that is placed, connecting or connected (never only ringing),
/// whether the call was full screen or collapsed.
///
/// It shows what the collapsed window shows (``CallWindowContent``): the call
/// screen's stage person, the own inset while the camera sends, the name and
/// `+N`. Its controls are the system's own (back to the app, close); mute is in
/// the system call controls the call already has through CallKit. Closing it
/// never ends the call.
///
/// The picture is drawn from frames through an `AVSampleBufferDisplayLayer`
/// (``SampleBufferVideoRenderer``), not WebRTC's Metal view: the content of a
/// picture-in-picture window is rendered out of process, and a Metal-backed
/// view is not guaranteed to update there.
///
/// Owned by ``CallHostOverlay``, which is mounted once at the signed-in root.
/// A clear ``PipSourceView`` sits behind the app's content and another behind
/// the call screen, so whichever is on top gives the system a view to animate
/// the window out of (``PipSourceViewHost``).
@MainActor
final class CallPictureInPicture: NSObject, ObservableObject {

    /// The clear views the system may animate the window out of, in the
    /// order they joined a window. The newest one still in a window is used:
    /// the call screen's while it is up, the app's otherwise.
    private var sourceViews: [WeakSourceView] = []
    private weak var activeSource: PipSourceView?

    private var controller: AVPictureInPictureController?
    private let callController = AVPictureInPictureVideoCallViewController()
    private let model = PipCallModel()
    private let stageRenderer = SampleBufferVideoRenderer()
    private let ownRenderer = SampleBufferVideoRenderer()
    private var observation: AnyCancellable?
    private weak var state: CallUiState?
    private var groupCall = false
    private var live = false
    /// iOS has interrupted the camera (the app is not in front and may not
    /// keep it). The own inset then shows nothing rather than a frozen frame.
    private var cameraInterrupted = false
    private var interruptionObservers: [NSObjectProtocol] = []
    /// `isPictureInPicturePossible` of the current controller, logged when it
    /// changes: the system says nothing when it declines to start the window.
    private var possibleObservation: NSKeyValueObservation?

    override init() {
        super.init()
        model.stageView = stageRenderer.view
        model.ownView = ownRenderer.view
        let hosting = UIHostingController(rootView: PipCallView(model: model))
        hosting.view.backgroundColor = .black
        callController.addChild(hosting)
        callController.view.addSubview(hosting.view)
        hosting.view.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            hosting.view.leadingAnchor.constraint(equalTo: callController.view.leadingAnchor),
            hosting.view.trailingAnchor.constraint(equalTo: callController.view.trailingAnchor),
            hosting.view.topAnchor.constraint(equalTo: callController.view.topAnchor),
            hosting.view.bottomAnchor.constraint(equalTo: callController.view.bottomAnchor),
        ])
        hosting.didMove(toParent: callController)
        callController.preferredContentSize = Self.personContentSize
        // While a call runs the only camera capture is the call's (the
        // composer records nothing during a call, section 4.5.5), so any
        // capture session's interruption is the call camera's.
        let center = NotificationCenter.default
        for (name, interrupted) in [
            (AVCaptureSession.wasInterruptedNotification, true),
            (AVCaptureSession.interruptionEndedNotification, false),
        ] {
            interruptionObservers.append(center.addObserver(
                forName: name, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.cameraInterrupted = interrupted
                    self?.refresh()
                }
            })
        }
        // What the system window can do when the app leaves the screen: the
        // system starts it or declines silently, so the state is logged.
        interruptionObservers.append(center.addObserver(
            forName: UIApplication.willResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.logState("app leaving the screen") }
        })
    }

    /// One line on whether the system window could start now, and why not.
    private func logState(_ moment: String) {
        guard live else { return }
        let source = activeSource
        NSLog(
            "call pip: %@: controller=%@ possible=%@ auto=%@ active=%@ source=%@ sourceSize=%.0fx%.0f",
            moment,
            controller == nil ? "none" : "yes",
            controller?.isPictureInPicturePossible == true ? "yes" : "no",
            controller?.canStartPictureInPictureAutomaticallyFromInline == true ? "yes" : "no",
            controller?.isPictureInPictureActive == true ? "yes" : "no",
            source == nil ? "none" : (source?.window == nil ? "not-in-window" : "in-window"),
            source?.bounds.width ?? 0, source?.bounds.height ?? 0)
    }

    /// A person's camera is upright on a phone.
    private static let personContentSize = CGSize(width: 720, height: 960)
    private static let screenContentSize = CGSize(width: 1280, height: 720)

    /// Follow the live call, or none.
    ///
    /// - Parameters:
    ///   - state: the call's state, the same object the call screen and the
    ///     collapsed window read.
    ///   - live: the call has a window at all (``CallWindowContent/hasWindow(_:)``):
    ///     placed, connecting, connected or reconnecting. A call that is only
    ///     ringing, or none, starts no system window.
    func follow(
        state: CallUiState?, groupCall: Bool, label: String, peerKey: Data?, live: Bool
    ) {
        let liveNow = live && state != nil
        if self.state !== state {
            self.state = state
            observation = state?.objectWillChange
                // `objectWillChange` fires before the change; reading on the
                // next turn of the main queue reads the new value.
                .receive(on: DispatchQueue.main)
                .sink { [weak self] _ in
                    MainActor.assumeIsolated { self?.refresh() }
                }
        }
        self.groupCall = groupCall
        model.label = label
        model.peerKey = peerKey
        if liveNow != self.live {
            NSLog("call pip: %@", liveNow ? "armed for a live call" : "disarmed, no live call")
        }
        self.live = liveNow
        if liveNow {
            makeController()
        } else {
            dropController()
        }
        refresh()
    }

    /// The system controller exists only while a call is live. On a phone
    /// with no call it was seen starting the window by itself when the app
    /// left the screen (an iPhone on iOS 27.2, 2026-11-10), with
    /// `canStartPictureInPictureAutomaticallyFromInline` false as far as the
    /// app's own state says; with no controller there is nothing to start.
    private func makeController() {
        guard let source = activeSource else { return }
        if let controller {
            controller.canStartPictureInPictureAutomaticallyFromInline = true
            return
        }
        guard AVPictureInPictureController.isPictureInPictureSupported() else {
            NSLog("call pip: picture in picture not supported on this device")
            return
        }
        let pip = AVPictureInPictureController(
            contentSource: AVPictureInPictureController.ContentSource(
                activeVideoCallSourceView: source, contentViewController: callController))
        pip.delegate = self
        pip.canStartPictureInPictureAutomaticallyFromInline = true
        controller = pip
        possibleObservation = pip.observe(\.isPictureInPicturePossible, options: [.initial, .new]) {
            controller, _ in
            let possible = controller.isPictureInPicturePossible
            DispatchQueue.main.async {
                NSLog("call pip: possible=%@", possible ? "yes" : "no")
            }
        }
    }

    /// Stop the window if it is up and let the controller go.
    private func dropController() {
        guard let controller else { return }
        controller.canStartPictureInPictureAutomaticallyFromInline = false
        if controller.isPictureInPictureActive { controller.stopPictureInPicture() }
        controller.delegate = nil
        possibleObservation = nil
        self.controller = nil
    }

    /// A source view joined or left a window.
    func sourceViewMoved(_ view: PipSourceView) {
        sourceViews.removeAll { $0.view == nil || $0.view === view }
        if view.window != nil { sourceViews.append(WeakSourceView(view: view)) }
        guard let next = sourceViews.last(where: { $0.view?.window != nil })?.view,
            next !== activeSource
        else { return }
        activeSource = next
        // A new source view gets a new controller rather than a new content
        // source on the old one: with the content source replaced, the system
        // reported the window possible and never started it when the app left
        // the screen (an iPhone on iOS 27.2, 2026-11-10). Not while the window
        // is up: it keeps its view until it closes.
        guard live, controller?.isPictureInPictureActive != true else { return }
        dropController()
        makeController()
        logState("source view changed")
    }

    /// Recompute what the window shows from the call's state.
    private func refresh() {
        guard live, let state else {
            stageRenderer.attach(nil)
            ownRenderer.attach(nil)
            model.update(shown: nil, stageHasVideo: false, ownHasVideo: false, screen: false)
            return
        }
        let shown = CallWindowContent.shown(state: state, groupCall: groupCall)
        let stageTrack = pictureTrack(shown: shown, state: state)
        // The own inset only while the camera really sends: an interrupted
        // camera (iOS stops it for a background app) shows nothing rather than
        // a frozen frame.
        let ownTrack = shown.showsOwnInset && !cameraInterrupted ? state.localVideoTrack : nil
        stageRenderer.mirrored = shown.stage?.isLocal == true && state.settings.mirrorLocal
        ownRenderer.mirrored = state.settings.mirrorLocal
        stageRenderer.attach(stageTrack)
        ownRenderer.attach(ownTrack)
        model.update(
            shown: shown, stageHasVideo: stageTrack != nil, ownHasVideo: ownTrack != nil,
            screen: shown.isScreenShare)
        let size = shown.isScreenShare ? Self.screenContentSize : Self.personContentSize
        if callController.preferredContentSize != size {
            callController.preferredContentSize = size
        }
    }

    /// The track the window draws for the person shown, or `nil` for their
    /// avatar. In a group call the light picture when the person sends one:
    /// it always arrives and is the right size for a small window.
    private func pictureTrack(shown: CallWindowShown, state: CallUiState) -> RTCVideoTrack? {
        guard let stage = shown.stage else { return nil }
        if stage.isScreen { return state.screenShareTrack }
        guard stage.camOn else { return nil }
        if stage.isLocal { return cameraInterrupted ? nil : state.localVideoTrack }
        if let main = state.remoteVideoTracks[stage.id] {
            return state.remoteLightVideoTracks[stage.id]
                ?? state.remoteMiddleVideoTracks[stage.id] ?? main
        }
        if stage.id == state.soleRemoteParticipantId { return state.remoteVideoTrack }
        return nil
    }
}

extension CallPictureInPicture: AVPictureInPictureControllerDelegate {
    nonisolated func pictureInPictureController(
        _ pictureInPictureController: AVPictureInPictureController,
        restoreUserInterfaceForPictureInPictureStopWithCompletionHandler completionHandler: @escaping (Bool) -> Void
    ) {
        // Coming back into the app returns the call exactly as it was left,
        // full screen or collapsed: nothing to restore beyond the app itself.
        completionHandler(true)
    }

    nonisolated func pictureInPictureController(
        _ pictureInPictureController: AVPictureInPictureController,
        failedToStartPictureInPictureWithError error: Error
    ) {
        NSLog("call pip: failed to start: %@", String(describing: error))
    }

    nonisolated func pictureInPictureControllerDidStartPictureInPicture(
        _ pictureInPictureController: AVPictureInPictureController
    ) {
        NSLog("call pip: started")
    }

    nonisolated func pictureInPictureControllerDidStopPictureInPicture(
        _ pictureInPictureController: AVPictureInPictureController
    ) {
        // Closing the system window never ends the call: the call goes on with
        // sound, and the system's call indicator brings it back.
        NSLog("call pip: stopped")
    }
}

// MARK: - The source view

/// A clear view behind the app's content or the call screen: what the system
/// animates the picture-in-picture window out of. Reports when it joins or
/// leaves a window, since the controller can only use a view that is in one.
final class PipSourceView: UIView {
    var onWindowChange: ((PipSourceView) -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        isUserInteractionEnabled = false
        isAccessibilityElement = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        return nil
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        onWindowChange?(self)
    }
}

/// A ``PipSourceView`` for ``CallPictureInPicture`` in a SwiftUI layout.
struct PipSourceViewHost: UIViewRepresentable {
    let pip: CallPictureInPicture

    func makeUIView(context: Context) -> PipSourceView {
        let view = PipSourceView(frame: .zero)
        view.onWindowChange = { [weak pip] moved in
            MainActor.assumeIsolated { pip?.sourceViewMoved(moved) }
        }
        return view
    }

    func updateUIView(_ view: PipSourceView, context: Context) {}
}

private struct WeakSourceView {
    weak var view: PipSourceView?
}

// MARK: - The window's content

/// What the picture-in-picture window draws, published for its SwiftUI view.
@MainActor
final class PipCallModel: ObservableObject {
    @Published private(set) var shown: CallWindowShown?
    @Published private(set) var stageHasVideo = false
    @Published private(set) var ownHasVideo = false
    @Published private(set) var screen = false
    @Published var label = ""
    @Published var peerKey: Data?
    var stageView: UIView?
    var ownView: UIView?

    func update(shown: CallWindowShown?, stageHasVideo: Bool, ownHasVideo: Bool, screen: Bool) {
        if self.shown != shown { self.shown = shown }
        if self.stageHasVideo != stageHasVideo { self.stageHasVideo = stageHasVideo }
        if self.ownHasVideo != ownHasVideo { self.ownHasVideo = ownHasVideo }
        if self.screen != screen { self.screen = screen }
    }
}

/// The picture-in-picture window: the stage person's picture or avatar, the
/// own inset, the name and `+N` (section 4.5.4).
struct PipCallView: View {
    @ObservedObject var model: PipCallModel

    var body: some View {
        GeometryReader { proxy in
            ZStack {
                Color(red: 0.06, green: 0.07, blue: 0.13)
                if model.stageHasVideo, let view = model.stageView {
                    HostedUIView(view: view)
                } else {
                    InitialsAvatar(
                        name: model.shown?.name ?? model.label,
                        identity: model.shown?.stage?.id ?? model.label,
                        size: min(proxy.size.width, proxy.size.height) * 0.4,
                        placeholderSymbol: "person.fill",
                        photo: model.shown?.stage?.isLocal == true
                            ? .own : model.peerKey.map(AvatarPhoto.person))
                }
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    LinearGradient(
                        colors: [.clear, .black.opacity(0.5)], startPoint: .top, endPoint: .bottom)
                        .frame(height: proxy.size.height * 0.25)
                }
                VStack(alignment: .leading, spacing: 0) {
                    Spacer(minLength: 0)
                    if model.ownHasVideo, let view = model.ownView {
                        let width = proxy.size.width * 0.28
                        HostedUIView(view: view)
                            .frame(width: width, height: width * 4 / 3)
                            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                            .frame(maxWidth: .infinity, alignment: .trailing)
                            .padding(.horizontal, 8)
                            .padding(.bottom, 4)
                    }
                    HStack(spacing: 6) {
                        Text(model.shown?.name ?? model.label)
                            .font(.system(size: max(12, proxy.size.height * 0.05), weight: .semibold))
                            .lineLimit(1)
                        if let others = CallWindowContent.othersLabel(model.shown?.othersBeyond ?? 0) {
                            Text(others)
                                .font(.system(size: max(12, proxy.size.height * 0.05)))
                                .monospacedDigit()
                        }
                        Spacer(minLength: 0)
                    }
                    .foregroundStyle(.white)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 8)
                }
            }
        }
        .ignoresSafeArea()
        .environment(\.colorScheme, .dark)
    }
}

/// A UIKit view placed as-is in a SwiftUI layout. The view is owned
/// elsewhere (the renderer), so SwiftUI only lays it out.
private struct HostedUIView: UIViewRepresentable {
    let view: UIView
    func makeUIView(context: Context) -> UIView { view }
    func updateUIView(_ uiView: UIView, context: Context) {}
}

// MARK: - Frames to a sample-buffer layer

/// An `RTCVideoRenderer` that draws a track's frames through an
/// `AVSampleBufferDisplayLayer`, the kind of layer the system renders inside
/// a picture-in-picture window.
///
/// Frames that arrive as a `CVPixelBuffer` (hardware decoding, the local
/// camera) are wrapped as they are; frames in I420 memory (software decoding,
/// VP8) are copied into a biplanar `CVPixelBuffer` first. The rotation the
/// sender stamped on the frame is applied to the layer.
final class SampleBufferVideoRenderer: NSObject, RTCVideoRenderer {

    let view: SampleBufferDisplayView
    /// Mirror the picture (the own camera, as a mirror shows it).
    @MainActor var mirrored = false {
        didSet { view.mirrored = mirrored }
    }

    @MainActor private weak var track: RTCVideoTrack?
    private let converter = I420ToPixelBuffer()

    @MainActor
    override init() {
        view = SampleBufferDisplayView(frame: .zero)
        super.init()
    }

    /// Draw `track`, or nothing.
    @MainActor
    func attach(_ next: RTCVideoTrack?) {
        guard track !== next else { return }
        track?.remove(self)
        track = next
        view.flush()
        next?.add(self)
    }

    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: RTCVideoFrame?) {
        guard let frame else { return }
        let pixelBuffer: CVPixelBuffer?
        if let cv = frame.buffer as? RTCCVPixelBuffer {
            pixelBuffer = cv.pixelBuffer
        } else {
            pixelBuffer = converter.convert(frame.buffer.toI420())
        }
        guard let pixelBuffer, let sample = Self.sampleBuffer(for: pixelBuffer) else { return }
        let rotation = frame.rotation
        let box = SampleBox(sample: sample)
        DispatchQueue.main.async { [weak view] in
            view?.enqueue(box.sample, rotation: rotation)
        }
    }

    /// One frame, shown as soon as it is enqueued.
    private static func sampleBuffer(for pixelBuffer: CVPixelBuffer) -> CMSampleBuffer? {
        var format: CMVideoFormatDescription?
        guard CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pixelBuffer,
            formatDescriptionOut: &format) == noErr, let format
        else { return nil }
        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: pixelBuffer,
            formatDescription: format, sampleTiming: &timing, sampleBufferOut: &sample) == noErr,
            let sample
        else { return nil }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
            CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}

/// Carries a sample buffer to the main queue. A `CMSampleBuffer` is an
/// immutable Core Foundation object once created, so handing it to one other
/// thread is safe.
private struct SampleBox: @unchecked Sendable {
    let sample: CMSampleBuffer
}

/// A view whose layer is an `AVSampleBufferDisplayLayer`, rotated by the
/// frame's own rotation and filling its bounds.
final class SampleBufferDisplayView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    private var displayLayer: AVSampleBufferDisplayLayer {
        // `layerClass` makes this cast always succeed.
        layer as! AVSampleBufferDisplayLayer // swiftlint:disable:this force_cast
    }

    private var rotation: RTCVideoRotation = ._0
    var mirrored = false {
        didSet { if mirrored != oldValue { applyTransform() } }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        displayLayer.videoGravity = .resizeAspectFill
        backgroundColor = .black
        clipsToBounds = true
        isUserInteractionEnabled = false
        isAccessibilityElement = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        return nil
    }

    func flush() {
        displayLayer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }

    func enqueue(_ sample: CMSampleBuffer, rotation: RTCVideoRotation) {
        if rotation != self.rotation {
            self.rotation = rotation
            applyTransform()
        }
        let renderer = displayLayer.sampleBufferRenderer
        if renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
            renderer.flush()
        }
        renderer.enqueue(sample)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        applyTransform()
    }

    /// Turn the layer by the frame's rotation; a quarter turn swaps the
    /// layer's width and height so it still fills the view.
    private func applyTransform() {
        let quarterTurn = rotation == ._90 || rotation == ._270
        let angle: CGFloat
        switch rotation {
        case ._90: angle = .pi / 2
        case ._180: angle = .pi
        case ._270: angle = -.pi / 2
        default: angle = 0
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        displayLayer.transform = CATransform3DIdentity
        displayLayer.bounds = CGRect(
            origin: .zero,
            size: quarterTurn ? CGSize(width: bounds.height, height: bounds.width) : bounds.size)
        displayLayer.position = CGPoint(x: bounds.midX, y: bounds.midY)
        displayLayer.transform = Self.layerTransform(angle: angle, mirrored: mirrored)
        CATransaction.commit()
    }

    /// The frame turned upright by `angle`, then mirrored left to right on
    /// the screen. The mirror comes after the turn: before it, a front-camera
    /// frame (turned a quarter) was flipped upside down instead (seen in the
    /// picture-in-picture window on an iPhone, 2026-11-10).
    static func layerTransform(angle: CGFloat, mirrored: Bool) -> CATransform3D {
        let turn = CATransform3DMakeRotation(angle, 0, 0, 1)
        guard mirrored else { return turn }
        return CATransform3DConcat(turn, CATransform3DMakeScale(-1, 1, 1))
    }
}

/// Copies an I420 frame into a video-range biplanar (NV12) `CVPixelBuffer`
/// from a pool, which an `AVSampleBufferDisplayLayer` can draw. Called on the
/// one libwebrtc thread a track's renderer receives frames on; the lock keeps
/// it correct if that ever changes.
final class I420ToPixelBuffer: @unchecked Sendable {
    private var pool: CVPixelBufferPool?
    private var poolSize: (Int32, Int32) = (0, 0)
    private let lock = NSLock()

    func convert(_ i420: RTCI420BufferProtocol) -> CVPixelBuffer? {
        lock.lock()
        defer { lock.unlock() }
        let width = i420.width
        let height = i420.height
        guard width > 0, height > 0 else { return nil }
        if pool == nil || poolSize != (width, height) {
            let attributes: [String: Any] = [
                kCVPixelBufferPixelFormatTypeKey as String:
                    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                kCVPixelBufferWidthKey as String: Int(width),
                kCVPixelBufferHeightKey as String: Int(height),
                kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
            ]
            var created: CVPixelBufferPool?
            CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attributes as CFDictionary, &created)
            pool = created
            poolSize = (width, height)
        }
        guard let pool else { return nil }
        var out: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &out) == kCVReturnSuccess,
            let buffer = out
        else { return nil }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let yBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
            let uvBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)
        else { return nil }
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let uvStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        let y = yBase.assumingMemoryBound(to: UInt8.self)
        let uv = uvBase.assumingMemoryBound(to: UInt8.self)
        let w = Int(width)
        let h = Int(height)
        for row in 0..<h {
            (y + row * yStride).update(from: i420.dataY + row * Int(i420.strideY), count: w)
        }
        let cw = Int(i420.chromaWidth)
        let ch = Int(i420.chromaHeight)
        for row in 0..<ch {
            let src = i420.dataU + row * Int(i420.strideU)
            let srcV = i420.dataV + row * Int(i420.strideV)
            let dst = uv + row * uvStride
            for col in 0..<cw {
                dst[2 * col] = src[col]
                dst[2 * col + 1] = srcV[col]
            }
        }
        return buffer
    }
}
