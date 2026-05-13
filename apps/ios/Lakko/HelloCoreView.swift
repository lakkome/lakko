// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// "Hello core" smoke surface for Phase 1.14c.
///
/// Constructs a fresh ``Identity`` via the UniFFI-generated
/// Swift bindings, reads `signaturePublicKey()`, renders the
/// 32-byte Ed25519 verifying key as hex. If hex appears on
/// screen, the JNA-equivalent UniFFI bridge through the
/// `LakkoCore.xcframework` works on this device — every later
/// phase (auth in 1.14d, contacts in 1.14e, chat in 1.14f)
/// sits on a proven foundation.
///
/// Mirror of:
///
///   * Android `MainActivity.HelloCoreScreen` (1.15c)
///   * Web `DevProbe.tsx` (1.16a)
///
/// The Identity is constructed inside a `.task` modifier so
/// the FFI call runs off the main thread on first appear. The
/// resulting hex string is held in `@State` + rendered when
/// non-nil. Production paths in 1.14d will hold the Identity
/// inside a session manager + explicitly `.close()` it on
/// logout — for a single-frame smoke we let the auto-cleaner
/// the UniFFI runtime registers handle the release.
struct HelloCoreView: View {

    @State private var status: String = "Loading core…"

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Lakko")
                .font(.largeTitle)
                .fontWeight(.bold)

            Text("Identity public key")
                .font(.headline)

            Text(status)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)

            Text("Phase 1.14c smoke — UniFFI bridge via XCFramework")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.top, 16)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .task {
            await loadIdentity()
        }
    }

    /// Build the smoke `Identity` + render its signature
    /// public key as a hex string. Errors land in the same
    /// `status` slot so the screen never goes blank.
    private func loadIdentity() async {
        do {
            // The UniFFI-generated `Identity` constructor is
            // synchronous; we `await Task.detached` it onto a
            // CPU dispatcher so the on-first-appear render
            // doesn't stutter while OpenMLS spins up the
            // provider storage. The KeyPackage / group
            // operations later in 1.14d-f get the same
            // treatment.
            let hex = try await Task.detached(priority: .userInitiated) { () -> String in
                let identity = try Identity(name: "lakko-ios-skeleton")
                let pubkey = identity.signaturePublicKey()
                return pubkey.map { String(format: "%02x", $0) }.joined()
            }.value
            await MainActor.run { status = hex }
        } catch {
            await MainActor.run {
                status = "Error: \(error.localizedDescription)"
            }
        }
    }
}

#Preview {
    HelloCoreView()
}
