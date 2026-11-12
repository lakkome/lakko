// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// The chat of a group call (`docs/ux/calls.md` section 4.1.4): the panel, the
/// one-line preview above the control bar, and the `Chat` circle with its
/// unread badge. What they show is ``CallChatModel``'s; this file only draws.

// MARK: - The circle

/// `Chat` in the group row: the panel opens and closes from here, and a count
/// of messages from others waits on it while the panel is closed.
struct CallChatControlButton: View {
    @ObservedObject var chat: CallChatModel
    @ObservedObject var l10n = Localization.shared
    let enabled: Bool

    var body: some View {
        CallControlButton(
            control: .chat,
            icon: .bubblesFill,
            on: chat.panelOpen,
            enabled: enabled,
            spokenLabel: CallChatModel.spokenCircleLabel(open: chat.panelOpen, unread: chat.unread),
            badge: chat.panelOpen ? nil : CallChatModel.badgeText(unread: chat.unread),
            action: { chat.togglePanel() })
    }
}

/// The accent unread badge of the design system: white 12/600 on the accent,
/// a pill.
struct CallChatBadge: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Color.white)
            .padding(.horizontal, 5)
            .frame(minWidth: 18, minHeight: 18)
            .background(Capsule().fill(Color.lakkoAccent))
            .accessibilityHidden(true)
    }
}

// MARK: - The preview

/// One line above the control bar for 4 seconds while the panel is closed:
/// the sender's name and the start of the text. A tap opens the panel.
struct CallChatPreviewLine: View {
    @ObservedObject var chat: CallChatModel
    /// Read so the name follows the tile's.
    @ObservedObject var state: CallUiState

    var body: some View {
        ZStack {
            line
        }
        .animation(.easeInOut(duration: 0.2), value: chat.preview)
    }

    @ViewBuilder
    private var line: some View {
        if let preview = chat.preview {
            Button {
                chat.openPanel()
            } label: {
                HStack(spacing: 6) {
                    Text(chat.senderName(preview.peerId))
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                        .layoutPriority(1)
                    Text(preview.text.replacingOccurrences(of: "\n", with: " "))
                        .font(.system(size: 13))
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .foregroundStyle(Color.lakkoText)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .frame(maxWidth: 520)
                .background(.ultraThinMaterial, in: Capsule())
                .overlay(Capsule().strokeBorder(Color.white.opacity(0.10), lineWidth: 1))
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .transition(.opacity)
            .id(preview.id)
        }
    }
}

// MARK: - The panel

/// The panel itself: a panel over the whole call screen under 900 pt, a 360 pt
/// column at the end side from 900 pt on.
struct CallChatPanel: View {
    @ObservedObject var chat: CallChatModel
    /// Read for the names over the bubbles and for the microphone.
    @ObservedObject var state: CallUiState
    @ObservedObject var l10n = Localization.shared
    /// The column of a wide screen: a cross to close, and no mute button
    /// (the control row stays visible beside it).
    let column: Bool
    let micOn: Bool
    let onToggleMic: () -> Void
    /// A Lakko link of this deployment was tapped in a message.
    let onOwnLink: (URL) -> Void

    @State private var draft = ""
    @State private var tooLong = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().background(Color.lakkoBorder)
            messageList
            composer
        }
        .background(Color.lakkoSurface)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t("call.chat.a11y"))
        // Esc on a hardware keyboard closes the panel first.
        .onKeyPress(.escape) {
            chat.closePanel()
            return .handled
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 12) {
            Text(l10n.t("call.control.chat"))
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Color.lakkoText)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 8)
            if !column {
                muteButton
            }
            Button {
                chat.closePanel()
            } label: {
                LkIconView(column ? .xmark : .chevronDown, size: 20, relativeTo: nil)
                    .foregroundStyle(Color.lakkoText)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("call.chat.close_a11y"))
        }
        .padding(.leading, 16)
        .padding(.trailing, 6)
        .padding(.vertical, 4)
    }

    /// The round mute button of the collapsed bar (section 4.5), drawn on and
    /// off as the call screen decides: off is the loud face.
    private var muteButton: some View {
        let face = CallScreenPresentation.face(.microphone, on: micOn)
        return Button(action: onToggleMic) {
            ZStack {
                Circle().fill(face == .solid ? Color.lakkoText : Color.lakkoSurfaceSunken)
                if face == .outline {
                    Circle().strokeBorder(Color.lakkoBorder, lineWidth: 1)
                }
                LkIconView(micOn ? .micFill : .micSlashFill, size: 14, relativeTo: nil)
                    .foregroundStyle(face == .solid ? Color.lakkoSurface : Color.lakkoText)
            }
            .frame(width: 32, height: 32)
            .frame(width: 44, height: 44)
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(CallStatusText.spokenControlLabel(.microphone, on: micOn))
    }

    // MARK: Messages

    private static let bottomId = "call-chat-bottom"

    @ViewBuilder
    private var messageList: some View {
        if chat.messages.isEmpty {
            VStack(spacing: 4) {
                Text(l10n.t("call.chat.notice_people"))
                Text(l10n.t("call.chat.notice_ends"))
            }
            .font(.system(size: 15))
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            GeometryReader { geo in
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 4) {
                            notice
                            ForEach(Array(chat.messages.enumerated()), id: \.element.id) { index, message in
                                bubbleRow(
                                    message,
                                    startsRun: CallChatModel.startsRun(at: index, in: chat.messages),
                                    maxBubbleWidth: max(120, (geo.size.width - 24) * 0.65))
                                    .id(message.id)
                            }
                            Color.clear
                                .frame(height: 1)
                                .id(Self.bottomId)
                                .onAppear { chat.setAtBottom(true) }
                                .onDisappear { chat.setAtBottom(false) }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                    }
                    .defaultScrollAnchor(.bottom)
                    .onChange(of: chat.scrollRequest) { _, _ in
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo(Self.bottomId, anchor: .bottom)
                        }
                    }
                    .overlay(alignment: .bottom) {
                        if chat.newBelow {
                            Button {
                                withAnimation(.easeOut(duration: 0.2)) {
                                    proxy.scrollTo(Self.bottomId, anchor: .bottom)
                                }
                            } label: {
                                HStack(spacing: 4) {
                                    LkIconView(.arrowDown, size: 20)
                                    Text(l10n.t("call.chat.new_messages"))
                                }
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundStyle(Color.white)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(Capsule().fill(Color.lakkoAccent))
                            }
                            .buttonStyle(.plain)
                            .padding(.bottom, 8)
                        }
                    }
                }
            }
        }
    }

    /// The two notice lines, kept small at the top once there are messages.
    private var notice: some View {
        VStack(spacing: 2) {
            Text(l10n.t("call.chat.notice_people"))
            Text(l10n.t("call.chat.notice_ends"))
        }
        .font(.footnote)
        .foregroundStyle(Color.lakkoTextMuted)
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private func bubbleRow(
        _ message: CallChatMessage, startsRun: Bool, maxBubbleWidth: CGFloat
    ) -> some View {
        let name: String? = {
            if case .peer(let peerId) = message.sender { return chat.senderName(peerId) }
            return nil
        }()
        VStack(alignment: message.isOwn ? .trailing : .leading, spacing: 2) {
            if startsRun, let name {
                Text(name)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .padding(.horizontal, 4)
                    .padding(.top, 6)
                    .accessibilityHidden(true)
            }
            HStack {
                if message.isOwn { Spacer(minLength: 0) }
                bubble(message)
                    .opacity(message.delivery == .notSent ? 0.6 : 1)
                    .lakkoLongPressMenu {
                        [.action(l10n.t("common.copy"), icon: .copy) {
                            UIPasteboard.general.string = message.text
                        }]
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(
                        name.map {
                            l10n.t("call.chat.message_a11y", texts: ["name": $0, "text": message.text])
                        } ?? message.text)
                    // At most 65% of the pane (design system, message bubble).
                    .frame(maxWidth: maxBubbleWidth, alignment: message.isOwn ? .trailing : .leading)
                if !message.isOwn { Spacer(minLength: 0) }
            }
            if message.delivery == .notSent {
                HStack(spacing: 8) {
                    Text(l10n.t("call.chat.not_sent"))
                        .font(.system(size: 13))
                        .foregroundStyle(Color.lakkoDanger)
                    Button {
                        chat.sendAgain(message.id)
                    } label: {
                        Text(l10n.t("call.chat.send_again"))
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(Color.lakkoAccent)
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: message.isOwn ? .trailing : .leading)
    }

    private func bubble(_ message: CallChatMessage) -> some View {
        let own = message.isOwn
        return VStack(alignment: .trailing, spacing: 2) {
            MessageText(
                text: message.text,
                linkColor: own ? Color.lakkoAccentForeground : Color.lakkoAccent,
                onOwnLink: onOwnLink)
                .font(.system(size: 15))
                .foregroundStyle(own ? Color.lakkoAccentForeground : Color.lakkoText)
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
            Text(message.arrivedAt, format: .dateTime.hour().minute())
                .font(.system(size: 11))
                .foregroundStyle(own ? Color.lakkoAccentForeground.opacity(0.7) : Color.lakkoTextMuted)
                .environment(\.locale, l10n.locale)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .fill(own ? Color.lakkoAccent : Color.lakkoSurfaceSunken))
    }

    // MARK: Composer

    private var sendable: Bool { CallChatText.isSendable(draft) }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .bottom, spacing: 8) {
                TextField(
                    l10n.t("call.chat.placeholder"),
                    text: Binding(
                        get: { draft },
                        set: { input in
                            let edited = CallChatText.edited(input)
                            draft = edited.text
                            tooLong = edited.tooLong
                        }),
                    axis: .vertical
                )
                .lineLimit(1...5)
                .composerFieldChrome()
                // A hardware keyboard's Return sends; Shift+Return breaks the
                // line. The on-screen keyboard's return keeps breaking it.
                .submitOnHardwareReturn(send)
                Button(action: send) {
                    LkIconView(.arrowUpCircleFill, size: 36, relativeTo: nil)
                        .foregroundStyle(sendable ? Color.lakkoAccent : Color.lakkoTextMuted)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!sendable)
                .accessibilityLabel(l10n.t("call.chat.send_a11y"))
            }
            if tooLong {
                Text(l10n.t("call.chat.too_long"))
                    .font(.system(size: 13))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .padding(.horizontal, 6)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.lakkoSurface)
        .overlay(alignment: .top) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
    }

    private func send() {
        guard sendable else { return }
        if chat.send(draft) {
            draft = ""
            tooLong = false
        }
    }
}
