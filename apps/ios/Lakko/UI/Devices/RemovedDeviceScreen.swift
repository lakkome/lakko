// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "This device was removed" (docs/ux/second-device.md section 3.1) and its
/// second variant, "Your account keys were reset" (docs/ux/identity-recovery.md
/// section 15.2, ADR-0065 section 3.5).
///
/// Replaces the whole signed-in app once another device of the account has
/// removed this one, or reset the account's keys: no chat list behind it,
/// no dismiss, no retry. In the removed variant the one button is Settings'
/// "Forget this device" (``AppServices/forgetThisDevice()``): this device's
/// keys and local history for the account go, then the login screen. In the
/// reset variant the deletion starts by itself; the one button, "Sign in",
/// appears once it is done. The mismatch variant (the directory publishes
/// other keys at the same generation, an anomaly) keeps the "Sign out"
/// button and runs the deletion on the tap. Drawn in the anonymous screens'
/// frame (``AuthScreen``), since where it leads is sign-in.
struct RemovedDeviceScreen: View {

    /// Which of the three states the screen shows.
    enum Variant: Equatable {
        /// Another device removed this one.
        case removed
        /// The keys were reset elsewhere; the wipe is running or done.
        case keysReset(KeysSignOutState)
    }

    let variant: Variant
    /// `@name` for the reset copy; empty for a guest.
    var username: String = ""
    let onSignOut: () -> Void
    var onSignIn: () -> Void = {}

    /// Set on the tap and never cleared: sign-out replaces this screen, and a
    /// second tap must not start a second wipe.
    @State private var busy = false
    /// Re-renders the copy on a language switch.
    @ObservedObject private var l10n = Localization.shared

    init(onSignOut: @escaping () -> Void) {
        self.variant = .removed
        self.onSignOut = onSignOut
    }

    init(keysReset state: KeysSignOutState, username: String, onSignOut: @escaping () -> Void, onSignIn: @escaping () -> Void) {
        self.variant = .keysReset(state)
        self.username = username
        self.onSignOut = onSignOut
        self.onSignIn = onSignIn
    }

    var body: some View {
        AuthScreen {
            switch variant {
            case .removed:
                AuthTitle(
                    title: RemovedDevice.Copy.title,
                    subtitle: RemovedDevice.Copy.body
                )
                AuthPrimaryButton(title: RemovedDevice.Copy.signOut, busy: busy) {
                    guard !busy else { return }
                    busy = true
                    onSignOut()
                }
            case .keysReset(let state):
                keysReset(state)
            }
        }
    }

    private var displayedAccount: String {
        username.isEmpty ? "" : "@\(username)"
    }

    @ViewBuilder
    private func keysReset(_ state: KeysSignOutState) -> some View {
        switch state {
        case .removing:
            // UX 15.2: the title and the body are one announcement; the
            // progress state is announced once, not on every tick.
            AuthTitle(
                title: l10n.t("devices.signed_out.title"),
                subtitle: l10n.t("devices.signed_out.body_removing", texts: ["username": displayedAccount])
            )
            .accessibilityElement(children: .combine)
            HStack(spacing: 10) {
                ProgressView()
                Text(l10n.t("devices.signed_out.removing"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .accessibilityElement(children: .combine)
        case .removed:
            AuthTitle(
                title: l10n.t("devices.signed_out.title"),
                subtitle: l10n.t("devices.signed_out.body_removed", texts: ["username": displayedAccount])
            )
            .accessibilityElement(children: .combine)
            AuthPrimaryButton(title: l10n.t("devices.signed_out.sign_in"), action: onSignIn)
        case .mismatch:
            AuthTitle(
                title: l10n.t("devices.signed_out.title"),
                subtitle: l10n.t("devices.signed_out.body_mismatch")
            )
            .accessibilityElement(children: .combine)
            AuthPrimaryButton(title: RemovedDevice.Copy.signOut, busy: busy) {
                guard !busy else { return }
                busy = true
                onSignOut()
            }
        }
    }
}
