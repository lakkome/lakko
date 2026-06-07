// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// `DevicesScreen` — multi-device management surface
/// (Phase 2a3b3-ui).
///
/// Mirror of `apps/web/src/routes/Devices.tsx`. One screen,
/// two flows picked via a `Picker`:
///
///   1. **Pair this device** — joining side. Generates an
///      ephemeral DeviceIdentity-derived KeyPackage +
///      rendezvous pseudonym via
///      ``PairingOrchestrator/prepareJoinFlow``, renders the
///      pairing QR with ``QrImageView``, long-polls the DS
///      mailbox for the Welcome, runs `accept` on receipt.
///   2. **Add a device** — scanner side. Opens the camera via
///      ``QrScannerView``, decodes the first QR, base64url-
///      decodes the text payload, drives
///      ``PairingOrchestrator/runScanFlow``.
///
/// Both flows pull `accountIdBytes` + `accountIdB64` from the
/// injected ``AuthRepository``; the screen renders an
/// "auth required" stub when the session isn't ready.
public struct DevicesScreen: View {

    @ObservedObject public var authRepository: AuthRepository
    public var onBack: () -> Void

    @State private var tab: Tab = .list

    private enum Tab: String, CaseIterable, Identifiable {
        case list
        case pair
        case scan
        var id: String { rawValue }
        var label: String {
            switch self {
            case .list: return "Your devices"
            case .pair: return "Pair this device"
            case .scan: return "Add a device"
            }
        }
    }

    public init(authRepository: AuthRepository, onBack: @escaping () -> Void) {
        self.authRepository = authRepository
        self.onBack = onBack
    }

    public var body: some View {
        let session = currentSession()
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(
                        "Link a second device to this account. The pairing " +
                        "is end-to-end-encrypted: the server sees only the " +
                        "rendezvous pseudonym and opaque MLS bytes."
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)

                    if let session = session {
                        Picker("", selection: $tab) {
                            ForEach(Tab.allCases) { t in
                                Text(t.label).tag(t)
                            }
                        }
                        .pickerStyle(.segmented)

                        switch tab {
                        case .list:
                            DevicesListView(
                                accountIdBytes: session.accountIdBytes,
                                accountIdB64: session.accountIdB64,
                                sessionToken: session.sessionToken
                            )
                        case .pair:
                            PairView(
                                accountIdBytes: session.accountIdBytes,
                                accountIdB64: session.accountIdB64
                            )
                        case .scan:
                            ScanView(
                                accountIdBytes: session.accountIdBytes,
                                accountIdB64: session.accountIdB64
                            )
                        }
                    } else {
                        Text("Sign in first to manage devices.")
                            .foregroundStyle(.red)
                    }
                }
                .padding(20)
            }
            .navigationTitle("Devices")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button("Back", action: onBack)
                }
            }
        }
    }

    private func currentSession()
        -> (accountIdBytes: Data, accountIdB64: String, sessionToken: String)?
    {
        guard
            let secrets = authRepository.identitySecrets,
            case .session(let account) = authRepository.state
        else {
            return nil
        }
        return (secrets.accountId, account.accountIdB64, secrets.sessionToken)
    }
}

// MARK: - Pair (joining device)

private struct PairView: View {

    let accountIdBytes: Data
    let accountIdB64: String

    @State private var state: PairState = .idle
    @State private var session: PairingOrchestrator.JoinSession?
    @State private var pollTask: Task<Void, Never>?

    private enum PairState: Equatable {
        case idle
        case preparing
        case waiting(qrPayload: Data)
        case joining
        case done
        case error(String)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "Show this QR code to your already-linked device. " +
                "It scans the code, runs the MLS handshake, and your " +
                "new device joins the DeviceGroup."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: { start() }) {
                    Text("Show pairing code").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .preparing:
                ProgressView("Preparing pairing material…")

            case .waiting(let payload):
                VStack {
                    QrImageView(payload: payload)
                        .padding(.vertical, 8)
                    Text("Waiting for the other device to scan this code…")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button("Cancel", role: .cancel, action: cancel)
                        .padding(.top, 4)
                }
                .frame(maxWidth: .infinity)

            case .joining:
                ProgressView("Joining the device group…")

            case .done:
                Label(
                    "Linked. This device is now part of your DeviceGroup.",
                    systemImage: "checkmark.circle.fill"
                )
                .foregroundStyle(.green)
                .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button("Try again", action: { state = .idle })
                }
            }
        }
        .onDisappear { cancel() }
    }

    private func start() {
        state = .preparing
        do {
            let s = try PairingOrchestrator.prepareJoinFlow(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64
            )
            session = s
            state = .waiting(qrPayload: s.qrPayload)
            pollTask = Task { @MainActor in
                do {
                    let outcome = try await s.awaitWelcome()
                    if Task.isCancelled { return }
                    switch outcome {
                    case .received(let welcome, let welcomeCursor):
                        state = .joining
                        do {
                            try await s.accept(
                                welcomeBytes: welcome,
                                welcomeCursor: welcomeCursor
                            )
                            state = .done
                        } catch {
                            state = .error(error.localizedDescription)
                        }
                    case .timeout:
                        state = .error("Timed out waiting for the other device. Try again.")
                    case .cancelled:
                        state = .idle
                    }
                } catch {
                    if Task.isCancelled { return }
                    state = .error(error.localizedDescription)
                }
            }
        } catch {
            state = .error(error.localizedDescription)
        }
    }

    private func cancel() {
        pollTask?.cancel()
        pollTask = nil
        session = nil
        if case .waiting = state {
            state = .idle
        }
    }
}

// MARK: - Scan (scanner device)

private struct ScanView: View {

    let accountIdBytes: Data
    let accountIdB64: String

    @State private var state: ScanState = .idle

    private enum ScanState: Equatable {
        case idle
        case scanning
        case processing
        case done
        case error(String)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "Aim your camera at the QR code your new device is " +
                "showing. The pairing completes within a couple of seconds."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: { state = .scanning }) {
                    Text("Open camera").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .scanning:
                VStack {
                    QrScannerView(
                        onDetected: { text in handleDetected(text: text) },
                        onError: { msg in state = .error(msg) }
                    )
                    .frame(height: 280)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    Button("Cancel", role: .cancel, action: { state = .idle })
                        .padding(.top, 4)
                }

            case .processing:
                ProgressView("Adding the device…")

            case .done:
                Label(
                    "Device added. The other device should land on its " +
                    "success screen any moment now.",
                    systemImage: "checkmark.circle.fill"
                )
                .foregroundStyle(.green)
                .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button("Try again", action: { state = .idle })
                }
            }
        }
    }

    private func handleDetected(text: String) {
        guard case .scanning = state else { return }
        guard let bytes = Self.base64UrlDecodeNoPad(text) else {
            // Scanned QR didn't carry our base64url payload —
            // keep the scanner running; the user might
            // re-aim at the right code.
            return
        }
        state = .processing
        Task { @MainActor in
            do {
                _ = try await PairingOrchestrator.runScanFlow(
                    scannedBytes: bytes,
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64
                )
                state = .done
            } catch {
                state = .error(error.localizedDescription)
            }
        }
    }

    private static func base64UrlDecodeNoPad(_ s: String) -> Data? {
        var b64 = s
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let pad = (4 - (b64.count % 4)) % 4
        b64 += String(repeating: "=", count: pad)
        return Data(base64Encoded: b64)
    }
}

// MARK: - Your devices (list + remove)

private struct DevicesListView: View {

    let accountIdBytes: Data
    let accountIdB64: String
    /// Bearer token for the action-binding ceremony (G-13). The
    /// device-removal gate is conditional: it only runs when the
    /// account has a passkey, so the token is enough to drive the
    /// authed `/auth/action/start` probe.
    let sessionToken: String

    @State private var state: ListState = .loading
    @State private var removingLeaf: UInt32?

    private enum ListState: Equatable {
        case loading
        case ready([DeviceRemoval.DeviceListItem])
        case error(String)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "These are the devices currently linked to your " +
                "account. Removing a device revokes its access " +
                "via an MLS commit broadcast on the DeviceGroup " +
                "journal — the revoked device can no longer " +
                "decrypt any subsequent traffic on this account."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .loading:
                ProgressView("Loading devices…")
            case .ready(let devices):
                ForEach(devices) { device in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("#\(device.leafIndex) \(hexPrefix(device.signaturePublicKey, n: 8))")
                                .font(.system(.footnote, design: .monospaced))
                            if device.isSelf {
                                Text("this device")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Spacer()
                        if device.isSelf {
                            Text("—")
                                .foregroundStyle(.secondary)
                        } else {
                            Button(role: .destructive) {
                                Task { await remove(leafIndex: device.leafIndex) }
                            } label: {
                                if removingLeaf == device.leafIndex {
                                    Text("Removing…")
                                } else {
                                    Text("Remove")
                                }
                            }
                            .disabled(removingLeaf != nil)
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                        }
                    }
                    .padding(.vertical, 4)
                    Divider()
                }
                Button(action: { refresh() }) {
                    Text("Refresh").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(removingLeaf != nil)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button("Try again") { refresh() }
                }
            }
        }
        .onAppear { refresh() }
    }

    private func refresh() {
        state = .loading
        do {
            let list = try DeviceRemoval.listDevices(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64
            )
            state = .ready(list.devices)
        } catch {
            state = .error(error.localizedDescription)
        }
    }

    @MainActor
    private func remove(leafIndex: UInt32) async {
        removingLeaf = leafIndex
        defer { removingLeaf = nil }
        do {
            // G-13 (ADR-0014): gate the eviction on a fresh passkey
            // gesture cryptographically bound to the removed leaf —
            // but only if the account actually has a passkey.
            // OPAQUE-only accounts (no passkey) fall through on
            // session-token auth alone: a 404 from the ceremony
            // start signals "no credentials". A user-cancelled
            // gesture or any other failure aborts the removal
            // (fail-closed).
            // G-13 (ADR-0015): the ceremony yields an attestation bound to the
            // removed leaf; carry it with the Remove commit so peers verify it.
            var attestation: Data?
            if let target = currentDevices().first(where: { $0.leafIndex == leafIndex }) {
                let wc = WebauthnClient()
                wc.setSessionToken(sessionToken)
                let actionHashB64 = actionHashForRemoveDevice(
                    signaturePublicKey: target.signaturePublicKey
                )
                do {
                    let result = try await wc.assertAction(actionHashB64: actionHashB64)
                    attestation = base64UrlDecodeNoPad(result.attestation_b64)
                } catch let error as WebauthnClientError {
                    // 404 = the account has no passkey credential
                    // (the server's NoCredentials path). Proceed
                    // without the gate; any other HTTP / network /
                    // decode failure aborts.
                    if case .http(404, _, _) = error {
                        // No passkey — fall through to the removal.
                    } else {
                        throw error
                    }
                }
            }

            try await DeviceRemoval.removeDeviceAt(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64,
                leafIndex: leafIndex,
                attestation: attestation
            )
            refresh()
        } catch {
            state = .error(error.localizedDescription)
        }
    }

    /// Snapshot of the currently-listed devices, for resolving the
    /// removed leaf's signature public key in ``remove(leafIndex:)``.
    private func currentDevices() -> [DeviceRemoval.DeviceListItem] {
        if case .ready(let devices) = state {
            return devices
        }
        return []
    }

    private func hexPrefix(_ data: Data, n: Int) -> String {
        let take = min(n, data.count)
        return data.prefix(take).map { String(format: "%02x", $0) }.joined()
    }
}
