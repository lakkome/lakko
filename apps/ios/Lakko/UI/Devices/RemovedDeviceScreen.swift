// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "This device was removed" (docs/ux/second-device.md section 3.1).
///
/// Replaces the whole signed-in app once another device of the account has
/// removed this one: no chat list behind it, no dismiss, no retry. The one
/// button is Settings' "Forget this device"
/// (``AppServices/forgetThisDevice()``): this device's keys and local history
/// for the account go, then the login screen. Drawn in the anonymous screens'
/// frame (``AuthScreen``), since where it leads is sign-in.
struct RemovedDeviceScreen: View {
    let onSignOut: () -> Void

    /// Set on the tap and never cleared: sign-out replaces this screen, and a
    /// second tap must not start a second wipe.
    @State private var busy = false
    /// Re-renders the copy on a language switch.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            AuthTitle(
                title: RemovedDevice.Copy.title,
                subtitle: RemovedDevice.Copy.body
            )
            AuthPrimaryButton(title: RemovedDevice.Copy.signOut, busy: busy) {
                guard !busy else { return }
                busy = true
                onSignOut()
            }
        }
    }
}
