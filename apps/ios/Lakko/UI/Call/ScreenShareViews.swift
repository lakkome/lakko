// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit
import WebRTC

/// What a receiver sees of a shared screen (ADR-0083,
/// `docs/ux/screen-share.md` section 5): the picture whole, uncropped, on
/// black; a screen glyph in place of the microphone; a wait until the first
/// frame; pinch, pan and a double tap back to whole on the large place.

// MARK: - The picture, zoomable

/// The share's picture on the large place: aspect fit on black, pinch to
/// zoom, drag to pan while zoomed, double tap to return to whole. The zoom
/// lives in this view, so it resets whenever the share leaves the large place
/// (the view goes) or another share replaces it (the caller keys it on the
/// share).
struct ZoomableScreenVideo: View {
    @ObservedObject var l10n = Localization.shared
    let track: RTCVideoTrack
    /// The spoken name of the picture (`Anna's screen`).
    let label: String
    /// A single tap that is not part of a double tap.
    var onSingleTap: (() -> Void)?
    /// A drag while the picture is whole (it has nothing to pan).
    var onUnzoomedDrag: ((CGSize) -> Void)?
    /// Zoomed in or back to whole.
    var onZoomChanged: (Bool) -> Void = { _ in }

    @State private var scale: CGFloat = ScreenShareZoom.minScale
    @State private var offset: CGSize = .zero
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var pan: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            let size = geo.size
            let live = ScreenShareZoom.clampScale(scale * pinch)
            let shown = ScreenShareZoom.clampOffset(
                CGSize(width: offset.width + pan.width, height: offset.height + pan.height),
                scale: live, size: size)
            ZStack {
                Color.black
                LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFit)
                    .frame(width: size.width, height: size.height)
                    .scaleEffect(live)
                    .offset(shown)
            }
            .frame(width: size.width, height: size.height)
            .clipped()
            .contentShape(Rectangle())
            .gesture(magnify.simultaneously(with: drag(size: size)))
            .onTapGesture(count: 2) { setScale(ScreenShareZoom.minScale, size: size) }
            .onTapGesture { onSingleTap?() }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(label)
            .accessibilityAddTraits(.isImage)
            .accessibilityAction(named: Text(l10n.t("call.screen_share.zoom_in"))) {
                setScale(scale * ScreenShareZoom.step, size: size)
            }
            .accessibilityAction(named: Text(l10n.t("call.screen_share.zoom_out"))) {
                setScale(scale / ScreenShareZoom.step, size: size)
            }
            .accessibilityAction(named: Text(l10n.t("call.screen_share.zoom_reset"))) {
                setScale(ScreenShareZoom.minScale, size: size)
            }
        }
        .onDisappear { onZoomChanged(false) }
    }

    private var magnify: some Gesture {
        MagnificationGesture()
            .updating($pinch) { value, state, _ in state = value }
            .onEnded { value in
                scale = ScreenShareZoom.clampScale(scale * value)
                if scale <= ScreenShareZoom.minScale { offset = .zero }
                onZoomChanged(scale > ScreenShareZoom.minScale)
            }
    }

    private func drag(size: CGSize) -> some Gesture {
        DragGesture(minimumDistance: 10)
            .updating($pan) { value, state, _ in
                if scale > ScreenShareZoom.minScale { state = value.translation }
            }
            .onEnded { value in
                if scale > ScreenShareZoom.minScale {
                    offset = ScreenShareZoom.clampOffset(
                        CGSize(
                            width: offset.width + value.translation.width,
                            height: offset.height + value.translation.height),
                        scale: scale, size: size)
                } else {
                    onUnzoomedDrag?(value.translation)
                }
            }
    }

    private func setScale(_ next: CGFloat, size: CGSize) {
        scale = ScreenShareZoom.clampScale(next)
        offset = ScreenShareZoom.clampOffset(offset, scale: scale, size: size)
        if scale <= ScreenShareZoom.minScale { offset = .zero }
        onZoomChanged(scale > ScreenShareZoom.minScale)
    }
}

// MARK: - The picture or the wait

/// The share's picture (zoomable on the large place, plain elsewhere) and,
/// until its first frame, the screen glyph, a spinner and
/// `Waiting for Anna's screen`.
struct ScreenSharePicture: View {
    @ObservedObject var l10n = Localization.shared
    @ObservedObject var state: CallUiState
    /// The share is on the large place: zoomable.
    let large: Bool
    var onSingleTap: (() -> Void)?
    var onUnzoomedDrag: ((CGSize) -> Void)?

    var body: some View {
        ZStack {
            Color.black
            if let track = state.screenShareTrack {
                if large {
                    ZoomableScreenVideo(
                        track: track, label: tileLabel,
                        onSingleTap: onSingleTap, onUnzoomedDrag: onUnzoomedDrag,
                        onZoomChanged: { state.setScreenShareZoomed($0) })
                        // A replacing share starts whole.
                        .id(state.screenShare.map { "\($0.tileId)#\($0.seq)" })
                } else {
                    LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFit)
                        .accessibilityHidden(true)
                }
            }
            if !state.screenShareHasPicture {
                VStack(spacing: 10) {
                    Image(systemName: "display")
                        .font(.system(size: 28, weight: .regular))
                    ProgressView()
                        .progressViewStyle(.circular)
                        .tint(Color.white)
                    Text(l10n.t("call.screen_share.waiting", texts: ["name": sharerName]))
                        .font(.caption)
                        .multilineTextAlignment(.center)
                        .lineLimit(2)
                }
                .foregroundColor(.white)
                .padding(12)
                .accessibilityElement(children: .combine)
            }
        }
    }

    private var sharerName: String {
        CallPeerName.tileLabel(displayName: state.screenShareSharerName, isLocal: false)
    }

    private var tileLabel: String {
        l10n.t("call.screen_share.tile", texts: ["name": sharerName])
    }
}

// MARK: - The share tile (group call)

/// A shared screen's tile in a group call (`group-call-grid.md` section 6.3):
/// its own tile beside the sharer's camera tile, the picture uncropped on
/// black, a screen glyph in the name strip instead of a microphone glyph. It
/// asks the SFU for nothing (a screen is never in `video-wants`).
struct ScreenShareTile: View {
    @ObservedObject var l10n = Localization.shared
    @ObservedObject var state: CallUiState
    let tile: CallUiState.Participant
    let size: CGSize
    /// On the stage of Focus: the large place, zoomable.
    let isStage: Bool

    private static let corner: CGFloat = 12

    var body: some View {
        ZStack {
            ScreenSharePicture(state: state, large: isStage)
            VStack(spacing: 0) {
                Spacer(minLength: 0)
                HStack(spacing: 6) {
                    Image(systemName: "display")
                        .font(.caption)
                        .accessibilityHidden(true)
                    Text(l10n.t(
                        "call.screen_share.tile",
                        texts: ["name": CallScreenPresentation.tileName(name, tileWidth: size.width)]))
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .font(.caption)
                .foregroundColor(Color.lakkoText)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(Color.lakkoBackground.opacity(0.45))
                .allowsHitTesting(false)
                .accessibilityHidden(true)
            }
        }
        .frame(width: size.width, height: size.height)
        .clipShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Self.corner, style: .continuous)
                .strokeBorder(Color.lakkoBorder, lineWidth: 1)
                .allowsHitTesting(false))
        .accessibilityElement(children: isStage ? .contain : .ignore)
        .accessibilityLabel(l10n.t("call.screen_share.tile", texts: ["name": name]))
    }

    private var name: String {
        CallPeerName.tileLabel(displayName: tile.displayName, isLocal: false)
    }
}

// MARK: - The chip while a pin holds

/// On the stage while someone is pinned and a share is live: `Anna is sharing
/// their screen` with `View`, which clears the pin and stages the share
/// (`screen-share.md` section 5.2).
struct ScreenSharePinnedChip: View {
    @ObservedObject var l10n = Localization.shared
    @ObservedObject var state: CallUiState

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "display")
                .font(.caption)
                .accessibilityHidden(true)
            Text(l10n.t(
                "call.screen_share.pinned_chip",
                texts: ["name": CallPeerName.tileLabel(
                    displayName: state.screenShareSharerName, isLocal: false)]))
                .font(.caption)
                .lineLimit(2)
            Button {
                state.viewScreenShare()
            } label: {
                Text(l10n.t("call.screen_share.view"))
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(Capsule().fill(Color.lakkoAccent))
                    .foregroundColor(.white)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .foregroundColor(Color.lakkoText)
        .padding(.leading, 12)
        .padding(.trailing, 6)
        .background(Capsule().fill(Color.lakkoBackground.opacity(0.75)))
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Announcements

enum ScreenShareAnnouncer {
    /// Say `line` politely: queued behind current speech, never interrupting
    /// it (`screen-share.md` section 10).
    static func post(_ line: String) {
        guard !line.isEmpty else { return }
        var text = AttributedString(line)
        text.accessibilitySpeechAnnouncementPriority = .default
        AccessibilityNotification.Announcement(text).post()
    }
}

// MARK: - Landscape while a share is on the large place

/// The app is portrait; the call screen allows landscape while a share is on
/// the large place (`screen-share.md` section 5.3), so turning the phone gives
/// the share the whole width. The app delegate asks ``mask``.
@MainActor
enum CallOrientation {
    private(set) static var allowsLandscape = false

    static var mask: UIInterfaceOrientationMask {
        allowsLandscape ? .allButUpsideDown : .portrait
    }

    static func setAllowsLandscape(_ on: Bool) {
        guard on != allowsLandscape else { return }
        allowsLandscape = on
        for scene in UIApplication.shared.connectedScenes {
            guard let scene = scene as? UIWindowScene else { continue }
            for window in scene.windows {
                var controller = window.rootViewController
                while let current = controller {
                    current.setNeedsUpdateOfSupportedInterfaceOrientations()
                    controller = current.presentedViewController
                }
            }
            // Back to portrait the moment the share leaves the large place.
            if !on {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: .portrait)) { _ in }
            }
        }
    }
}
