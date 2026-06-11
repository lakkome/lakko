// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 lost-everything fallback — show the freshly-minted 24-word
/// recovery phrase for the user to save, gated behind an acknowledge
/// toggle, then finish the session.
///
/// Mirror of the web `SaveFreshPhrasePrompt` / Android `SaveFreshPhraseScreen`.
/// The new identity is already published (without a supersession proof);
/// this screen only displays the phrase + lands the session on confirm.
public struct SaveFreshPhraseScreen: View {

    public let phrase: String
    public let onConfirm: () -> Void

    @State private var acknowledged = false

    public init(phrase: String, onConfirm: @escaping () -> Void) {
        self.phrase = phrase
        self.onConfirm = onConfirm
    }

    private var words: [String] {
        phrase.split(whereSeparator: { $0.isWhitespace }).map(String.init)
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Save your new recovery phrase")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                Text(
                    "You created a brand-new secure identity. Write these 24 " +
                    "words down and keep them safe — they are the only way to " +
                    "restore this identity on another device. Your existing " +
                    "contacts will be asked to verify you again."
                )
                .font(.body)
                .foregroundStyle(.secondary)

                LazyVGrid(
                    columns: [GridItem(.flexible()), GridItem(.flexible())],
                    alignment: .leading,
                    spacing: 6
                ) {
                    ForEach(Array(words.enumerated()), id: \.offset) { index, word in
                        Text("\(index + 1). \(word)")
                            .font(.body.monospaced())
                    }
                }

                Toggle(
                    "I have written down my 24-word recovery phrase.",
                    isOn: $acknowledged
                )
                .font(.body)

                Button(action: onConfirm) {
                    Text("Done — take me to Lakko")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(!acknowledged)
            }
            .padding(24)
        }
    }
}
