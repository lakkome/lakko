// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "This device was removed" (docs/ux/second-device.md section 3.1) and its
/// second variant, "Your account keys were reset" (docs/ux/identity-recovery.md
/// section 15.2, ADR-0065 section 3.5).
///
/// Replaces the whole signed-in app once another device of the account has
/// removed this one, or reset the account's keys: no chat list behind it,
/// no dismiss, no retry. In BOTH variants the deletion starts by itself
/// (ADR-0066 section 2.3, UX 18.2: a removed device no longer waits for a
/// tap); the one button, "Sign in", appears once it is done. The mismatch variant (the directory publishes
/// other keys at the same generation, an anomaly) keeps the "Sign out"
/// button and runs the deletion on the tap. Drawn in the anonymous screens'
/// frame (``AuthScreen``), since where it leads is sign-in.
struct RemovedDeviceScreen: View {

    /// Which of the three states the screen shows.
    enum Variant: Equatable {
        /// Another device removed this one; the wipe is running or done.
        case removed(KeysSignOutState)
        /// The keys were reset elsewhere; the wipe is running or done.
        case keysReset(KeysSignOutState)
    }

    let variant: Variant
    /// `@name` for the reset copy; empty for a guest.
    var username: String = ""
    /// A catalog key drawn under the body of the reset variant, or `nil`: a
    /// recall that ended because the tries ran out or the time passed while
    /// the form was open (UX 18.4, `devices.recall.error.*`).
    var noticeKey: String? = nil
    let onSignOut: () -> Void
    var onSignIn: () -> Void = {}

    /// Set on the tap and never cleared: sign-out replaces this screen, and a
    /// second tap must not start a second wipe.
    @State private var busy = false
    /// Re-renders the copy on a language switch.
    @ObservedObject private var l10n = Localization.shared

    init(
        _ state: KeysSignOutState,
        cause: DeviceSignOutCause,
        username: String,
        noticeKey: String? = nil,
        onSignOut: @escaping () -> Void,
        onSignIn: @escaping () -> Void
    ) {
        self.variant = RemovedDevice.variant(cause: cause, state: state)
        self.username = username
        self.noticeKey = noticeKey
        self.onSignOut = onSignOut
        self.onSignIn = onSignIn
    }

    var body: some View {
        AuthScreen {
            switch variant {
            case .removed(let state):
                removed(state)
            case .keysReset(let state):
                keysReset(state)
            }
        }
    }

    private var displayedAccount: String {
        username.isEmpty ? "" : "@\(username)"
    }

    /// UX 18.2: the deletion runs by itself, then one button. No "Was it
    /// you?" and no way back: a removal changes no keys.
    @ViewBuilder
    private func removed(_ state: KeysSignOutState) -> some View {
        switch state {
        case .removing, .mismatch:
            // `.mismatch` is a state of the reset variant only; a removal
            // never waits for a tap.
            AuthTitle(
                title: RemovedDevice.Copy.title,
                subtitle: RemovedDevice.Copy.bodyRemoving(account: displayedAccount)
            )
            .accessibilityElement(children: .combine)
            HStack(spacing: 10) {
                ProgressView()
                Text(RemovedDevice.Copy.removing)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .accessibilityElement(children: .combine)
        case .removed:
            AuthTitle(
                title: RemovedDevice.Copy.title,
                subtitle: RemovedDevice.Copy.bodyDone(account: displayedAccount)
            )
            .accessibilityElement(children: .combine)
            AuthPrimaryButton(title: RemovedDevice.Copy.signIn, action: onSignIn)
        }
    }

    /// Why a recall ended, under the body (UX 18.4): the tries ran out, or
    /// the time passed while the form was open.
    @ViewBuilder private var notice: some View {
        if let noticeKey {
            AuthHint(text: l10n.t(noticeKey), isError: true)
        }
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
            notice
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
            notice
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
