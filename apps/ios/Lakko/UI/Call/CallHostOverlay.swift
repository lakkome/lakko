// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The 1x1 call's app-wide presentation: the ringing banner above whatever
/// screen is showing (Home, the chat list, Contacts, any chat), the full-screen
/// call once it is placed or accepted, and the call's error alert.
///
/// Mounted once, at the signed-in root, so the call is never owned by a screen
/// the user can leave. Everything it shows comes from ``CallHost``.
struct CallHostOverlay: ViewModifier {
    @ObservedObject var host: CallHost

    func body(content: Content) -> some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) {
                if host.activeCallState == .ringing {
                    IncomingCallBanner(
                        callerName: host.activePeerName,
                        video: host.incomingVideo,
                        onAccept: { host.acceptIncomingCall(withVideo: false) },
                        onAcceptWithVideo: { host.acceptIncomingCall(withVideo: true) },
                        onDecline: { host.declineIncomingCall() }
                    )
                }
            }
            .fullScreenCover(
                isPresented: Binding(
                    get: { Self.presentsCallScreen(host.activeCallState) },
                    set: { _ in }
                )
            ) {
                if let session = host.activeCall {
                    CallScreen(session: session) {
                        host.clearFinishedCall()
                    }
                }
            }
            .alert(
                "Call",
                isPresented: Binding(
                    get: { host.callError != nil },
                    set: { if !$0 { host.dismissCallError() } }
                ),
                presenting: host.callError
            ) { _ in
                Button("OK", role: .cancel) { host.dismissCallError() }
            } message: { message in
                Text(message)
            }
    }

    /// The full call screen owns everything from the prejoin of an outgoing
    /// call, through an accepted incoming one, to the terminal "Call ended"
    /// overlay. A ringing call is the banner; no call is nothing.
    static func presentsCallScreen(_ state: CallSession.WebCallState?) -> Bool {
        switch state {
        case .idle?, .outgoing?, .connecting?, .connected?, .ending?, .done?:
            return true
        case .ringing?, nil:
            return false
        }
    }
}

extension View {
    func callHostOverlay(_ host: CallHost) -> some View {
        modifier(CallHostOverlay(host: host))
    }
}

/// The ringing banner: who is calling, and Accept / Video / Decline.
private struct IncomingCallBanner: View {
    let callerName: String?
    let video: Bool
    let onAccept: () -> Void
    let onAcceptWithVideo: () -> Void
    let onDecline: () -> Void

    private var headline: String {
        let what = video ? "video call" : "call"
        if let callerName { return "\(callerName) — incoming \(what)" }
        return "Incoming \(what)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(headline)
                    .font(.headline)
                    .foregroundStyle(.white)
                    .lineLimit(2)
                Text("End-to-end encrypted")
                    .font(.caption2)
                    .foregroundStyle(.white.opacity(0.85))
            }
            .accessibilityElement(children: .combine)
            HStack(spacing: 8) {
                Button("Accept", action: onAccept)
                    .buttonStyle(.borderedProminent)
                    .tint(.white)
                    .foregroundStyle(.green)
                    .accessibilityLabel("Accept with audio")
                Button(action: onAcceptWithVideo) {
                    Label("Video", systemImage: "video.fill")
                }
                .buttonStyle(.bordered)
                .tint(.white)
                .accessibilityLabel("Accept with video")
                Spacer()
                Button("Decline", role: .destructive, action: onDecline)
                    .buttonStyle(.bordered)
                    .tint(.white)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange)
    }
}
