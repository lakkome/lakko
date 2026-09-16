// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Social recovery shares, behind a secondary "More options" link on the
/// recovery-phrase step: split the phrase into five shares of which any
/// three restore it, for people the user trusts. A complement to writing
/// the phrase down, never a replacement, so it stays collapsed by default.
struct SocialRecoverySharesPanel: View {
    let phrase: String

    @State private var expanded = false
    @State private var shares: [String] = []
    @State private var busy = false
    @State private var error: String?
    @State private var copiedIndex: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Button {
                withAnimation { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Text("More options")
                        .font(.footnote.weight(.medium))
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold))
                }
                .foregroundStyle(Color.lakkoAccent)
            }
            .buttonStyle(.plain)

            if expanded {
                content
                    .padding(16)
                    .background(Color.lakkoSurface)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
                    .overlay(
                        RoundedRectangle(cornerRadius: LakkoRadius.card)
                            .stroke(Color.lakkoBorder, lineWidth: 1)
                    )
            }
        }
    }

    @ViewBuilder private var content: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Share the recovery with people you trust")
                .font(.body.weight(.medium))
                .foregroundStyle(Color.lakkoText)
            Text(
                "Split your phrase into five shares and give one to each of five people. Any three of them together can restore it; fewer cannot."
            )
            .font(.subheadline)
            .foregroundStyle(Color.lakkoTextMuted)

            if shares.isEmpty {
                AuthSecondaryButton(
                    title: busy ? "Generating..." : "Generate five shares",
                    disabled: busy,
                    action: generate
                )
            } else {
                ForEach(Array(shares.enumerated()), id: \.offset) { index, share in
                    ShareRow(
                        index: index + 1,
                        share: share,
                        copied: copiedIndex == index,
                        onCopy: { copy(index: index, value: share) }
                    )
                }
                Text("Give one share to each person. To restore, choose \"Social recovery shares\" on the recovery screen and enter any three.")
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }

            if let error {
                AuthHint(text: error, isError: true)
            }
        }
    }

    private func generate() {
        busy = true
        error = nil
        Task { @MainActor in
            defer { busy = false }
            do {
                shares = try ShamirBridge.splitMnemonic(
                    phrase: phrase,
                    threshold: ShamirBridge.defaultThreshold,
                    shares: ShamirBridge.defaultShareCount
                )
            } catch {
                self.error = "Could not create the shares. Try again."
            }
        }
    }

    private func copy(index: Int, value: String) {
        UIPasteboard.general.string = value
        copiedIndex = index
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if copiedIndex == index { copiedIndex = nil }
        }
    }
}

private struct ShareRow: View {
    let index: Int
    let share: String
    let copied: Bool
    let onCopy: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Text("\(index)")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(width: 22, alignment: .trailing)
            Text(share)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(Color.lakkoText)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onCopy) {
                Text(copied ? "Copied" : "Copy")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Color.lakkoAccent)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
    }
}
