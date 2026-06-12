// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
/// dismissible security banner shown when the account's published identity
/// did not match this device's at login (someone replaced it; the
/// ACCESS-hijack signature). Mirror of the web `IdentitySecurityBanner`.
public struct IdentitySecurityBanner: View {

    public let onDismiss: () -> Void

    public init(onDismiss: @escaping () -> Void) {
        self.onDismiss = onDismiss
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Your account's secure identity was changed.")
                    .font(.subheadline)
                    .fontWeight(.semibold)
                Text(
                    "Someone published a different identity key for this " +
                    "account. If this was you (you reset your identity on " +
                    "another device), you can dismiss this. If it was NOT " +
                    "you, your account may be compromised — re-verify your " +
                    "safety number with your contacts out of band, and " +
                    "rotate your password / passkeys."
                )
                .font(.footnote)
            }
            Spacer(minLength: 8)
            Button("Dismiss", action: onDismiss)
                .font(.footnote)
        }
        .padding(12)
        .background(Color.red.opacity(0.15))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.red.opacity(0.5), lineWidth: 1)
        )
        .padding(8)
    }
}
