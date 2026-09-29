// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Settings > Devices (UX spec section 5.5): the list of this account's
/// devices with Remove, and one "Add a device" button that pushes
/// ``AddDeviceScreen``. Pushed onto the Settings tab's stack; the system back
/// button returns to Settings.
///
/// Mirror of `apps/web/src/routes/Devices.tsx`. The recovery factors that used
/// to share this screen's segmented control (authenticator, email or phone)
/// live under Settings > Recovery (``RecoveryFactorsScreen``).
public struct DevicesScreen: View {

    @ObservedObject public var authRepository: AuthRepository
    /// Push the pairing flow (show QR / scan QR).
    public var onAddDevice: () -> Void
    /// ADR-0050 section 2.10 — called with a device's 32-byte key once it was
    /// removed from the DeviceGroup; the composition root retires it from
    /// every chat.
    public var onDeviceRemoved: (Data) -> Void

    public init(
        authRepository: AuthRepository,
        onAddDevice: @escaping () -> Void = {},
        onDeviceRemoved: @escaping (Data) -> Void = { _ in }
    ) {
        self.authRepository = authRepository
        self.onAddDevice = onAddDevice
        self.onDeviceRemoved = onDeviceRemoved
    }

    public var body: some View {
        List {
            if let session = DeviceSession.current(authRepository) {
                DevicesListView(
                    accountIdBytes: session.accountIdBytes,
                    accountIdB64: session.accountIdB64,
                    sessionToken: session.sessionToken,
                    onDeviceRemoved: onDeviceRemoved
                )
                Section {
                    Button(action: onAddDevice) {
                        Label("Add a device", systemImage: "plus")
                    }
                } footer: {
                    Text("Show a code on the new device and scan it here, or the other way round.")
                }
            } else {
                Section {
                    Text("Sign in first to manage devices.")
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Devices")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// The session values every device flow needs, or `nil` when the session is
/// not ready.
struct DeviceSession {
    let accountIdBytes: Data
    let accountIdB64: String
    let sessionToken: String
    let accountIdentitySeed: Data
    /// The account's canonical username; empty for a guest (ADR-0032).
    let username: String

    /// ADR-0056 — the login handle of this account's own username, which a
    /// pairing code must name; `nil` for a guest, who cannot add a device.
    var loginHandle: Data? { ownLoginHandle(username: username) }

    @MainActor
    static func current(_ authRepository: AuthRepository) -> DeviceSession? {
        guard
            let secrets = authRepository.identitySecrets,
            case .session(let account) = authRepository.state
        else {
            return nil
        }
        return DeviceSession(
            accountIdBytes: secrets.accountId,
            accountIdB64: account.accountIdB64,
            sessionToken: secrets.sessionToken,
            accountIdentitySeed: secrets.accountIdentitySeed,
            username: account.username
        )
    }
}

// MARK: - Add a device (pairing flow)

/// Settings > Devices > Add a device: this device either shows its pairing
/// code (it is the new device, it joins) or scans the other device's code (it
/// is the already-linked one, it admits). Two flows, one segmented switch.
///
///   1. **Show a code** — joining side. Generates the pairing material via
///      ``PairingOrchestrator/prepareJoinFlow``, renders the QR with
///      ``QrImageView``, waits for the other device, runs `accept` on receipt.
///   2. **Scan a code** — scanner side. Opens the camera via ``QrScannerView``,
///      decodes the first QR, drives ``PairingOrchestrator/runScanFlow``.
struct AddDeviceScreen: View {
    @ObservedObject var authRepository: AuthRepository
    /// ADR-0050 section 2.8 — called with a device's 32-byte key once the
    /// scan flow added it to the DeviceGroup; the composition root admits it
    /// to every chat.
    var onDeviceAdded: (Data) -> Void = { _ in }

    private enum Side: String, CaseIterable, Identifiable {
        case show
        case scan
        var id: String { rawValue }
        var labelKey: String {
            switch self {
            case .show: return "devices.add.side.show"
            case .scan: return "devices.add.side.scan"
            }
        }

        /// One line under the switch saying which device this is.
        var explanationKey: String {
            switch self {
            case .show: return "devices.add.side.show_detail"
            case .scan: return "devices.add.side.scan_detail"
            }
        }
    }

    @State private var side: Side = .scan
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(l10n.t("devices.add.e2ee_note"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                if let session = DeviceSession.current(authRepository),
                   session.loginHandle == nil {
                    // ADR-0056 — a pairing code names the account by its
                    // username; a guest has none, so neither side can run.
                    Text(PairingScanFailure.needsUsernameCopy)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                } else if let session = DeviceSession.current(authRepository),
                          let handle = session.loginHandle {
                    Picker("", selection: $side) {
                        ForEach(Side.allCases) { s in
                            Text(l10n.t(s.labelKey)).tag(s)
                        }
                    }
                    .pickerStyle(.segmented)
                    Text(l10n.t(side.explanationKey))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)

                    switch side {
                    case .show:
                        PairView(
                            loginHandle: handle,
                            accountIdBytes: session.accountIdBytes,
                            accountIdB64: session.accountIdB64
                        )
                    case .scan:
                        ScanView(
                            ownLoginHandle: handle,
                            accountIdBytes: session.accountIdBytes,
                            accountIdB64: session.accountIdB64,
                            accountIdentitySeed: session.accountIdentitySeed,
                            sessionToken: session.sessionToken,
                            onDeviceAdded: onDeviceAdded
                        )
                    }
                } else {
                    Text(l10n.t("devices.add.sign_in_first"))
                        .foregroundStyle(.red)
                }
            }
            .padding(16)
        }
        .navigationTitle(l10n.t("devices.add.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - Recovery factors

/// Settings > Recovery (UX spec section 5.5): the recovery phrase status,
/// then the authenticator app and the email or phone number that can restore
/// ACCESS to the account (ADR-0018 factors (c) and (d)). The three-word vault
/// (ADR-0049) is web-only so far and has no row here.
struct RecoveryFactorsScreen: View {
    @ObservedObject var authRepository: AuthRepository

    /// Whether this device keeps a copy of the recovery phrase (the backup
    /// flow stores it in the Keychain). A read, never a reveal: no screen
    /// shows the phrase after signup.
    private var phraseStoredHere: Bool {
        guard case .session(let account) = authRepository.state else { return false }
        return ((try? PhraseStore().getPhrase(accountIdB64: account.accountIdB64)) ?? nil) != nil
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text(
                    "Your recovery phrase or another signed-in device restores "
                        + "everything. These extra methods get you back into the "
                        + "account if you lose your password."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 12) {
                    Label("Recovery phrase", systemImage: "key")
                        .font(.headline)
                    HStack(spacing: 8) {
                        Image(systemName: phraseStoredHere ? "checkmark.circle.fill" : "exclamationmark.circle")
                            .foregroundStyle(phraseStoredHere ? Color.lakkoSuccess : Color.lakkoWarning)
                        Text(phraseStoredHere
                             ? "Saved on this device."
                             : "Not saved on this device. Keep your written copy safe.")
                            .font(.body)
                    }
                    Text("The 24 words you wrote down at signup restore your identity, contacts and messages on a new device.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Divider()
                if let session = DeviceSession.current(authRepository) {
                    VStack(alignment: .leading, spacing: 12) {
                        Label("Authenticator app", systemImage: "lock.rotation")
                            .font(.headline)
                        TotpEnrollView(sessionToken: session.sessionToken)
                    }
                    Divider()
                    VStack(alignment: .leading, spacing: 12) {
                        Label("Email or phone", systemImage: "envelope")
                            .font(.headline)
                        ContactEnrollView(sessionToken: session.sessionToken)
                    }
                } else {
                    Text("Sign in first to manage recovery.")
                        .foregroundStyle(.red)
                }
            }
            .padding(16)
        }
        .navigationTitle("Recovery")
        .navigationBarTitleDisplayMode(.inline)
    }
}

// MARK: - Pair (joining device)

private struct PairView: View {

    /// ADR-0056 — this account's login handle, which the QR carries.
    let loginHandle: Data
    let accountIdBytes: Data
    let accountIdB64: String

    @State private var state: PairState = .idle
    @State private var session: PairingOrchestrator.JoinSession?
    @State private var pollTask: Task<Void, Never>?
    @ObservedObject private var l10n = Localization.shared

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
            Text(l10n.t("device_link.qr.scan_hint"))
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: { start() }) {
                    Text(l10n.t("device_link.show_code")).frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .preparing:
                ProgressView(l10n.t("common.preparing"))

            case .waiting(let payload):
                VStack {
                    QrImageView(payload: payload)
                        .padding(.vertical, 8)
                    Text(l10n.t("device_link.qr.waiting"))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button(l10n.t("common.cancel"), role: .cancel, action: cancel)
                        .padding(.top, 4)
                }
                .frame(maxWidth: .infinity)

            case .joining:
                ProgressView(l10n.t("devices.add.joining"))

            case .done:
                Label(l10n.t("devices.add.linked"), systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button(l10n.t("common.try_again"), action: { state = .idle })
                }
            }
        }
        .onDisappear { cancel() }
    }

    private func start() {
        state = .preparing
        do {
            let s = try PairingOrchestrator.prepareJoinFlow(
                loginHandle: loginHandle,
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
                            NSLog("[pairing] WARN join failed: \(error)")
                            state = .error(l10n.t("devices.add.error.generic"))
                        }
                    case .timeout:
                        state = .error(l10n.t("device_link.error.timeout"))
                    case .cancelled:
                        state = .idle
                    }
                } catch {
                    if Task.isCancelled { return }
                    NSLog("[pairing] WARN waiting for the other device failed: \(error)")
                    state = .error(AuthErrorCopy.shared(error) ?? l10n.t("devices.add.error.generic"))
                }
            }
        } catch {
            NSLog("[pairing] WARN pairing code not prepared: \(error)")
            state = .error(l10n.t("devices.add.error.generic"))
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

    /// ADR-0056 — this account's own login handle; a scanned code must
    /// name it.
    let ownLoginHandle: Data
    let accountIdBytes: Data
    let accountIdB64: String
    /// ADR-0018 factor (a) — the scanner's cached AccountIdentity
    /// (phrase) seed, sealed to the fresh device alongside a
    /// single-use device grant. `ScanRecovery` makes a defensive
    /// copy; `runScanFlow` consumes + zeroizes it.
    let accountIdentitySeed: Data
    /// Bearer token for the `/device/authorize/start` grant mint.
    let sessionToken: String
    /// ADR-0050 section 2.8 — the admission fan-out over every chat.
    let onDeviceAdded: (Data) -> Void

    @State private var state: ScanState = .idle
    @ObservedObject private var l10n = Localization.shared

    private enum ScanState: Equatable {
        case idle
        case scanning
        /// ADR-0018 / lakko-security finding #1 — a decoded pairing
        /// QR parks here until the user explicitly confirms: the scan
        /// now transfers DeviceGroup membership AND the identity seed
        /// AND a session grant, so it must not run on a bare camera
        /// glance.
        case confirm(Data)
        case processing
        case done
        case error(String)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("devices.scan.intro"))
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: { state = .scanning }) {
                    Text(l10n.t("devices.scan.open_camera")).frame(maxWidth: .infinity)
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
                    Button(l10n.t("common.cancel"), role: .cancel, action: { state = .idle })
                        .padding(.top, 4)
                }

            case .confirm(let scannedBytes):
                VStack(alignment: .leading, spacing: 12) {
                    Text(l10n.t("devices.scan.confirm.title"))
                        .font(.headline)
                    Text(l10n.t("devices.scan.confirm.body"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    HStack {
                        Button(action: { runTransfer(bytes: scannedBytes) }) {
                            Text(l10n.t("devices.scan.confirm.submit"))
                        }
                        .buttonStyle(.borderedProminent)
                        Button(l10n.t("common.cancel"), role: .cancel, action: { state = .idle })
                    }
                }

            case .processing:
                ProgressView(l10n.t("device_link.request.adding"))

            case .done:
                Label(l10n.t("device_link.request.added"), systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button(l10n.t("common.try_again"), action: { state = .idle })
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
        // Park for explicit confirmation (lakko-security #1) rather
        // than running the transfer on a bare decode.
        state = .confirm(bytes)
    }

    private func runTransfer(bytes: Data) {
        state = .processing
        Task { @MainActor in
            do {
                // ADR-0018 factor (a): pass the recovery half —
                // `runScanFlow` treats it strictly best-effort (a
                // failure there never breaks the pairing). The seed
                // rides as a dedicated copy inside `ScanRecovery`;
                // `runScanFlow` consumes (zeroizes) it. Mirror of the
                // web `Devices.tsx` scanner wiring.
                let token = sessionToken
                let recovery = PairingOrchestrator.ScanRecovery(
                    accountIdentitySeed: accountIdentitySeed,
                    deviceAuthorizeStart: {
                        try await AuthClient().deviceAuthorizeStart(
                            sessionToken: token
                        )
                    }
                )
                let scanned = try await PairingOrchestrator.runScanFlow(
                    scannedBytes: bytes,
                    ownLoginHandle: ownLoginHandle,
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64,
                    recovery: recovery
                )
                // ADR-0050 section 2.8 — admit the new device to every chat.
                // Not awaited here: it waits for the device to land and
                // publish its KeyPackage, which is minutes.
                onDeviceAdded(scanned.newDeviceKey)
                state = .done
            } catch {
                state = .error(PairingScanFailure.userFacing(error))
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
    /// ADR-0050 section 2.10 — the removal fan-out over every chat.
    let onDeviceRemoved: (Data) -> Void

    @State private var state: ListState = .loading
    @State private var removingLeaf: UInt32?

    private enum ListState: Equatable {
        case loading
        case ready([DeviceRemoval.DeviceListItem])
        case error(String)
    }

    var body: some View {
        Section {
            switch state {
            case .loading:
                ProgressView("Loading...")
            case .ready(let devices):
                // A device has no human label yet (no platform, model or
                // user-set name reaches the client), so the row says
                // "This device" / "Device N" rather than showing its key.
                // The record carries no added date either, so none is shown.
                ForEach(Array(devices.enumerated()), id: \.element.id) { index, device in
                    HStack(spacing: 12) {
                        Image(systemName: device.isSelf ? "iphone" : "laptopcomputer.and.iphone")
                            .foregroundStyle(Color.lakkoTextMuted)
                        Text(DeviceRowPresentation.title(isSelf: device.isSelf, position: index + 1))
                            .font(.body)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        if !device.isSelf {
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
                    .padding(.vertical, 2)
                }

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(Color.lakkoDanger)
                        .font(.footnote)
                    Button("Try again") { refresh() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                }
            }
        } header: {
            Text("Your devices")
        } footer: {
            Text("Removing a device signs it out of your account; it can no longer read anything sent afterwards.")
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
            let removedKey = currentDevices().first(where: { $0.leafIndex == leafIndex })?
                .signaturePublicKey
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
            // ADR-0050 section 2.10 — the device is out of the DeviceGroup;
            // now out of every chat and out of the directory.
            if let removedKey { onDeviceRemoved(removedKey) }
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

}

// ---- Authenticator (TOTP) enrollment — ADR-0018 factor (c) -------

/// Enroll an authenticator app as an ACCESS-recovery factor:
/// `/totp/enroll/start` renders the `otpauth://` QR (RAW TEXT —
/// authenticator apps parse the URI straight out of the QR) + the
/// manual-entry secret; `/totp/enroll/finish` confirms with a live
/// 6-digit code. Mirror of the web `TotpEnrollView` / Android
/// `TotpEnrollTab`.
///
/// There is no server "status" endpoint — the Enable button renders
/// by default and a 409 from either call flips the card to "already
/// enabled" (honest lazy state derivation, same as web/Android).
private struct TotpEnrollView: View {
    let sessionToken: String

    private enum UiState: Equatable {
        case idle
        case starting
        case confirm(otpauthUri: String, secretB32: String)
        case done
        case alreadyEnabled
        case unavailable
        case error(String)
    }

    @State private var state: UiState = .idle
    @State private var code: String = ""
    @State private var busy: Bool = false
    @State private var inlineError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "If you lose this device, a 6-digit code from your authenticator "
                    + "app gets you back into the account. Your contacts and "
                    + "messages then come back from your recovery phrase or another device."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: start) {
                    Text("Enable authenticator").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .starting:
                ProgressView()

            case .confirm(let otpauthUri, let secretB32):
                Text(
                    "Scan this QR with your authenticator app (or type " +
                    "the secret), then enter the current 6-digit code to " +
                    "confirm."
                )
                .font(.footnote)
                QrTextImageView(text: otpauthUri, size: 220)
                Text("Secret: \(secretB32)")
                    .font(.footnote.weight(.semibold))
                    .textSelection(.enabled)
                TextField("6-digit code", text: $code)
                    .textFieldStyle(.roundedBorder)
                    .keyboardType(.numberPad)
                    .textContentType(.oneTimeCode)
                    .disabled(busy)
                if let inlineError {
                    Text(inlineError)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
                Button(action: { confirm(otpauthUri: otpauthUri, secretB32: secretB32) }) {
                    Text(busy ? "Confirming…" : "Confirm")
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)

            case .done:
                Label(
                    "Authenticator enabled. A 6-digit code can now " +
                    "recover access to this account.",
                    systemImage: "checkmark.circle.fill"
                )
                .foregroundStyle(.green)
                .font(.footnote)

            case .alreadyEnabled:
                Text("An authenticator is already enabled on this account.")
                    .font(.body)

            case .unavailable:
                Text("Authenticator enrollment is not available on this server.")
                    .font(.body)
                    .foregroundStyle(.red)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(.red)
                    Button("Try again", action: { state = .idle })
                }
            }
        }
    }

    private func start() {
        state = .starting
        Task { @MainActor in
            do {
                let res = try await AuthClient().totpEnrollStart(
                    sessionToken: sessionToken
                )
                state = .confirm(
                    otpauthUri: res.otpauthUri,
                    secretB32: res.secretB32
                )
            } catch let err as AuthClientError {
                state = mapStartError(err)
            } catch {
                state = .error(error.localizedDescription)
            }
        }
    }

    private func confirm(otpauthUri: String, secretB32: String) {
        guard let normalized = normalizeTotpCode(code) else {
            inlineError = "Enter the 6-digit code from the app."
            return
        }
        busy = true
        inlineError = nil
        Task { @MainActor in
            do {
                try await AuthClient().totpEnrollFinish(
                    sessionToken: sessionToken,
                    code: normalized
                )
                busy = false
                state = .done
            } catch let err as AuthClientError {
                busy = false
                if case .http(let status, _, _) = err {
                    switch status {
                    case 401:
                        inlineError = "Wrong code — check the app and try again."
                        // Keep the QR up; the user re-reads the code.
                        state = .confirm(
                            otpauthUri: otpauthUri,
                            secretB32: secretB32
                        )
                    case 409:
                        state = .alreadyEnabled
                    default:
                        state = .error("Confirmation failed (\(status)).")
                    }
                } else {
                    state = .error("Confirmation failed.")
                }
            } catch {
                busy = false
                state = .error(error.localizedDescription)
            }
        }
    }

    private func mapStartError(_ err: AuthClientError) -> UiState {
        if let limit = AuthRateLimit.copy(for: err) { return .error(limit) }
        if case .http(let status, _, _) = err {
            switch status {
            case 409: return .alreadyEnabled
            case 503: return .unavailable
            default: return .error("Enrollment failed (\(status)).")
            }
        }
        return .error("Enrollment failed.")
    }
}

// MARK: - Email / phone OTP enrollment (ADR-0018 factor (d))

/// Enroll a phone number or email as an ACCESS-recovery factor:
/// `/recovery-contact/enroll/start` sends a 6-digit code to the
/// supplied contact; `/recovery-contact/enroll/finish` confirms it
/// with a live code. Mirror of the web `ContactEnrollView` (in
/// `apps/web/src/routes/Devices.tsx`).
///
/// HONESTY NOTE: there is no server "status" endpoint for the
/// recovery contact, so the enrolled/not-enrolled state cannot be
/// probed up front — the request form renders by default. A 503 from
/// enroll/start flips this card to "not available", a 409 to "already
/// enabled" (honest lazy state derivation, same posture as web /
/// Android and the TOTP card above).
private struct ContactEnrollView: View {
    let sessionToken: String

    private enum UiState: Equatable {
        case request
        case starting
        // A code has been sent; waiting for the confirm code.
        case confirm(kind: ContactKind, contact: String)
        case finishing(kind: ContactKind, contact: String)
        case done
        case alreadyEnabled
        case unavailable
        case error(String)
    }

    @State private var state: UiState = .request
    @State private var contactKind: ContactKind = .email
    @State private var contact: String = ""
    @State private var code: String = ""
    @State private var requestError: String?
    @State private var confirmError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(
                "If you lose this device, a one-time code sent to this email or "
                    + "phone number gets you back into the account. Your contacts and "
                    + "messages then come back from your recovery phrase or another device."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .request, .starting:
                let busy = state == .starting
                Picker("Contact type", selection: $contactKind) {
                    Text("Email").tag(ContactKind.email)
                    Text("SMS").tag(ContactKind.phone)
                }
                .pickerStyle(.segmented)
                .disabled(busy)

                TextField(
                    contactKind == .email
                        ? "alice@example.com"
                        : "+15551234567",
                    text: $contact
                )
                .textFieldStyle(.roundedBorder)
                .keyboardType(contactKind == .email ? .emailAddress : .phonePad)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .disabled(busy)

                if let requestError {
                    Text(requestError)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
                Button(action: start) {
                    Text(busy ? "Sending…" : "Send code").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(busy)

            case .confirm(let kind, let contactValue),
                 .finishing(let kind, let contactValue):
                let busy: Bool = {
                    if case .finishing = state { return true }
                    return false
                }()
                Text(
                    "Enter the 6-digit code we sent to confirm this " +
                    "\(kind == .email ? "email" : "phone number")."
                )
                .font(.footnote)
                TextField("6-digit code", text: $code)
                    .textFieldStyle(.roundedBorder)
                    .keyboardType(.numberPad)
                    .textContentType(.oneTimeCode)
                    .disabled(busy)
                if let confirmError {
                    Text(confirmError)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
                Button(action: { confirm(kind: kind, contact: contactValue) }) {
                    Text(busy ? "Confirming…" : "Confirm")
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)

            case .done:
                Label(
                    "Recovery contact enabled. A one-time code can now " +
                    "recover access to this account.",
                    systemImage: "checkmark.circle.fill"
                )
                .foregroundStyle(.green)
                .font(.footnote)

            case .alreadyEnabled:
                Text("A recovery contact is already enabled on this account.")
                    .font(.body)

            case .unavailable:
                Text("Email / SMS recovery is not available on this server.")
                    .font(.body)
                    .foregroundStyle(.red)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(.red)
                    Button("Try again", action: { state = .request })
                }
            }
        }
    }

    private func start() {
        requestError = nil
        // Light client-side shape check — saves a round trip on an
        // obviously-malformed contact. The server is authoritative.
        guard isPlausibleContact(kind: contactKind, contact: contact) else {
            requestError = contactKind == .email
                ? "Enter a valid email address."
                : "Enter a phone number in international format (e.g. +15551234567)."
            return
        }
        let kindSnapshot = contactKind
        let contactSnapshot = contact
        state = .starting
        Task { @MainActor in
            do {
                try await AuthClient().recoveryContactEnrollStart(
                    sessionToken: sessionToken,
                    kind: kindSnapshot,
                    contact: contactSnapshot
                )
                code = ""
                confirmError = nil
                state = .confirm(kind: kindSnapshot, contact: contactSnapshot)
            } catch let err as AuthClientError {
                state = mapStartError(err, kind: kindSnapshot, contact: contactSnapshot)
            } catch {
                state = .error(error.localizedDescription)
            }
        }
    }

    private func confirm(kind: ContactKind, contact contactValue: String) {
        guard let normalized = normalizeTotpCode(code) else {
            confirmError = "Enter the 6-digit code we sent."
            return
        }
        confirmError = nil
        state = .finishing(kind: kind, contact: contactValue)
        Task { @MainActor in
            do {
                try await AuthClient().recoveryContactEnrollFinish(
                    sessionToken: sessionToken,
                    kind: kind,
                    code: normalized
                )
                code = ""
                state = .done
            } catch let err as AuthClientError {
                if case .http(let status, _, _) = err {
                    switch status {
                    case 401:
                        // Wrong / stale code — the pending challenge
                        // survives, so let the user retry with the code.
                        confirmError =
                            "That code didn't verify. Check the code we sent, or re-send."
                        state = .confirm(kind: kind, contact: contactValue)
                    case 409:
                        state = .alreadyEnabled
                    case 503:
                        state = .unavailable
                    default:
                        state = .error("Confirmation failed (\(status)).")
                    }
                } else {
                    state = .error("Confirmation failed.")
                }
            } catch {
                state = .error(error.localizedDescription)
            }
        }
    }

    private func mapStartError(
        _ err: AuthClientError,
        kind: ContactKind,
        contact contactValue: String
    ) -> UiState {
        if let limit = AuthRateLimit.copy(for: err) { return .error(limit) }
        if case .http(let status, _, _) = err {
            switch status {
            case 409:
                return .alreadyEnabled
            case 503:
                return .unavailable
            case 400:
                // Malformed contact — bounce back to the request step so
                // the user can correct it (mirror of the web flow).
                requestError = "That contact looks malformed. Check it and retry."
                return .request
            default:
                return .error("Enrollment failed (\(status)).")
            }
        }
        return .error("Enrollment failed.")
    }
}
