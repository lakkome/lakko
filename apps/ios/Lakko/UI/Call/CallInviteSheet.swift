// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UIKit
import SwiftUI

/// `Invite` on the call screen (`docs/ux/call-links.md` section 3) — the one
/// control the redesign adds, and the panel it opens.
///
/// ## Why the link is offered in a panel rather than straight to the share sheet
///
/// The share sheet needs the thing it is sharing before it can open, and the
/// first `Invite` in a call has nothing yet: making the link is a round trip
/// plus an anti-spam solve, a few seconds on a slow device. Something has to be
/// on screen for those seconds, and the spec is explicit that it says what it
/// is doing rather than only spinning. So the panel opens at once, shows the
/// work, and then holds the link with Share — which IS the iOS share sheet —
/// and Copy beside it, exactly the two the spec's state table names.
///
/// ## Why it is in the call's overflow menu and not the control row
///
/// The row already shrinks to its floor at five controls; a sixth measurably
/// runs off a 375 pt screen, and the last thing to clip would be End. `Invite`
/// is pressed at most once in a call and often never, which is what the one
/// menu a call is allowed exists for — unlike mute and camera, the two the
/// redesign took OUT of that menu because people reach for them mid-sentence.

// MARK: - The control

/// The `Invite` entry in a group call's overflow menu.
struct CallInviteMenuButton: View {
    @ObservedObject var offer: CallInviteOffer

    var body: some View {
        Button {
            Task { await offer.press() }
        } label: {
            Label(CallInviteOfferRules.controlLabel, systemImage: "person.badge.plus")
        }
    }
}

// MARK: - The panel

/// What `Invite` opens: the work, then the link.
struct CallInviteSheet: View {
    @ObservedObject var offer: CallInviteOffer
    let onDone: () -> Void

    /// Confirms the copy for a moment, then goes back. Local to the sheet: it
    /// is about this press, not about the offer.
    @State private var copied = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 20) {
                switch offer.state {
                case .idle, .minting:
                    minting
                case .ready(let url):
                    ready(url: url)
                case .failed(let message):
                    failed(message: message)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.lakkoBackground)
            .navigationTitle("Invite")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done", action: onDone)
                }
            }
        }
        // Sized to what is in it, not to half the screen. `.medium` left a
        // pane of empty background under two small buttons, which reads as an
        // unfinished screen; there are three lines and two actions here, and a
        // sheet should be as tall as the thing it carries. `.large` stays
        // available for a body text size that needs it.
        .presentationDetents([.height(sheetHeight), .large])
        .presentationDragIndicator(.visible)
    }

    /// Enough for the sentence, the link and the two actions in each state.
    private var sheetHeight: CGFloat {
        switch offer.state {
        case .idle, .minting: return 220
        case .ready: return 380
        case .failed: return 260
        }
    }

    private var minting: some View {
        HStack(alignment: .top, spacing: 12) {
            ProgressView()
            Text(CallInviteOfferRules.mintingMessage)
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
        }
    }

    /// The link, then the two things a person can do with it.
    ///
    /// Three deliberate departures from the generic link row this used to
    /// reuse, each from watching the panel in a real call:
    ///
    ///   * **the link is shown.** The generic row offers a title and two
    ///     buttons, which asks a person to send something they have never
    ///     seen. It is display-only, scheme removed, and elided in the middle
    ///     by the LAYOUT rather than by the string - doing both elided it
    ///     twice; the full URL is what Share and Copy carry.
    ///   * **Share is the primary action, full width.** Sharing is what the
    ///     person came to do - the reason the link exists is to reach someone
    ///     who is not here - and it was drawn as a small tinted chip beside an
    ///     equally small Copy.
    ///   * **Copy is second and also full width.** It is a real alternative,
    ///     not a footnote, and two buttons of one width read as two choices
    ///     rather than as one control that happens to have grown a twin.
    private func ready(url: String) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(CallInviteOfferRules.readyMessage)
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
                // Two sentences, and the second one carries the reason no
                // setting was asked for. Letting it be clipped to "...this
                // call...." drops exactly the half that explains the design.
                .fixedSize(horizontal: false, vertical: true)

            Text(CallInviteOfferRules.shortLink(url))
                .font(.callout.monospaced())
                .foregroundStyle(Color.lakkoText)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
                .accessibilityLabel("Call link")
                .accessibilityValue(url)

            ShareLink(item: url) {
                Label("Share link", systemImage: "square.and.arrow.up")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            Button {
                UIPasteboard.general.string = url
                copied = true
            } label: {
                Label(copied ? "Copied" : "Copy link", systemImage: copied ? "checkmark" : "doc.on.doc")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .animation(.easeInOut(duration: 0.15), value: copied)
        }
    }

    private func failed(message: String) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(message)
                .font(.body)
                .foregroundStyle(Color.lakkoDanger)
            Button("Try again") {
                Task { await offer.retry() }
            }
            .buttonStyle(.borderedProminent)
        }
    }
}

// MARK: - Attaching it

/// Carries the panel for a call that has an offer, and nothing at all for one
/// that does not — a joiner, or a group call in a chat, neither of which can
/// sign a capability that would lead anywhere.
///
/// It is a modifier rather than a `.sheet` inside the call screen because the
/// offer is optional, and a `@ObservedObject` cannot be: the inner modifier is
/// what observes it, and the outer one is what decides whether there is
/// anything to observe.
struct CallInvitePanel: ViewModifier {
    let offer: CallInviteOffer?

    @ViewBuilder
    func body(content: Content) -> some View {
        if let offer {
            content.modifier(Attached(offer: offer))
        } else {
            content
        }
    }

    private struct Attached: ViewModifier {
        @ObservedObject var offer: CallInviteOffer

        func body(content: Content) -> some View {
            content.sheet(
                isPresented: Binding(
                    get: { offer.panelOpen },
                    set: { offer.setPanelOpen($0) })
            ) {
                CallInviteSheet(offer: offer, onDone: { offer.setPanelOpen(false) })
            }
        }
    }
}
