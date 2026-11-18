// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0052 decision 1 / `docs/ux/message-actions.md` section 3.1 — the two
/// surfaces a reply has: the strip above the composer while one is armed, and
/// the quote block inside the bubble once it is sent or received.
///
/// Both draw the same two strings through ``ReplySnapshot/replyQuoteLines(_:)``
/// so they cannot disagree about what a snapshot says; neither reads the
/// quoted message, which may be edited, deleted or absent on this device.

/// The strip above the composer: a rule in the accent colour, the quoted
/// author's name, the excerpt, and a close control at the trailing end. One
/// line each, semantic colours only, so dark mode follows the tokens.
struct ReplyStrip: View {

    /// ADR-0061 — observed so a language switch re-renders the view.
    @ObservedObject var l10n = Localization.shared
    let reply: ReplyRef
    let onClose: () -> Void

    var body: some View {
        let lines = ReplySnapshot.replyQuoteLines(reply)
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                .fill(Color.lakkoAccent)
                .frame(width: 3)
            VStack(alignment: .leading, spacing: 1) {
                Text(lines.name)
                    .chatFont(.caption1, weight: .semibold)
                    .foregroundStyle(Color.lakkoAccent)
                    .lineLimit(1)
                Text(lines.excerpt)
                    .chatFont(.footnote)
                    .foregroundStyle(Color.lakkoText)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onClose) {
                LkIconView(.xmark, size: 14, relativeTo: nil)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(width: 32, height: 32)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("chat.reply.cancel"))
        }
        .padding(.leading, 10)
        .padding(.trailing, 4)
        .padding(.vertical, 6)
        .fixedSize(horizontal: false, vertical: true)
        .background(
            Color.lakkoSurfaceSunken,
            in: RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel(
            l10n.t(
                "chat.reply.strip_label",
                texts: ["name": lines.name.isEmpty ? l10n.t("chat.reply.fallback_excerpt") : lines.name]))
        .accessibilityIdentifier(ReplyStrip.accessibilityID)
    }

    static let accessibilityID = "reply-strip"
}

/// The quote block inside a bubble, above the text: the rule, the name line,
/// the excerpt line, both single-line and truncated, in the bubble's own
/// colours. A button on every platform ("Reply to <name>"): tapping it asks
/// the screen to jump to the quoted message.
struct ReplyQuoteBlock: View {

    let reply: ReplyRef
    /// Which bubble this sits in, so it takes that bubble's foreground.
    let outgoing: Bool
    let onTap: () -> Void

    var body: some View {
        let lines = ReplySnapshot.replyQuoteLines(reply)
        Button(action: onTap) {
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 1.25, style: .continuous)
                    .fill(rule)
                    .frame(width: 2.5)
                VStack(alignment: .leading, spacing: 1) {
                    // An empty name still takes its line, so a reply with no
                    // snapshot has the same shape as one with.
                    Text(lines.name.isEmpty ? " " : lines.name)
                        .chatFont(.caption1, weight: .semibold)
                        .lineLimit(1)
                    Text(lines.excerpt)
                        .chatFont(.caption1)
                        .lineLimit(1)
                }
                .foregroundStyle(foreground)
                .opacity(0.92)
            }
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                foreground.opacity(0.1),
                in: RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            lines.name.isEmpty
                ? Localization.text("chat.reply.quote")
                : Localization.text("chat.reply.quote_to", texts: ["name": lines.name]))
        .accessibilityIdentifier(ReplyQuoteBlock.accessibilityID)
    }

    /// The bubble's own text colour: accent-foreground on the accent bubble,
    /// primary text on the sunken one.
    private var foreground: Color {
        outgoing ? Color.lakkoAccentForeground : Color.lakkoText
    }

    /// The rule: the accent on an incoming bubble, where it reads against the
    /// sunken surface; the bubble's foreground on the accent bubble, where the
    /// accent itself would vanish.
    private var rule: Color {
        outgoing ? Color.lakkoAccentForeground : Color.lakkoAccent
    }

    static let accessibilityID = "reply-quote"
}
