// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0038 S4 — the dismissible "update available" notice (iOS).
///
/// Clones ``IdentitySecurityBanner``'s placement + structure (a horizontal row:
/// leading icon + message + trailing controls, `cornerRadius: 8`, `.padding(12)`
/// inner / `.padding(8)` outer) but with an **informational** tone, NOT the
/// security banner's danger palette (UX §3 / §9): a subtle accent tint + thin
/// accent stroke so it reads neutral/helpful and stacks legibly *below* the red
/// security banner (severity ordering — security first).
///
/// The `Update` control is shown only when a store URL is configured; otherwise
/// the banner is purely informational text (no dead / greyed-out button, UX §5).
/// The whole banner is an instant conditional render — no entrance animation
/// (UX §10 reduced-motion).
struct UpdateAvailableBanner: View {
    /// Whether to render the primary Update control (false → text-only notice).
    let showUpdateButton: Bool
    /// Open the store deep-link.
    let onUpdate: () -> Void
    /// Dismiss the banner (session + persisted cadence handled by the checker).
    let onDismiss: () -> Void
    /// Re-renders the banner on a language switch (ADR-0058).
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            // Leading glyph so the meaning survives color-blindness / greyscale
            // (UX §10 — icon + label, never color alone). Decorative; the text
            // carries the accessible content.
            Image(systemName: "arrow.down.circle.fill")
                .font(.subheadline)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)

            Text(l10n.t("update.available.message"))
                .font(.subheadline)
                .fontWeight(.semibold)
                .fixedSize(horizontal: false, vertical: true)

            Spacer(minLength: 8)

            if showUpdateButton {
                Button(l10n.t("update.available.update"), action: onUpdate)
                    .font(.footnote)
                    .buttonStyle(.borderedProminent)
                    .frame(minHeight: 44)
                    .accessibilityHint(l10n.t("update.available.opens_app_store_hint"))
            }
            Button(l10n.t("common.dismiss"), action: onDismiss)
                .font(.footnote)
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
                .accessibilityLabel(l10n.t("common.dismiss"))
        }
        .padding(12)
        .background(Color.lakkoAccentSoft)
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.lakkoAccent.opacity(0.5), lineWidth: 1)
        )
        .padding(8)
        // Expose the row as one informational element so VoiceOver reads the
        // notice then its controls (UX §10). Weaker than the security banner's
        // alert trait — an available update is not an interrupt.
        .accessibilityElement(children: .contain)
    }
}
