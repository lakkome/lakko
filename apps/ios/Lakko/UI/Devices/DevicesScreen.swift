// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Settings > Devices (UX spec section 5.5): the list of this account's
/// devices with Remove, and one "Add a device" button that pushes
/// ``AddDeviceScreen``. Pushed onto the Settings tab's stack; the system back
/// button returns to Settings.
///
/// Mirror of `apps/web/src/routes/Devices.tsx`. The ways back into the
/// account live under Settings > Security > Recovery
/// (``RecoverySettingsScreen``).
public struct DevicesScreen: View {

    @ObservedObject public var authRepository: AuthRepository
    /// UX 15.3 — the reset note was dismissed (for this screen instance).
    @State private var resetNoteDismissed = false
    /// Push the pairing flow (show QR / scan QR).
    public var onAddDevice: () -> Void
    /// UX 17.5 — `Change password` on the line shown after a `New` device
    /// was removed.
    public var onChangePassword: () -> Void
    /// Called with a device's 32-byte key once it was signed out at the
    /// server (ADR-0074 section 2.3); the removal itself, with its owed
    /// steps, is the runner's.
    public var onDeviceRemoved: (Data) -> Void
    /// ADR-0074 section 2.3 — the removal flow and its owed records.
    private let removal: DeviceRemovalRunner
    /// ADR-0074 section 3.2 — "It was lost or stolen": remove the device and
    /// change the account keys. `nil` offers the plain removal only.
    private let rotation: KeyRotationRunner?

    @ObservedObject private var l10n = Localization.shared

    public init(
        authRepository: AuthRepository,
        removal: DeviceRemovalRunner,
        rotation: KeyRotationRunner? = nil,
        onAddDevice: @escaping () -> Void = {},
        onChangePassword: @escaping () -> Void = {},
        onDeviceRemoved: @escaping (Data) -> Void = { _ in }
    ) {
        self.authRepository = authRepository
        self.removal = removal
        self.rotation = rotation
        self.onChangePassword = onChangePassword
        self.onAddDevice = onAddDevice
        self.onDeviceRemoved = onDeviceRemoved
    }

    public var body: some View {
        List {
            if let session = DeviceSession.current(authRepository) {
                // UX 17.5 — the same banner as on Chats (no `Open Devices`
                // here), or the change-password line after a `New` device
                // was removed.
                NewDeviceNoticeRow(onChangePassword: onChangePassword)
                if authRepository.keysResetThisSession, !resetNoteDismissed {
                    // UX 15.3 — one dismissible note for the rest of this
                    // session: the other devices were signed out and removed
                    // (they are not devices of the account any more, so the
                    // list below shows this device alone).
                    Section {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: "info.circle")
                                .foregroundStyle(Color.lakkoAccent)
                            Text(l10n.t("settings.devices.reset_note"))
                                .font(.subheadline)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Button {
                                resetNoteDismissed = true
                            } label: {
                                Image(systemName: "xmark")
                                    .font(.footnote.weight(.semibold))
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(l10n.t("common.dismiss"))
                        }
                    }
                }
                DevicesListView(
                    authRepository: authRepository,
                    removal: removal,
                    rotation: rotation,
                    accountIdBytes: session.accountIdBytes,
                    accountIdB64: session.accountIdB64,
                    sessionToken: session.sessionToken,
                    onDeviceRemoved: onDeviceRemoved
                )
                Section {
                    Button(action: onAddDevice) {
                        Label(l10n.t("devices.add.title"), systemImage: "plus")
                    }
                } footer: {
                    Text(l10n.t("settings.devices.add.lede"))
                }
            } else {
                Section {
                    Text(l10n.t("settings.devices.sign_in_first"))
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
        }
        .listStyle(.insetGrouped)
        // A UIKit-backed List keeps the mirroring it was built with, so it
        // is rebuilt when the layout direction changes.
        .id(l10n.layoutDirection)
        .navigationTitle(l10n.t("settings.devices.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// The session values every device flow needs, or `nil` when the session is
/// not ready.
/// The new-device notice as a row of the Devices list: present only while
/// there is something to say, so no empty section is left behind.
private struct NewDeviceNoticeRow: View {
    @ObservedObject private var center = NewDeviceNoticeCenter.shared
    let onChangePassword: () -> Void

    var body: some View {
        if center.banner != nil || center.showsRemovedLine {
            Section {
                NewDeviceNoticeBanner(
                    center: center, onOpenDevices: nil, onChangePassword: onChangePassword)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }
    }
}

struct DeviceSession {
    let accountIdBytes: Data
    let accountIdB64: String
    let sessionToken: String
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
                            authRepository: authRepository,
                            ownLoginHandle: handle,
                            accountIdBytes: session.accountIdBytes,
                            accountIdB64: session.accountIdB64,
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
        case error(UiText)
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
                    Label(message.text, systemImage: "exclamationmark.triangle.fill")
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
                            state = .error(.key("devices.add.error.generic"))
                        }
                    case .timeout:
                        state = .error(.key("device_link.error.timeout"))
                    case .cancelled:
                        state = .idle
                    }
                } catch {
                    if Task.isCancelled { return }
                    NSLog("[pairing] WARN waiting for the other device failed: \(error)")
                    state = .error(UiText {
                        AuthErrorCopy.shared(error) ?? Localization.text("devices.add.error.generic")
                    })
                }
            }
        } catch {
            NSLog("[pairing] WARN pairing code not prepared: \(error)")
            state = .error(.key("devices.add.error.generic"))
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
    /// Bearer token for the `/device/authorize/start` grant mint.
    let sessionToken: String
    /// ADR-0050 section 2.8 — the admission fan-out over every chat.
    let onDeviceAdded: (Data) -> Void

    private let authRepository: AuthRepository
    /// UX 17.4 — the proof block of `Link this device?` (absent while a
    /// proof is live).
    @StateObject private var proof: FreshAuthProofModel
    @State private var state: ScanState = .idle
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository,
        ownLoginHandle: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        sessionToken: String,
        onDeviceAdded: @escaping (Data) -> Void
    ) {
        self.authRepository = authRepository
        self.ownLoginHandle = ownLoginHandle
        self.accountIdBytes = accountIdBytes
        self.accountIdB64 = accountIdB64
        self.sessionToken = sessionToken
        self.onDeviceAdded = onDeviceAdded
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .addDeviceScan, repository: authRepository))
    }

    private enum ScanState: Equatable {
        case idle
        case scanning
        /// A decoded pairing QR parks here until the user explicitly
        /// confirms: the scan transfers DeviceGroup membership AND the
        /// account's keys AND a session grant, so it must not run on a
        /// bare camera glance.
        case confirm(Data)
        case processing
        /// The device was added; `keysSent` says whether this device could
        /// hand over the keys with it.
        case done(keysSent: Bool)
        case error(UiText)
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
                        onError: { key in state = .error(.key(key)) }
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
                    // UX 17.4 — the same block as "Is this you?", between
                    // the body and `Link device`. A failure leaves this step
                    // open with the scanned code kept.
                    FreshAuthBlock(model: proof, onSubmit: { link(bytes: scannedBytes) })
                    HStack {
                        Button(action: { link(bytes: scannedBytes) }) {
                            Text(l10n.t(proof.layout.working
                                ? "common.checking" : "devices.scan.confirm.submit"))
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(!proof.layout.ready)
                        Button(l10n.t("common.cancel"), role: .cancel, action: {
                            proof.closed()
                            state = .idle
                        })
                    }
                }
                .task { await proof.opened() }

            case .processing:
                ProgressView(l10n.t("device_link.request.adding"))

            case .done(let keysSent):
                Label(l10n.t("device_link.request.added"), systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)
                if !keysSent {
                    Label(l10n.t("device_link.keys_not_sent"), systemImage: "exclamationmark.circle")
                        .foregroundStyle(Color.lakkoWarning)
                        .font(.footnote)
                }

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message.text, systemImage: "exclamationmark.triangle.fill")
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

    /// `Link device`: the proof first; nothing of the hand-over starts
    /// before it passes.
    private func link(bytes: Data) {
        guard proof.layout.ready else { return }
        Task { @MainActor in
            guard await proof.ensure() else { return }
            guard case .confirm = state else { return }
            proof.closed()
            runTransfer(bytes: bytes)
        }
    }

    private func runTransfer(bytes: Data) {
        state = .processing
        Task { @MainActor in
            do {
                // The recovery half: a single-use grant and the entropy of
                // the recovery phrase this device holds (recovery payload
                // v2). `runScanFlow` treats it best-effort: a failure there
                // never breaks the pairing, and with no phrase stored here
                // no keys are sent (the person is told so below).
                // The proof `link(bytes:)` just obtained; read once so the
                // grant mint carries exactly it.
                guard let grant = authRepository.liveFreshGrant() else {
                    proof.proofExpired()
                    state = .confirm(bytes)
                    return
                }
                let token = authRepository.identitySecrets?.sessionToken ?? sessionToken
                let recovery = PairingOrchestrator.ScanRecovery(
                    accountIdB64: accountIdB64,
                    deviceAuthorizeStart: {
                        try await AuthClient().deviceAuthorizeStart(
                            sessionToken: token, freshGrant: grant
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
                state = .done(keysSent: scanned.keysSent)
            } catch {
                state = .error(UiText { PairingScanFailure.userFacing(error) })
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

    let authRepository: AuthRepository
    /// ADR-0074 section 2.3 — the removal flow; its pending set marks the
    /// rows whose device is signed out and still being removed.
    @ObservedObject var removal: DeviceRemovalRunner
    /// ADR-0074 section 3.2 — the "remove and change keys" flow and what a
    /// stored record of it asks of this screen.
    let rotation: KeyRotationRunner?
    /// What the stored rotation record asks for (re-read when the screen
    /// opens and after the sheet closes).
    @State private var rotationResume: KeyRotationPlan.Resume = .nothing
    /// The new words of a rotation that finished after the sheet was closed.
    @State private var rotatedWords: RotatedWords?
    let accountIdBytes: Data
    let accountIdB64: String
    /// Bearer token for the ADR-0015 action-binding ceremony.
    let sessionToken: String
    /// The device was signed out at the server.
    let onDeviceRemoved: (Data) -> Void

    @State private var state: ListState = .loading
    /// The device whose remove sheet is open.
    @State private var removing: RemoveTarget?
    /// The line under the list after a removal or a `Finish removing`.
    @State private var note: DeviceRemovalNote?
    @ObservedObject private var l10n = Localization.shared
    /// UX 17.5 — which rows carry `New`.
    @ObservedObject private var notice = NewDeviceNoticeCenter.shared
    /// ADR-0080 — what each device said about itself.
    @ObservedObject private var cards = DeviceCardCenter.shared
    /// UX section 3 — the Remove all other devices sheet, the run it starts,
    /// and each row's own progress while it runs.
    @State private var showingRemoveAll = false
    @State private var removingAll = false
    @State private var rowRun: [String: RowRun] = [:]

    private enum RowRun: Equatable {
        case removing
        case failed
    }

    private enum ListState: Equatable {
        case loading
        case ready([DeviceRemoval.DeviceListItem])
        case error(UiText)
    }

    private struct RemoveTarget: Identifiable {
        let key: Data
        let title: String
        /// Open straight on "It was lost or stolen" (`Try again`).
        var stolen = false
        var id: Data { key }
    }

    private struct RotatedWords: Identifiable {
        let phrase: String
        var id: String { phrase }
    }

    /// UX 18.1 — what a rotation that did not finish in its sheet leaves on
    /// this screen: "the device was removed, but your account keys were not
    /// changed" with `Try again` / `Not now`; the owed publish (it retries by
    /// itself); or the new words still to be confirmed.
    @ViewBuilder private var rotationFooter: some View {
        switch rotationResume {
        case .nothing:
            EmptyView()
        case .removedNotRotated:
            VStack(alignment: .leading, spacing: 6) {
                Text(l10n.t(RotateDeviceCopy.notStarted))
                    .foregroundStyle(Color.lakkoDanger)
                HStack(spacing: 12) {
                    Button(l10n.t(RotateDeviceCopy.retry)) {
                        if let key = rotation?.storedTargetDeviceId() {
                            removing = RemoveTarget(
                                key: key,
                                // The second choice's title names no device.
                                title: "",
                                stolen: true)
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    Button(l10n.t(RotateDeviceCopy.notNow)) {
                        rotation?.abandonUnstarted()
                        refreshRotation()
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
        case .commitLandedUnseen, .finishOwed:
            Text(l10n.t(RotateDeviceCopy.publishOwed))
                .foregroundStyle(Color.lakkoDanger)
        case .showWords:
            VStack(alignment: .leading, spacing: 6) {
                Text(l10n.t(RotateDeviceCopy.done))
                Button(l10n.t(KeysRotatedCopy.showPhrase)) {
                    if let phrase = rotation?.wordsToShow() {
                        rotatedWords = RotatedWords(phrase: phrase)
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
    }

    private func refreshRotation() {
        rotation?.reload()
        rotationResume = rotation?.resume ?? .nothing
    }

    /// ADR-0080 — the words a row needs, in the interface language.
    private var rowWords: DeviceCards.Words {
        let locale = l10n.locale
        return DeviceCards.Words(
            device: l10n.t("devices.row.device"),
            online: l10n.t("devices.row.online"),
            app: { version in
                version.isEmpty ? l10n.t("devices.row.app_name") : l10n.t("devices.row.app", texts: ["version": version])
            },
            place: { city, country in l10n.t("devices.row.place", texts: ["city": city, "country": country]) },
            countryName: { code in locale.localizedString(forRegionCode: code) ?? code })
    }

    private func rows(_ devices: [DeviceRemoval.DeviceListItem]) -> [DeviceCards.RowInput] {
        devices.map { device in
            let keyHex = hexEncode(device.signaturePublicKey)
            return DeviceCards.RowInput(
                keyHex: keyHex, leafIndex: device.leafIndex, isSelf: device.isSelf, card: cards.cards[keyHex])
        }
    }

    private static func iconName(_ kind: DeviceCards.IconKind) -> String {
        switch kind {
        case .phone: return "iphone"
        case .tablet: return "ipad"
        case .computer: return "laptopcomputer"
        case .browser: return "globe"
        case .unknown: return "laptopcomputer.and.iphone"
        }
    }

    @ViewBuilder
    private func deviceRow(_ input: DeviceCards.RowInput, _ devices: [DeviceRemoval.DeviceListItem]) -> some View {
        if let device = devices.first(where: { hexEncode($0.signaturePublicKey) == input.keyHex }) {
            let view = DeviceCards.view(
                input, words: rowWords, nowMs: Int64(Date().timeIntervalSince1970 * 1000),
                calendar: .current, locale: l10n.locale)
            let deviceIdB64 = device.signaturePublicKey.base64EncodedString()
            let run = rowRun[input.keyHex]
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: Self.iconName(view.icon))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(view.title)
                            .font(.body)
                            .foregroundStyle(Color.lakkoText)
                        if notice.isNew(signaturePublicKey: device.signaturePublicKey) {
                            // Text, not colour alone.
                            Text(l10n.t("settings.devices.new_badge"))
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Color.lakkoAccent)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .overlay(Capsule().stroke(Color.lakkoAccent, lineWidth: 1))
                        }
                    }
                    if !view.detail.isEmpty {
                        Text(view.detail).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    }
                    if !view.place.isEmpty {
                        Text(view.place).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    }
                    if run == .removing {
                        Text(l10n.t(RemoveDeviceCopy.working)).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    } else if run == .failed {
                        Text(l10n.t("devices.remove_all.row_failed")).font(.footnote).foregroundStyle(Color.lakkoDanger)
                    }
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 6) {
                    if !view.activity.isEmpty {
                        Text(view.activity).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
                    }
                    if run != .removing {
                        switch DeviceRowPresentation.control(
                            isSelf: device.isSelf,
                            removalPending: removal.pendingDeviceIds.contains(deviceIdB64),
                            waitingOnPerson: removal.waitingOnPersonDeviceIds.contains(deviceIdB64)) {
                        case .none:
                            EmptyView()
                        case .pending:
                            // UX 18.1 — signed out, removal still finishing:
                            // a label and no Remove control.
                            Text(l10n.t(RemoveDeviceCopy.pending))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                        case .pendingFinish:
                            let finishing = removal.finishingDeviceIds.contains(deviceIdB64)
                            Text(l10n.t(RemoveDeviceCopy.pending))
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoTextMuted)
                            Button {
                                finishRemoval(device.signaturePublicKey)
                            } label: {
                                Text(l10n.t(RemoveDeviceCopy.finish))
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(finishing)
                        case .remove:
                            Button(role: .destructive) {
                                rowRun[input.keyHex] = nil
                                removing = RemoveTarget(key: device.signaturePublicKey, title: view.title)
                            } label: {
                                Text(l10n.t("common.remove"))
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .disabled(removingAll)
                        }
                    }
                }
            }
            .padding(.vertical, 2)
            .accessibilityElement(children: .contain)
        }
    }

    @ViewBuilder private var listFooter: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(l10n.t("settings.devices.list.lede"))
            if let note {
                Text(l10n.t(note.key))
                    .foregroundStyle(note.isError ? Color.lakkoDanger : Color.lakkoTextMuted)
            }
            rotationFooter
        }
    }

    var body: some View {
        Group {
            switch state {
            case .loading:
                Section {
                    ProgressView(l10n.t("common.loading"))
                } header: {
                    Text(l10n.t("settings.devices.list.title"))
                }
            case .ready(let devices):
                let ordered = DeviceCards.order(rows(devices))
                if let current = ordered.current {
                    Section {
                        deviceRow(current, devices)
                        if !ordered.others.isEmpty {
                            Button(role: .destructive) {
                                showingRemoveAll = true
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(l10n.t("devices.remove_all"))
                                        .foregroundStyle(Color.lakkoDanger)
                                    Text(l10n.t("devices.remove_all.sub"))
                                        .font(.footnote)
                                        .foregroundStyle(Color.lakkoTextMuted)
                                }
                            }
                            .disabled(removingAll)
                        }
                    } header: {
                        Text(l10n.t("devices.section.this_device"))
                    } footer: {
                        if ordered.others.isEmpty { listFooter }
                    }
                }
                if !ordered.others.isEmpty {
                    Section {
                        ForEach(ordered.others, id: \.keyHex) { input in
                            deviceRow(input, devices)
                        }
                    } header: {
                        Text(l10n.t("devices.section.other_devices"))
                    } footer: {
                        listFooter
                    }
                }
            case .error(let message):
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Label(message.text, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(Color.lakkoDanger)
                            .font(.footnote)
                        Button(l10n.t("common.try_again")) { refresh() }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                } header: {
                    Text(l10n.t("settings.devices.list.title"))
                }
            }
        }
        .onAppear { refresh() }
        // A removal left unfinished (offline, a closed app) is retried while
        // this screen is up; the rows follow the runner's pending set.
        .task {
            removal.reload()
            await removal.resumeOwed()
            // A rotation that committed finishes its owed steps here too.
            refreshRotation()
            await rotation?.resumeOwed()
            refreshRotation()
            refresh()
        }
        // UX section 3 — Remove all other devices: one proof covers all.
        .sheet(isPresented: $showingRemoveAll) {
            RemoveAllDevicesSheet(
                authRepository: authRepository,
                blocked: removal.canStart(deviceId: Data()) == .blockedPublishOwed,
                onConfirmed: {
                    showingRemoveAll = false
                    removeAllOthers()
                },
                onClose: { showingRemoveAll = false }
            )
            .presentationDetents([.medium, .large])
        }
        // The new words of a rotation whose sheet was closed before they
        // could be shown (UX 18.1, "Keys changed, publishing still owed").
        .sheet(item: $rotatedWords) { words in
            RecoveryPhraseGuide(
                phrase: words.phrase,
                title: l10n.t(RotateDeviceCopy.phraseTitle),
                intro: l10n.t(RotateDeviceCopy.effectPhrase),
                extra: { EmptyView() },
                onDone: {
                    rotation?.acknowledgePhrase()
                    rotatedWords = nil
                    refreshRotation()
                }
            )
        }
        // UX 18.1 — the app's own sheet: it carries the proof block.
        .sheet(item: $removing) { target in
            RemoveDeviceSheet(
                deviceTitle: target.title,
                deviceKey: target.key,
                sessionToken: authRepository.identitySecrets?.sessionToken ?? sessionToken,
                authRepository: authRepository,
                runner: removal,
                rotation: rotation,
                startsWithStolen: target.stolen,
                onRemoved: { result in
                    removing = nil
                    note = DeviceRemovalNote.afterRemove(result)
                    onDeviceRemoved(target.key)
                    refresh()
                },
                onRotationClosed: { rotated in
                    // The device is out either way; the keys were changed
                    // (`devices.rotate.done`) or were not (the footer says
                    // so, with `Try again`).
                    removing = nil
                    note = rotated ? RotateSheetRules.doneNote : nil
                    onDeviceRemoved(target.key)
                    refreshRotation()
                    if rotation?.resume == .showWords { note = nil }
                    refresh()
                },
                onClose: {
                    removing = nil
                    refreshRotation()
                    refresh()
                }
            )
            .presentationDetents([.medium, .large])
        }
    }

    /// `Remove all`: the proof passed in the sheet; now each other device's
    /// own removal (ADR-0074), one after another, each with its ADR-0015
    /// gesture when the account has a passkey. A device that could not be
    /// removed keeps its row with "Could not remove. Try again." and its own
    /// `Remove`.
    private func removeAllOthers() {
        guard case .ready(let devices) = state, !removingAll else { return }
        let targets = devices.filter { device in
            !device.isSelf && !removal.pendingDeviceIds.contains(device.signaturePublicKey.base64EncodedString())
        }
        removingAll = true
        Task { @MainActor in
            defer {
                removingAll = false
                refresh()
            }
            for device in targets {
                let key = hexEncode(device.signaturePublicKey)
                rowRun[key] = .removing
                let attestation: Data?
                switch await RemoveDeviceAttestation.obtain(
                    sessionToken: authRepository.identitySecrets?.sessionToken ?? sessionToken,
                    deviceKey: device.signaturePublicKey) {
                case .notNeeded: attestation = nil
                case .obtained(let bytes): attestation = bytes
                case .cancelled:
                    rowRun[key] = .failed
                    continue
                }
                let result = await removal.remove(deviceId: device.signaturePublicKey, attestation: attestation)
                if RemoveDeviceSheetState.closes(on: result) {
                    rowRun[key] = nil
                    onDeviceRemoved(device.signaturePublicKey)
                } else {
                    rowRun[key] = .failed
                }
            }
        }
    }

    /// `Finish removing`: the passkey gesture, then the owed steps.
    private func finishRemoval(_ deviceKey: Data) {
        Task { @MainActor in
            note = DeviceRemovalNote.afterFinish(await removal.finish(deviceId: deviceKey))
            refresh()
        }
    }

    private func refresh() {
        do {
            let list = try DeviceRemoval.listDevices(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64
            )
            state = .ready(list.devices)
        } catch {
            NSLog("[devices] WARN device list not loaded: \(error)")
            state = .error(.key("settings.devices.error.load"))
        }
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
struct TotpEnrollView: View {
    let sessionToken: String

    private enum UiState: Equatable {
        case idle
        case starting
        case confirm(otpauthUri: String, secretB32: String)
        case done
        case alreadyEnabled
        case unavailable
        case error(UiText)
    }

    @State private var state: UiState = .idle
    @State private var code: String = ""
    @State private var busy: Bool = false
    @State private var inlineError: UiText?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("settings.recovery.totp.lede"))
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch state {
            case .idle:
                Button(action: start) {
                    Text(l10n.t("settings.recovery.totp.set_up")).frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .starting:
                ProgressView()

            case .confirm(let otpauthUri, let secretB32):
                Text(l10n.t("settings.recovery.totp.scan"))
                .font(.footnote)
                QrTextImageView(text: otpauthUri, size: 220)
                keyLine(secretB32)
                TextField(l10n.t("recovery.totp.code_label"), text: $code)
                    .textFieldStyle(.roundedBorder)
                    .keyboardType(.numberPad)
                    .textContentType(.oneTimeCode)
                    .disabled(busy)
                if let inlineError {
                    Text(inlineError.text)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
                Button(action: { confirm(otpauthUri: otpauthUri, secretB32: secretB32) }) {
                    Text(l10n.t(busy ? "common.confirming" : "common.confirm"))
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)

            case .done:
                Label(l10n.t("settings.recovery.totp.done"), systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)

            case .alreadyEnabled:
                Text(l10n.t("settings.recovery.totp.already_enabled"))
                    .font(.body)

            case .unavailable:
                Text(l10n.t("settings.recovery.totp.unavailable"))
                    .font(.body)
                    .foregroundStyle(.red)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Text(message.text)
                        .font(.footnote)
                        .foregroundStyle(.red)
                    Button(l10n.t("common.try_again"), action: { state = .idle })
                }
            }
        }
    }

    /// `Key: <secret>`, the secret in a code font and selectable on its own
    /// terms (no bidi isolates around it, so a copied key types in cleanly).
    private func keyLine(_ secret: String) -> some View {
        let parts = l10n.parts("settings.recovery.totp.key", around: "key")
        return (Text(parts.lead)
            + Text(secret).font(.footnote.monospaced().weight(.semibold))
            + Text(parts.trail))
            .font(.footnote)
            .textSelection(.enabled)
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
                NSLog("[recovery] WARN authenticator setup not started: \(error)")
                state = .error(.key("settings.recovery.totp.error.generic"))
            }
        }
    }

    private func confirm(otpauthUri: String, secretB32: String) {
        guard let normalized = normalizeTotpCode(code) else {
            inlineError = .key("recovery.totp.error.enter_code")
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
                if case .http(401, _, _) = err {
                    inlineError = .key("settings.recovery.totp.error.wrong")
                    // Keep the QR up; the user re-reads the code.
                    state = .confirm(
                        otpauthUri: otpauthUri,
                        secretB32: secretB32
                    )
                } else if case .http(409, _, _) = err {
                    state = .alreadyEnabled
                } else {
                    NSLog("[recovery] WARN authenticator not confirmed: \(err)")
                    state = .error(UiText {
                        AuthErrorCopy.shared(err) ?? Localization.text("settings.recovery.totp.error.generic")
                    })
                }
            } catch {
                busy = false
                NSLog("[recovery] WARN authenticator not confirmed: \(error)")
                state = .error(.key("settings.recovery.totp.error.generic"))
            }
        }
    }

    private func mapStartError(_ err: AuthClientError) -> UiState {
        if AuthRateLimit.copy(for: err) != nil {
            return .error(UiText { AuthRateLimit.copy(for: err) ?? AuthErrorCopy.unavailable })
        }
        if case .http(let status, _, _) = err {
            switch status {
            case 409: return .alreadyEnabled
            case 503: return .unavailable
            default: break
            }
        }
        NSLog("[recovery] WARN authenticator setup not started: \(err)")
        return .error(UiText {
            AuthErrorCopy.shared(err) ?? Localization.text("settings.recovery.totp.error.generic")
        })
    }
}

// ---- Remove all other devices (docs/ux/second-device.md section 3) ----

/// The confirmation before removing every other device: the app's own sheet
/// with the proof block. One proof covers all of them; the removals run on
/// the list, each row showing its own progress.
private struct RemoveAllDevicesSheet: View {
    let blocked: Bool
    let onConfirmed: () -> Void
    let onClose: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @State private var working = false
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository,
        blocked: Bool,
        onConfirmed: @escaping () -> Void,
        onClose: @escaping () -> Void
    ) {
        self.blocked = blocked
        self.onConfirmed = onConfirmed
        self.onClose = onClose
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .removeDevice, repository: authRepository))
    }

    private var busy: Bool { working || proof.layout.working }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(l10n.t("devices.remove_all.title"))
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .accessibilityAddTraits(.isHeader)
                Text(l10n.t("devices.remove_all.body"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                if blocked {
                    AuthHint(text: l10n.t(RemoveDeviceCopy.publishOwed), isError: true)
                } else {
                    FreshAuthBlock(model: proof, onSubmit: confirm)
                }
                let enabled = !busy && !blocked && proof.layout.ready
                Button(action: confirm) {
                    HStack(spacing: 8) {
                        if busy { ProgressView().controlSize(.small) }
                        Text(l10n.t("devices.remove_all.confirm"))
                            .font(.body.weight(.medium))
                    }
                    .foregroundStyle(Color.lakkoDanger)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .overlay(
                        RoundedRectangle(cornerRadius: LakkoRadius.control)
                            .stroke(Color.lakkoDanger, lineWidth: 1)
                    )
                }
                .buttonStyle(.plain)
                .disabled(!enabled)
                .opacity(enabled ? 1 : 0.5)
                AuthSecondaryButton(title: l10n.t("common.cancel"), disabled: busy) {
                    proof.closed()
                    onClose()
                }
            }
            .padding(24)
        }
        .background(Color.lakkoBackground)
        .environment(\.layoutDirection, l10n.layoutDirection)
        .interactiveDismissDisabled(busy)
        .task { await proof.opened() }
    }

    private func confirm() {
        guard !busy, !blocked, proof.layout.ready else { return }
        Task { @MainActor in
            working = true
            defer { working = false }
            // The proof. Nothing is removed before it passes; the removals
            // that follow all run under the grant it obtained.
            guard await proof.ensure() else { return }
            proof.closed()
            onConfirmed()
        }
    }
}
