// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Placeholder home screen — Phase 1.14d.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Home.tsx` (1.17d9)
///   * `apps/android/.../ui/home/HomeScreen.kt` (1.15d4)
///
/// Once 1.14e (contacts) and 1.14f (chat) land, this becomes the
/// Lakko app surface proper. For 1.14d the only "you're logged in"
/// surface we need is a confirmation that the OPAQUE round-trip +
/// device-key persistence worked end-to-end, plus buttons to wipe
/// the device and start over (parallel to the web's `forgetDevice`
/// action) or to log out (keep on-device material, drop in-memory
/// session).
///
/// The Android side already surfaces Contacts / Chats entry buttons
/// because 1.15e/f shipped first; the iOS port follows the same
/// sub-sprint order and adds those buttons in 1.14e/f respectively.
public struct HomeScreen: View {

    public let account: SessionAccount
    public var onOpenContacts: () -> Void
    public var onOpenChats: () -> Void
    public var onOpenChannels: () -> Void
    public var onOpenPasskeys: () -> Void
    public var onOpenDevices: () -> Void
    public var onOpenAntiCensorship: () -> Void
    public var onLogout: () -> Void
    public var onForgetDevice: () -> Void

    public init(
        account: SessionAccount,
        onOpenContacts: @escaping () -> Void = {},
        onOpenChats: @escaping () -> Void = {},
        onOpenChannels: @escaping () -> Void = {},
        onOpenPasskeys: @escaping () -> Void = {},
        onOpenDevices: @escaping () -> Void = {},
        onOpenAntiCensorship: @escaping () -> Void = {},
        onLogout: @escaping () -> Void,
        onForgetDevice: @escaping () -> Void
    ) {
        self.account = account
        self.onOpenContacts = onOpenContacts
        self.onOpenChats = onOpenChats
        self.onOpenChannels = onOpenChannels
        self.onOpenPasskeys = onOpenPasskeys
        self.onOpenDevices = onOpenDevices
        self.onOpenAntiCensorship = onOpenAntiCensorship
        self.onLogout = onLogout
        self.onForgetDevice = onForgetDevice
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Lakko")
                .font(.largeTitle)
                .fontWeight(.bold)

            Text("Signed in as @\(account.username)")
                .font(.title2)

            Text("Account id (base64):")
                .font(.body)
                .foregroundStyle(.secondary)
            Text(account.accountIdB64)
                .font(.system(.footnote, design: .monospaced))
                .textSelection(.enabled)

            Spacer().frame(height: 16)

            Button(action: onOpenContacts) {
                Text("Open contacts")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            Button(action: onOpenChats) {
                Text("Open chats")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            // ADR-0010 — broadcast channels surface. Tapping pushes
            // `ChannelsRootScreen` via the SessionRoot destination
            // toggle, threaded with the session's AccountIdentity seed
            // so the MLS PublisherGroup can be built.
            Button(action: onOpenChannels) {
                Text("Open channels")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            // 2a2d — passkey management lives behind its own
            // surface so the Home screen doesn't bulk out with
            // a long credential list. Tapping pushes
            // `PasskeysView` via the SessionRoot destination
            // toggle.
            Button(action: onOpenPasskeys) {
                Text("Manage passkeys")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            // 2a3b3-ui — DeviceGroup pairing surface. Tapping
            // pushes `DevicesScreen` via the SessionRoot
            // destination toggle.
            Button(action: onOpenDevices) {
                Text("Devices")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            // ADR-0026 — anti-censorship arming surface. Tapping
            // pushes `AntiCensorshipSettingsView` via the SessionRoot
            // destination toggle.
            Button(action: onOpenAntiCensorship) {
                Text("Anti-censorship")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            Text(
                "Accept a contact request from the Contacts screen and the " +
                "1×1 MLS chat lands automatically. Tap into Chats to see " +
                "every active thread."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            Spacer()

            HStack(spacing: 12) {
                // `Log out` keeps the on-device material; the user
                // can log back in quickly with their password.
                Button(action: onLogout) {
                    Text("Log out")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

                // `Forget device` wipes the SessionStore marker + the
                // Keychain-stored device root key — the only path
                // back is the 24-word recovery phrase.
                Button(action: onForgetDevice) {
                    Text("Forget device")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}
