// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import SwiftUI

// UX section 5 — Lakko's own capture screen: shutter (tap = photo, press and
// hold = video while held), a Photo / Video switch for a screen reader,
// switch camera, flash, close; after capture the preview with Retake and
// Send. Video at most 60 s, H.264 (then exported to MP4 by the media prep).

/// What the camera produced.
enum CapturedMedia {
    case photo(Data)
    case video(URL)
}

@MainActor
final class CameraCapture: NSObject, ObservableObject, AVCapturePhotoCaptureDelegate,
    AVCaptureFileOutputRecordingDelegate {
    enum Access: Equatable { case unknown, granted, denied }

    @Published private(set) var access: Access = .unknown
    @Published private(set) var recording = false
    @Published private(set) var captured: CapturedMedia?
    /// Called with each capture: the screen hands it to the send preview
    /// (UX section 5: "the send preview of section 3 with Retake").
    var onCaptured: ((CapturedMedia) -> Void)?
    @Published var flashOn = false
    @Published private(set) var hasTwoCameras = false

    let session = AVCaptureSession()
    private let photoOutput = AVCapturePhotoOutput()
    private let movieOutput = AVCaptureMovieFileOutput()
    private var position: AVCaptureDevice.Position = .back
    private var input: AVCaptureDeviceInput?
    private var stopTimer: Task<Void, Never>?
    /// UX section 5 — at most 60 s (chosen).
    static let maxVideoSeconds: UInt64 = 60

    func start() async {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: access = .granted
        case .notDetermined:
            access = await AVCaptureDevice.requestAccess(for: .video) ? .granted : .denied
        default: access = .denied
        }
        guard access == .granted else { return }
        hasTwoCameras = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .front) != nil
            && AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) != nil
        configure()
        let session = session
        Task.detached { session.startRunning() }
    }

    func stop() {
        let session = session
        Task.detached { session.stopRunning() }
    }

    private func configure() {
        session.beginConfiguration()
        session.sessionPreset = .high
        if let input { session.removeInput(input) }
        if let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: position),
           let newInput = try? AVCaptureDeviceInput(device: device), session.canAddInput(newInput) {
            session.addInput(newInput)
            input = newInput
        }
        if session.outputs.isEmpty {
            if session.canAddOutput(photoOutput) { session.addOutput(photoOutput) }
            if session.canAddOutput(movieOutput) { session.addOutput(movieOutput) }
            if let mic = AVCaptureDevice.default(for: .audio), let micInput = try? AVCaptureDeviceInput(device: mic),
               session.canAddInput(micInput) {
                session.addInput(micInput)
            }
        }
        // The video note rule: H.264, not HEVC.
        if let connection = movieOutput.connection(with: .video),
           movieOutput.availableVideoCodecTypes.contains(.h264) {
            movieOutput.setOutputSettings([AVVideoCodecKey: AVVideoCodecType.h264], for: connection)
        }
        session.commitConfiguration()
    }

    func switchCamera() {
        position = position == .back ? .front : .back
        configure()
    }

    func takePhoto() {
        let settings = AVCapturePhotoSettings(format: [AVVideoCodecKey: AVVideoCodecType.jpeg])
        if photoOutput.supportedFlashModes.contains(.on) { settings.flashMode = flashOn ? .on : .off }
        photoOutput.capturePhoto(with: settings, delegate: self)
    }

    func startVideo() {
        guard !movieOutput.isRecording else { return }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("lakko-camera-\(UUID().uuidString).mov")
        movieOutput.startRecording(to: url, recordingDelegate: self)
        recording = true
        stopTimer = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.maxVideoSeconds * 1_000_000_000)
            if !Task.isCancelled { self?.stopVideo() }
        }
    }

    func stopVideo() {
        stopTimer?.cancel()
        if movieOutput.isRecording { movieOutput.stopRecording() }
    }

    func retake() {
        if case .video(let url) = captured { try? FileManager.default.removeItem(at: url) }
        captured = nil
    }

    nonisolated func photoOutput(_ output: AVCapturePhotoOutput, didFinishProcessingPhoto photo: AVCapturePhoto, error: Error?) {
        let data = photo.fileDataRepresentation()
        Task { @MainActor in
            guard let data else { return }
            self.captured = .photo(data)
            self.onCaptured?(.photo(data))
        }
    }

    nonisolated func fileOutput(
        _ output: AVCaptureFileOutput, didFinishRecordingTo outputFileURL: URL, from connections: [AVCaptureConnection],
        error: Error?
    ) {
        Task { @MainActor in
            self.recording = false
            self.captured = .video(outputFileURL)
            self.onCaptured?(.video(outputFileURL))
        }
    }
}

/// The live preview layer.
struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession

    final class PreviewView: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer } // swiftlint:disable:this force_cast
    }

    func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.preview.session = session
        view.preview.videoGravity = .resizeAspectFill
        return view
    }

    func updateUIView(_ uiView: PreviewView, context: Context) {}
}

/// UX section 5 — the capture screen.
struct CameraCaptureScreen: View {
    let onSend: (CapturedMedia) -> Void
    let onClose: () -> Void

    @StateObject private var camera = CameraCapture()
    @ObservedObject private var l10n = Localization.shared
    @State private var videoMode = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            switch camera.access {
            case .denied:
                VStack(spacing: 16) {
                    Text(l10n.t("camera.denied")).foregroundStyle(.white).multilineTextAlignment(.center)
                    Button(l10n.t("chat.record.open_settings")) {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                }
                .padding(24)
            case .unknown:
                ProgressView()
            case .granted:
                if let captured = camera.captured {
                    preview(captured)
                } else {
                    CameraPreview(session: camera.session).ignoresSafeArea()
                    controls
                }
            }
            VStack {
                HStack {
                    Button(action: onClose) {
                        LkIconView(.xmark, size: 24).foregroundStyle(.white).frame(width: 44, height: 44)
                    }
                    .accessibilityLabel(l10n.t("common.close"))
                    Spacer()
                    if camera.captured == nil, camera.access == .granted {
                        Button { camera.flashOn.toggle() } label: {
                            LkIconView(camera.flashOn ? .bolt : .boltSlash, size: 24)
                                .foregroundStyle(.white).frame(width: 44, height: 44)
                        }
                    }
                }
                .padding(.horizontal, 12)
                Spacer()
            }
        }
        .task {
            camera.onCaptured = { onSend($0) }
            await camera.start()
        }
        .onDisappear { camera.stop() }
    }

    private var controls: some View {
        VStack {
            Spacer()
            Picker("", selection: $videoMode) {
                Text(l10n.t("camera.photo")).tag(false)
                Text(l10n.t("camera.video")).tag(true)
            }
            .pickerStyle(.segmented)
            .frame(width: 200)
            HStack {
                Spacer().frame(width: 60)
                Spacer()
                shutter
                Spacer()
                if camera.hasTwoCameras {
                    Button { camera.switchCamera() } label: {
                        LkIconView(.cameraFlip, size: 28)
                            .foregroundStyle(.white).frame(width: 60, height: 60)
                    }
                    .accessibilityLabel(l10n.t("camera.switch"))
                } else {
                    Spacer().frame(width: 60)
                }
            }
            .padding(.bottom, 24)
        }
    }

    private var shutter: some View {
        Circle()
            .strokeBorder(.white, lineWidth: 4)
            .background(Circle().fill(camera.recording ? Color.lakkoDanger : Color.white.opacity(0.2)))
            .frame(width: 76, height: 76)
            .onTapGesture {
                if videoMode {
                    camera.recording ? camera.stopVideo() : camera.startVideo()
                } else {
                    camera.takePhoto()
                }
            }
            .onLongPressGesture(minimumDuration: 0.3, pressing: { pressing in
                if pressing { camera.startVideo() } else if camera.recording { camera.stopVideo() }
            }, perform: {})
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel(l10n.t(videoMode
                ? (camera.recording ? "camera.shutter_stop" : "camera.shutter_record") : "camera.shutter_photo"))
    }

    @ViewBuilder
    private func preview(_ media: CapturedMedia) -> some View {
        VStack {
            Spacer()
            switch media {
            case .photo(let data):
                if let image = UIImage(data: data) { Image(uiImage: image).resizable().scaledToFit() }
            case .video:
                LkIconView(.video, size: 64, relativeTo: nil).foregroundStyle(.white)
            }
            Spacer()
            HStack {
                Button(l10n.t("camera.retake")) { camera.retake() }.foregroundStyle(.white)
                Spacer()
                Button(l10n.t("attach.preview.send")) { onSend(media) }
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
            }
            .padding(24)
        }
    }
}
