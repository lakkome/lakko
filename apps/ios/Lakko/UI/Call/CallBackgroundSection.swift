// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import PhotosUI
import SwiftUI
import UIKit
import WebRTC

/// The `Background` section of the call's settings sheet (UX
/// `docs/ux/call-backgrounds.md` section 2.1): a 16:9 preview of the camera as
/// the others receive it (not mirrored), one row of thumbnails, a hint.
///
/// A tap applies at once. With the camera off, the section runs the camera
/// for the preview only (section 2.2): a capture that feeds no sender, so
/// nothing is sent and the camera button stays off; it stops when the sheet
/// closes.
struct CallBackgroundSection: View {
    @ObservedObject var state: CallUiState
    @ObservedObject var background = CameraBackground.shared
    @ObservedObject var l10n = Localization.shared
    /// Turn the call's camera on, as the camera button does.
    let onCameraOn: () -> Void

    @State private var pickedItem: PhotosPickerItem?
    @State private var previewCapture: LocalCameraCapture?

    private var cameraOn: Bool {
        state.participants.first(where: { $0.isLocal })?.camOn ?? false
    }

    private var choice: BackgroundChoice { background.choice }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(l10n.t("call.settings.background"))
                .font(.footnote.weight(.semibold))
                .foregroundColor(Color.lakkoTextMuted)
                .accessibilityAddTraits(.isHeader)
            preview
            strip
            if background.photoUnusable {
                Text(l10n.t("call.background.photo_unusable"))
                    .font(.footnote)
                    .foregroundColor(Color.lakkoDanger)
            }
            Text(l10n.t("call.settings.background_hint"))
                .font(.footnote)
                .foregroundColor(Color.lakkoTextMuted)
        }
        .onAppear {
            background.refresh()
            CameraBackground.applyStoredChoice()
            // The preview shows the processed picture with the camera off
            // too; the stage runs while it is open.
            background.stage?.setPreviewOpen(true)
            startPreviewCaptureIfNeeded()
        }
        .onDisappear {
            background.stage?.setPreviewOpen(false)
            stopPreviewCapture()
        }
        .onChange(of: pickedItem) { _, item in
            guard let item else { return }
            pickedItem = nil
            Task { await loadPicked(item) }
        }
    }

    // MARK: - Preview

    private var preview: some View {
        ZStack {
            Color.black
            BackgroundPreviewView()
                .accessibilityHidden(true)
            if !cameraOn, previewLine == nil {
                VStack {
                    Spacer(minLength: 0)
                    Text(l10n.t("call.background.preview_camera_off"))
                        .font(.caption)
                        .foregroundColor(.white)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .frame(maxWidth: .infinity)
                        .background(Color.black.opacity(0.5))
                }
            }
            if let line = previewLine {
                VStack(spacing: 10) {
                    Text(line)
                        .font(.footnote)
                        .foregroundColor(.white)
                        .multilineTextAlignment(.center)
                    if background.phase == .failed {
                        Button(action: turnOnWithoutBackground) {
                            Text(l10n.t("call.background.failed_action"))
                                .font(.footnote.weight(.semibold))
                                .foregroundColor(Color.lakkoAccentForeground)
                                .padding(.horizontal, 14)
                                .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
                                .background(Color.lakkoAccent)
                                .clipShape(Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.black.opacity(background.phase == .failed ? 0.7 : 0.45))
            }
        }
        .aspectRatio(16.0 / 9.0, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(l10n.t("call.background.preview_label"))
    }

    /// The line that covers the preview, if any: the failure, or the
    /// background starting (no picture exists then). "Your camera is off" is
    /// a band under a working preview instead.
    private var previewLine: String? {
        if background.phase == .failed { return l10n.t("call.background.failed") }
        if background.phase == .starting { return l10n.t("call.background.preparing") }
        return nil
    }

    // MARK: - Strip

    private var strip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            LazyHStack(alignment: .top, spacing: 10) {
                tile(
                    label: l10n.t("call.background.none"), caption: true,
                    selected: choice.kind == .none, select: { background.select("none") }
                ) { NoneSwatch() }
                tile(
                    label: l10n.t("call.background.blur"), caption: true,
                    selected: choice.kind == .blur, select: { background.select("blur") }
                ) { BlurSwatch(radius: 3) }
                tile(
                    label: l10n.t("call.background.strong_blur"), caption: true,
                    selected: choice.kind == .strongBlur,
                    select: { background.select("strong-blur") }
                ) { BlurSwatch(radius: 8) }
                ForEach(backgroundBuiltinIds(), id: \.self) { id in
                    tile(
                        label: l10n.t(
                            "call.background.builtin_label",
                            texts: ["name": l10n.t("call.background.name.\(id)")]),
                        caption: false,
                        selected: choice.kind == .builtin && choice.id == id,
                        select: { background.select(BackgroundResolution.stored(builtinId: id)) }
                    ) { BuiltinThumbnail(id: id) }
                }
                ForEach(background.photos) { photo in
                    tile(
                        label: l10n.t("call.background.your_photo"), caption: false,
                        selected: choice.kind == .photo && choice.id == photo.id,
                        select: {
                            background.select(BackgroundResolution.stored(photoId: photo.id))
                        }
                    ) { PhotoThumbnail(id: photo.id) }
                    .lakkoLongPressMenu {
                        [.action(l10n.t("call.background.remove"), systemImage: "trash",
                                 destructive: true) { background.removePhoto(id: photo.id) }]
                    }
                    .accessibilityAction(named: l10n.t("call.background.remove")) {
                        background.removePhoto(id: photo.id)
                    }
                }
                PhotosPicker(selection: $pickedItem, matching: .images,
                             preferredItemEncoding: .current) {
                    VStack(spacing: 4) {
                        ZStack {
                            RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous)
                                .fill(Color.lakkoSurfaceSunken)
                            Image(systemName: "plus")
                                .font(.title3.weight(.semibold))
                                .foregroundColor(Color.lakkoAccent)
                        }
                        .frame(width: Self.side, height: Self.side)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t("call.background.add_photo"))
            }
            .padding(.vertical, 3)
            .padding(.horizontal, 3)
        }
    }

    static let side: CGFloat = 64

    private func tile<Content: View>(
        label: String, caption: Bool, selected: Bool, select: @escaping () -> Void,
        @ViewBuilder content: () -> Content
    ) -> some View {
        Button(action: select) {
            VStack(spacing: 4) {
                content()
                    .frame(width: Self.side, height: Self.side)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous)
                            .strokeBorder(selected ? Color.lakkoAccent : Color.lakkoBorder,
                                          lineWidth: selected ? 3 : 1))
                if caption {
                    Text(label)
                        .font(.caption2)
                        .foregroundColor(Color.lakkoText)
                        .lineLimit(1)
                        .frame(width: Self.side + 8)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : [.isButton])
    }

    // MARK: - Actions

    private func turnOnWithoutBackground() {
        background.select("none")
        Task { @MainActor in
            await stopPreviewCaptureNow()
            onCameraOn()
        }
    }

    private func loadPicked(_ item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self) else {
            background.photoUnusable = true
            return
        }
        background.addPhoto(data: data)
    }

    // MARK: - Preview-only camera

    /// With the call's camera off and no capture running, run one for the
    /// preview: a capture whose only source is attached to no track, so no
    /// frame reaches a sender.
    private func startPreviewCaptureIfNeeded() {
        guard background.isAvailable, !cameraOn, LocalCameraCapture.active == nil,
              previewCapture == nil, LocalCameraCapture.hasUsableCamera()
        else { return }
        Task { @MainActor in
            guard await CameraPermission.ensureGranted() else { return }
            guard previewCapture == nil, LocalCameraCapture.active == nil else { return }
            guard let capture = LocalCameraCapture(source: WebRTCPeerConnection.newVideoSource())
            else { return }
            previewCapture = capture
            do {
                try await capture.start()
            } catch {
                NSLog("CallBackgroundSection: preview camera did not start")
                if previewCapture === capture { previewCapture = nil }
            }
        }
    }

    private func stopPreviewCapture() {
        guard let capture = previewCapture else { return }
        previewCapture = nil
        Task { await capture.stop() }
    }

    private func stopPreviewCaptureNow() async {
        guard let capture = previewCapture else { return }
        previewCapture = nil
        await capture.stop()
    }
}

// MARK: - The preview surface

/// The stage's output drawn as it is sent: not mirrored, fitted into the box.
private struct BackgroundPreviewView: UIViewRepresentable {
    func makeUIView(context: Context) -> RTCMTLVideoView {
        let view = RTCMTLVideoView(frame: .zero)
        view.isUserInteractionEnabled = false
        view.videoContentMode = .scaleAspectFit
        CameraBackgroundPreview.add(view)
        return view
    }

    func updateUIView(_ view: RTCMTLVideoView, context: Context) {}

    static func dismantleUIView(_ view: RTCMTLVideoView, coordinator: ()) {
        CameraBackgroundPreview.remove(view)
    }
}

// MARK: - Thumbnails

/// `None`: a crossed circle.
private struct NoneSwatch: View {
    var body: some View {
        ZStack {
            Color.lakkoSurfaceSunken
            Image(systemName: "circle.slash")
                .font(.system(size: 26, weight: .regular))
                .foregroundColor(Color.lakkoTextMuted)
        }
    }
}

/// `Blur` and `Strong blur`: a few soft shapes, blurred by `radius`.
private struct BlurSwatch: View {
    let radius: CGFloat

    var body: some View {
        ZStack {
            Color.lakkoAccentSoft
            Circle().fill(Color.lakkoAccent).frame(width: 26).offset(x: -12, y: -10)
            Circle().fill(Color.lakkoSuccess.opacity(0.8)).frame(width: 22).offset(x: 14, y: 6)
            Rectangle().fill(Color.lakkoWarning.opacity(0.7)).frame(width: 34, height: 10)
                .offset(x: 0, y: 20)
        }
        .blur(radius: radius)
        .clipped()
    }
}

/// A built-in background, drawn by the Rust renderer (the same picture on
/// every client).
private struct BuiltinThumbnail: View {
    let id: String
    @State private var image: CGImage?

    var body: some View {
        ZStack {
            Color.lakkoSurfaceSunken
            if let image {
                Image(decorative: image, scale: 1).resizable().scaledToFill()
            }
        }
        .task(id: id) {
            if image == nil { image = BuiltinThumbnailCache.image(id: id) }
        }
    }
}

/// Built-in thumbnails, drawn once per app run.
@MainActor
private enum BuiltinThumbnailCache {
    static let side = 128
    static var images: [String: CGImage] = [:]

    static func image(id: String) -> CGImage? {
        if let cached = images[id] { return cached }
        guard let rgba = backgroundBuiltinRender(
            id: id, width: UInt32(side), height: UInt32(side)),
              rgba.count == side * side * 4,
              let provider = CGDataProvider(data: rgba as CFData),
              let made = CGImage(
                width: side, height: side, bitsPerComponent: 8, bitsPerPixel: 32,
                bytesPerRow: side * 4,
                space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
        else { return nil }
        images[id] = made
        return made
    }
}

/// One of the person's own photos.
private struct PhotoThumbnail: View {
    let id: String
    @State private var image: CGImage?

    var body: some View {
        ZStack {
            Color.lakkoSurfaceSunken
            if let image {
                Image(decorative: image, scale: 1).resizable().scaledToFill()
            }
        }
        .task(id: id) {
            image = CameraBackground.shared.photoThumbnail(id: id)
        }
    }
}

// MARK: - The self view while the background starts

/// Over this device's own picture while a background starts (UX section 3):
/// the person's avatar with a small spinner, since no frame is sent yet.
struct BackgroundStartingCover: View {
    let name: String
    let identity: String
    let avatarSize: CGFloat
    @ObservedObject var background = CameraBackground.shared
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        if background.phase == .starting {
            ZStack {
                Color.lakkoSurfaceSunken
                InitialsAvatar(
                    name: name, identity: identity, size: avatarSize,
                    placeholderSymbol: "person.fill", photo: .own)
                VStack {
                    Spacer(minLength: 0)
                    ProgressView()
                        .progressViewStyle(.circular)
                        .tint(Color.lakkoText)
                        .padding(.bottom, 8)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(l10n.t("call.background.preparing"))
        }
    }
}
