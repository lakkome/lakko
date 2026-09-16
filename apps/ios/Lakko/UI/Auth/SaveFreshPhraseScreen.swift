// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 lost-everything fallback: show the freshly minted 24-word
/// recovery phrase, confirm three words, then finish the session.
///
/// The new identity is already published; this screen only displays the
/// phrase (the same guided step sign-up uses) and lands the session on
/// `onConfirm`.
public struct SaveFreshPhraseScreen: View {

    public let phrase: String
    public let onConfirm: () -> Void

    public init(phrase: String, onConfirm: @escaping () -> Void) {
        self.phrase = phrase
        self.onConfirm = onConfirm
    }

    public var body: some View {
        RecoveryPhraseGuide(
            phrase: phrase,
            title: "Save your new recovery phrase",
            intro: "You started over with a new identity. These 24 words are the only way to restore it on another device. Your contacts will be asked to verify you again.",
            extra: {
                SocialRecoverySharesPanel(phrase: phrase)
            },
            onDone: onConfirm
        )
    }
}
