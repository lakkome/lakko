// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 lost-everything fallback: show the freshly minted 12-word
/// recovery phrase, confirm three words, then finish the session.
///
/// The new identity is already published; this screen only displays the
/// phrase (the same guided step sign-up uses) and lands the session on
/// `onConfirm`.
public struct SaveFreshPhraseScreen: View {

    public let phrase: String
    /// The account's username; empty for a guest account (ADR-0032), whose
    /// phrase is shown right after it was created without a password.
    public let username: String
    public let onConfirm: () -> Void

    @ObservedObject private var l10n = Localization.shared

    public init(phrase: String, username: String = "", onConfirm: @escaping () -> Void) {
        self.phrase = phrase
        self.username = username
        self.onConfirm = onConfirm
    }

    public var body: some View {
        let guest = username.isEmpty
        RecoveryPhraseGuide(
            phrase: phrase,
            title: l10n.t(guest ? "recovery_phrase.save.title" : "recovery_phrase.fresh.title"),
            intro: guest
                ? l10n.t("recovery_phrase.guest.lede")
                : l10n.t("recovery_phrase.fresh.lede", texts: ["username": "@\(username)"]),
            extra: {
                if !guest {
                    AuthHint(text: l10n.t("recovery_phrase.fresh.safety_note"))
                }
            },
            onDone: onConfirm
        )
    }
}
