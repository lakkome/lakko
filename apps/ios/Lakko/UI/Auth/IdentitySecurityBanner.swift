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
            Image(systemName: "exclamationmark.shield")
                .font(.body)
                .foregroundStyle(Color.lakkoDanger)
            VStack(alignment: .leading, spacing: 4) {
                Text("Your account's security code changed.")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                Text(
                    "If you started over on another device, dismiss this. " +
                    "If you did not, change your password and passkeys and " +
                    "verify your safety number with your contacts in person."
                )
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
            }
            Spacer(minLength: 8)
            Button("Dismiss", action: onDismiss)
                .font(.footnote.weight(.medium))
                .foregroundStyle(Color.lakkoAccent)
        }
        .padding(12)
        .background(Color.lakkoSurfaceSunken)
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.control)
                .stroke(Color.lakkoDanger.opacity(0.5), lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
        .padding(8)
    }
}
