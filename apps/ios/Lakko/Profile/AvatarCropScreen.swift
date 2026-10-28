// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import SwiftUI
import UIKit

/// What the crop screen works on.
enum AvatarCropInput: Identifiable {
    case photo(CGImage)
    case video(AvatarVideoSource)

    var id: ObjectIdentifier {
        switch self {
        case .photo(let image): return ObjectIdentifier(image)
        case .video(let source): return ObjectIdentifier(source.asset)
        }
    }
}

/// UX `profile-screen.md` section 2.2 — choosing the square: the picture
/// fills the screen, a circle shows what is kept; drag to move, pinch or the
/// slider to zoom (fills the circle .. 4x). A video also chooses where its
/// 10 seconds start on a strip of frames; its frame at that start is shown
/// still.
struct AvatarCropScreen: View {
    let input: AvatarCropInput
    let onUse: (PendingAvatarMedia) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var geometry: AvatarCropGeometry?
    @State private var dragStart: CGSize?
    @State private var pinchStart: CGFloat?
    @State private var shown: CGImage?
    @State private var start: Double = 0
    @State private var strip: [CGImage] = []
    @State private var working = false
    @State private var failed = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button(l10n.t("profile.crop.cancel"), action: onCancel)
                    .foregroundStyle(.white)
                    .disabled(working)
                Spacer()
                Text(l10n.t("profile.crop.title")).font(.headline).foregroundStyle(.white)
                Spacer()
                Button(l10n.t("profile.crop.use"), action: use)
                    .font(.body.weight(.semibold))
                    .foregroundStyle(.white)
                    .disabled(working || geometry == nil)
            }
            .padding(.horizontal, 16)
            .frame(height: 52)

            GeometryReader { proxy in
                let viewport = max(1, min(proxy.size.width, proxy.size.height) - 32)
                ZStack {
                    if let shown, let geometry {
                        Image(decorative: shown, scale: 1)
                            .resizable()
                            .frame(width: geometry.displayedSize.width, height: geometry.displayedSize.height)
                            .offset(geometry.offset)
                    }
                    CircleMask(diameter: viewport)
                        .allowsHitTesting(false)
                    if working { ProgressView().tint(.white) }
                }
                .frame(width: proxy.size.width, height: proxy.size.height)
                .clipped()
                .contentShape(Rectangle())
                .gesture(drag.simultaneously(with: pinch))
                .onAppear { setUp(viewport: viewport) }
                .onChange(of: viewport) { _, value in setUp(viewport: value) }
            }

            VStack(spacing: 12) {
                HStack(spacing: 12) {
                    Image(systemName: "minus.magnifyingglass").foregroundStyle(.white.opacity(0.7))
                    Slider(
                        value: Binding(
                            get: { Double(geometry?.zoom ?? 1) },
                            set: { geometry?.setZoom(CGFloat($0)) }),
                        in: 1...Double(AvatarCropGeometry.maxZoom))
                        .accessibilityLabel(l10n.t("profile.crop.zoom"))
                    Image(systemName: "plus.magnifyingglass").foregroundStyle(.white.opacity(0.7))
                }
                if case .video(let source) = input, source.durationSeconds > AvatarVideoWindow.maxSeconds {
                    startStrip(source)
                }
                if failed {
                    Text(l10n.t("profile.video.unusable"))
                        .font(.footnote).foregroundStyle(Color.lakkoDanger)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(16)
        }
        .background(Color.black.ignoresSafeArea())
        .task { await loadInitialFrame() }
    }

    // MARK: - Gestures

    private var drag: some Gesture {
        DragGesture()
            .onChanged { value in
                guard var g = geometry else { return }
                let base = dragStart ?? g.offset
                if dragStart == nil { dragStart = base }
                g.setOffset(CGSize(width: base.width + value.translation.width,
                                   height: base.height + value.translation.height))
                geometry = g
            }
            .onEnded { _ in dragStart = nil }
    }

    private var pinch: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                guard var g = geometry else { return }
                let base = pinchStart ?? g.zoom
                if pinchStart == nil { pinchStart = base }
                g.setZoom(base * value.magnification)
                geometry = g
            }
            .onEnded { _ in pinchStart = nil }
    }

    // MARK: - Video start (the 10 s window)

    private func startStrip(_ source: AvatarVideoSource) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(l10n.t("profile.crop.start")).font(.footnote).foregroundStyle(.white.opacity(0.7))
            GeometryReader { proxy in
                let width = proxy.size.width
                let fraction = AvatarVideoWindow.maxSeconds / source.durationSeconds
                let windowWidth = max(24, width * fraction)
                let maxStart = max(0, source.durationSeconds - AvatarVideoWindow.maxSeconds)
                let x = maxStart == 0 ? 0 : (start / maxStart) * (width - windowWidth)
                ZStack(alignment: .leading) {
                    HStack(spacing: 0) {
                        ForEach(strip.indices, id: \.self) { index in
                            Image(decorative: strip[index], scale: 1)
                                .resizable().scaledToFill()
                                .frame(width: width / CGFloat(max(1, strip.count)), height: 48)
                                .clipped()
                        }
                    }
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color.white, lineWidth: 3)
                        .frame(width: windowWidth, height: 48)
                        .offset(x: x)
                }
                .frame(height: 48)
                .contentShape(Rectangle())
                .gesture(DragGesture(minimumDistance: 0).onChanged { value in
                    let position = min(max(0, value.location.x - windowWidth / 2), width - windowWidth)
                    let next = (width - windowWidth) <= 0 ? 0 : position / (width - windowWidth) * maxStart
                    start = AvatarVideoWindow.clampStart(next, durationSeconds: source.durationSeconds)
                }.onEnded { _ in
                    Task { shown = await source.frame(at: start) ?? shown }
                })
                .accessibilityElement()
                .accessibilityLabel(l10n.t("profile.crop.start"))
                .accessibilityValue("\(Int(start)) s")
            }
            .frame(height: 48)
        }
    }

    // MARK: - Set-up and Use

    private func setUp(viewport: CGFloat) {
        guard let shown else { return }
        let old = geometry
        var next = AvatarCropGeometry(
            imageWidth: CGFloat(shown.width), imageHeight: CGFloat(shown.height), viewport: viewport)
        if let old { next.setZoom(old.zoom); next.setOffset(old.offset) }
        geometry = next
    }

    private func loadInitialFrame() async {
        switch input {
        case .photo(let image):
            shown = image
        case .video(let source):
            shown = await source.frame(at: 0)
            if shown == nil { failed = true }
            if source.durationSeconds > AvatarVideoWindow.maxSeconds {
                var frames: [CGImage] = []
                let count = 8
                for index in 0..<count {
                    let at = source.durationSeconds * Double(index) / Double(count)
                    if let frame = await source.frame(at: at, maximumSize: CGSize(width: 120, height: 120)) {
                        frames.append(frame)
                    }
                }
                strip = frames
            }
        }
        // The viewport is known once the geometry reader laid out.
        if geometry == nil, let shown {
            let side = min(UIScreen.main.bounds.width, UIScreen.main.bounds.height) - 32
            geometry = AvatarCropGeometry(
                imageWidth: CGFloat(shown.width), imageHeight: CGFloat(shown.height), viewport: side)
        }
    }

    private func use() {
        guard let geometry, let shown else { return }
        working = true
        failed = false
        let crop = geometry.cropRect
        switch input {
        case .photo:
            Task.detached(priority: .userInitiated) {
                let still = AvatarJPEG.still(from: shown, crop: crop)
                await MainActor.run {
                    working = false
                    guard let still else { failed = true; return }
                    onUse(PendingAvatarMedia(stillJPEG: still.data, stillSide: still.side))
                }
            }
        case .video(let source):
            // The crop is in the frame's pixels; the frame generator returns
            // the upright size, which is what the export crops.
            let scaleX = source.size.width / CGFloat(shown.width)
            let videoCrop = CGRect(
                x: crop.minX * scaleX, y: crop.minY * scaleX,
                width: crop.width * scaleX, height: crop.height * scaleX)
            let at = start
            Task {
                do {
                    let media = try await AvatarVideoExport.export(
                        source, crop: videoCrop, start: at, directory: AvatarMediaFiles.directory)
                    working = false
                    onUse(media)
                } catch {
                    working = false
                    failed = true
                }
            }
        }
    }
}

/// The dark surround with a clear circle.
private struct CircleMask: View {
    let diameter: CGFloat

    var body: some View {
        GeometryReader { proxy in
            let rect = CGRect(origin: .zero, size: proxy.size)
            let circle = CGRect(
                x: (proxy.size.width - diameter) / 2, y: (proxy.size.height - diameter) / 2,
                width: diameter, height: diameter)
            Path { path in
                path.addRect(rect)
                path.addEllipse(in: circle)
            }
            .fill(Color.black.opacity(0.55), style: FillStyle(eoFill: true))
            Circle()
                .stroke(Color.white.opacity(0.8), lineWidth: 1)
                .frame(width: diameter, height: diameter)
                .position(x: proxy.size.width / 2, y: proxy.size.height / 2)
        }
    }
}

// MARK: - Camera (section 2.3)

/// Lakko's own camera: the front camera in a circle, switch camera, one
/// shutter with a Photo / Video toggle. Video stops on a second tap or at
/// 10 s, a ring shows the time. Then the crop screen.
struct AvatarCameraScreen: View {
    let onCaptured: (AvatarCropInput) -> Void
    let onClose: () -> Void

    static var hasCamera: Bool { VideoNoteRecorder.hasFrontCamera }

    enum Mode: Hashable { case photo, video }
    enum Phase: Equatable { case starting, ready, recording, denied, failed }

    @ObservedObject private var l10n = Localization.shared
    @State private var recorder = VideoNoteRecorder(includeAudio: false)
    @State private var mode: Mode = .photo
    @State private var phase: Phase = .starting
    @State private var elapsed: Double = 0
    @State private var timer: Task<Void, Never>?

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            switch phase {
            case .denied, .failed:
                VStack(spacing: 16) {
                    Text(l10n.t("profile.camera.denied"))
                        .foregroundStyle(.white)
                        .multilineTextAlignment(.center)
                    AuthSecondaryButton(title: l10n.t("profile.camera.close"), action: close)
                        .frame(maxWidth: 240)
                }
                .padding(24)
            default:
                VStack(spacing: 24) {
                    HStack {
                        Button(action: close) {
                            Image(systemName: "xmark").font(.title3.weight(.semibold))
                        }
                        .foregroundStyle(.white)
                        .accessibilityLabel(l10n.t("profile.camera.close"))
                        Spacer()
                        Button { recorder.flip() } label: {
                            Image(systemName: "arrow.triangle.2.circlepath.camera").font(.title3)
                        }
                        .foregroundStyle(.white)
                        .disabled(phase == .recording)
                        .accessibilityLabel(l10n.t("profile.camera.switch"))
                    }
                    .padding(.horizontal, 20)
                    Spacer()
                    ZStack {
                        CapturePreviewView(session: recorder.session)
                            .frame(width: 300, height: 300)
                            .clipShape(Circle())
                        if phase == .recording {
                            ProgressRing(fraction: elapsed / AvatarVideoWindow.maxSeconds, lineWidth: 5)
                                .frame(width: 316, height: 316)
                        }
                    }
                    Spacer()
                    LakkoSegmentedPicker(selection: $mode, options: [
                        LakkoPickerOption(value: Mode.photo, label: l10n.t("profile.camera.photo")),
                        LakkoPickerOption(value: Mode.video, label: l10n.t("profile.camera.video")),
                    ])
                    .frame(maxWidth: 240)
                    .disabled(phase == .recording)
                    Button(action: shutter) {
                        ZStack {
                            Circle().stroke(Color.white, lineWidth: 4).frame(width: 74, height: 74)
                            if phase == .recording {
                                RoundedRectangle(cornerRadius: 6).fill(Color.lakkoDanger).frame(width: 28, height: 28)
                            } else {
                                Circle().fill(mode == .video ? Color.lakkoDanger : Color.white)
                                    .frame(width: 62, height: 62)
                            }
                        }
                    }
                    .disabled(phase == .starting)
                    .accessibilityLabel(l10n.t("profile.camera.shutter"))
                    .padding(.bottom, 24)
                }
            }
        }
        .task { await startCamera() }
        .onDisappear {
            timer?.cancel()
            recorder.discard()
        }
    }

    private func startCamera() async {
        guard await CameraPermission.ensureGranted() else {
            phase = .denied
            return
        }
        do {
            try await recorder.start(directory: AvatarMediaFiles.directory, recordImmediately: false)
            phase = .ready
        } catch {
            phase = .failed
        }
    }

    private func shutter() {
        switch (mode, phase) {
        case (.photo, .ready):
            Task {
                guard let still = await recorder.captureStill() else { return }
                recorder.discard()
                onCaptured(.photo(still))
            }
        case (.video, .ready):
            recorder.beginTake()
            phase = .recording
            elapsed = 0
            timer = Task { @MainActor in
                while !Task.isCancelled, elapsed < AvatarVideoWindow.maxSeconds {
                    try? await Task.sleep(nanoseconds: 100_000_000)
                    elapsed += 0.1
                }
                if !Task.isCancelled { await finishVideo() }
            }
        case (.video, .recording):
            timer?.cancel()
            Task { await finishVideo() }
        default:
            break
        }
    }

    private func finishVideo() async {
        guard phase == .recording else { return }
        phase = .starting
        guard let take = await recorder.stop(), let source = await AvatarVideoSource.load(take.fileURL) else {
            phase = .failed
            return
        }
        onCaptured(.video(source))
    }

    private func close() {
        timer?.cancel()
        recorder.discard()
        onClose()
    }
}
