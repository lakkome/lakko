// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The camera check a video call opens with (`docs/ux/calls.md` section 3.2).
///
/// ## What this replaces
///
/// A dark card in the middle of the screen titled `Join call`, with the
/// subtitle `Check your microphone`, a large empty box saying `Camera off`, two
/// settings-style rows with switches on the right, a `Join` button and a
/// `Cancel` link. Every part of that was wrong: it was shown before a *voice*
/// call, where there is nothing to check; its camera box was a hole; and a
/// switch says "this is a preference you are configuring" when what is wanted is
/// a button you press before a call.
///
/// What it is now: the live self-view filling the screen, the callee's name
/// across the top, and three round buttons above the bottom edge — microphone
/// and camera either side of a large Call button — with a plain close control in
/// the corner. Turning the camera off does not leave a hole: the preview becomes
/// the callee's avatar on the call ground, and the button still reads Call.
struct CameraCheckScreen: View {
    /// Who is being called. Drives the line across the top and the avatar the
    /// camera-off state falls back to.
    let peerName: String?
    /// The identity the avatar tint and the backdrop are derived from.
    let identity: String
    /// Place the call with what the two round buttons were left at.
    let onCall: (_ micOn: Bool, _ cameraOn: Bool) -> Void
    /// Abandon the call before it is placed.
    let onCancel: () -> Void

    @StateObject private var preview = CameraCheckPreview()
    @State private var micOn = true
    @State private var cameraOn = true
    /// A camera start this screen refused because access is denied.
    @State private var permissionMessage: String?

    private var displayName: String {
        CallPeerName.resolve(profileName: peerName, chatTitle: nil) ?? CallPeerName.placeholder
    }

    var body: some View {
        ZStack {
            CallBackdrop(identity: identity)

            // The self-view fills the screen. With the camera off it gives way
            // to the person being called, which is the honest thing to show:
            // they are what this screen is about.
            if cameraOn, let track = preview.track {
                LocalVideoPreviewView(track: track, mirrored: true, contentMode: .scaleAspectFill)
                    .ignoresSafeArea()
                    .accessibilityHidden(true)
            } else {
                InitialsAvatar(
                    name: peerName ?? "", identity: identity, size: 128,
                    placeholderSymbol: "person.fill"
                )
                .accessibilityHidden(true)
            }

            VStack(spacing: 0) {
                header
                Spacer()
                controls
                    .padding(.bottom, 28)
            }
        }
        .preferredColorScheme(.dark)
        .onAppear { startCameraIfAllowed() }
        .onDisappear { preview.stop() }
        .onChange(of: cameraOn) { _, on in
            if on { startCameraIfAllowed() } else { preview.stop() }
        }
        .alert(
            "Camera unavailable",
            isPresented: Binding(
                get: { permissionMessage != nil },
                set: { if !$0 { permissionMessage = nil } }),
            presenting: permissionMessage
        ) { _ in
            Button("OK", role: .cancel) { permissionMessage = nil }
        } message: { message in
            Text(message)
        }
    }

    // MARK: - Pieces

    private var header: some View {
        ZStack {
            VStack(spacing: 2) {
                Text(displayName)
                    .font(.title3)
                    .fontWeight(.semibold)
                    .foregroundStyle(.white)
                    .lineLimit(1)
                Text("Video call")
                    .font(.footnote)
                    .foregroundStyle(Color.white.opacity(0.7))
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Video call to \(displayName)")

            HStack {
                Button(action: onCancel) {
                    Image(systemName: "xmark")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: 40, height: 40)
                        .background(Circle().fill(Color.white.opacity(0.14)))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Cancel the call")
                Spacer()
                if cameraOn && preview.canFlip {
                    Button {
                        Task { await preview.flip() }
                    } label: {
                        Image(systemName: "arrow.triangle.2.circlepath.camera")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 40, height: 40)
                            .background(Circle().fill(Color.white.opacity(0.14)))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Flip camera")
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
    }

    /// Microphone and camera either side of the Call button — the same round
    /// buttons the call itself uses, in the same floating bar, so nothing on
    /// this screen looks like a settings row.
    private var controls: some View {
        CallControlsBar {
            CallControlButton(
                control: .microphone,
                systemImage: micOn ? "mic.fill" : "mic.slash.fill",
                on: micOn,
                action: { micOn.toggle() })
            callButton
            CallControlButton(
                control: .camera,
                systemImage: cameraOn ? "video.fill" : "video.slash.fill",
                on: cameraOn,
                action: { cameraOn.toggle() })
        }
    }

    /// The large green Call button. Green rather than red because red on a call
    /// screen means one thing only, and it is not this (section 4.1.1).
    private var callButton: some View {
        Button {
            onCall(micOn, cameraOn)
        } label: {
            VStack(spacing: 6) {
                ZStack {
                    Circle().fill(Color.lakkoSuccess)
                    Image(systemName: "video.fill")
                        .font(.system(size: CallScreenPresentation.endDiameter * 0.36))
                        .foregroundStyle(.white)
                }
                .frame(
                    width: CallScreenPresentation.endDiameter,
                    height: CallScreenPresentation.endDiameter)
                Text("Call")
                    .font(.caption2)
                    .foregroundStyle(Color.white.opacity(0.75))
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            cameraOn ? "Start video call with \(displayName)"
                     : "Start voice call with \(displayName)")
    }

    // MARK: - Camera

    private func startCameraIfAllowed() {
        guard cameraOn else { return }
        Task { @MainActor in
            guard await CameraPermission.ensureGranted() else {
                permissionMessage = CameraPermission.deniedMessage
                // A refusal turns the button off rather than leaving it claiming
                // a camera that is not running.
                cameraOn = false
                return
            }
            preview.start()
        }
    }
}
