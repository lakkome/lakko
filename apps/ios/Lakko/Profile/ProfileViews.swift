// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

// MARK: - Username rule hint

/// The inline reason under a username field (UX spec §4): shown while the
/// user types, only for a name that fails the rule, never for an empty field.
struct UsernameRuleHint: View {
    let input: String

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let message = checkUsername(input).inlineMessage {
            Text(message)
                .font(.footnote)
                .foregroundStyle(Color.lakkoDanger)
                .accessibilityLabel(l10n.t("username.a11y_hint", texts: ["message": message]))
        }
    }
}

// Settings -> Profile is ``ProfileScreen`` (UX `profile-screen.md`).

// MARK: - A resolved name, observed

/// Re-renders `content` with the resolved name whenever a profile or a
/// username verdict changes, so every surface updates in place (UX spec §6).
struct ResolvedName<Content: View>: View {
    @ObservedObject private var directory: ProfileDirectory
    let signatureKey: Data
    let content: (ResolvedDisplayName) -> Content

    @MainActor
    init(
        signatureKey: Data,
        directory: ProfileDirectory? = nil,
        @ViewBuilder content: @escaping (ResolvedDisplayName) -> Content
    ) {
        self._directory = ObservedObject(wrappedValue: directory ?? .shared)
        self.signatureKey = signatureKey
        self.content = content
    }

    var body: some View {
        // Reading `revision` subscribes this view to every change.
        let _ = directory.revision
        content(directory.resolve(signatureKey: signatureKey))
    }
}
