// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Shared "restore from your other device" panel: the device link (or the
/// QR join) with the recovery payload opted in. Used by both completion
/// paths:
///
///   * ``RestoreIdentityPromptScreen`` — the person is signed in on this
///     device, which does not hold the account's keys; `onRecovered`
///     drives `AuthRepository.completeFromPairing`.
///   * the "Get back in" list's another-device form (no session yet);
///     `onRecovered` drives `AuthRepository.completeFromDeviceRestore`.
///
/// Mirror of `apps/web/src/devices/RestoreViaDevicePanel.tsx` + the
/// Android `RestoreViaDevicePanel.kt`. The other device adds this one
/// to the DeviceGroup AND (best-effort) publishes the sealed
/// `(grant ‖ phrase entropy)` recovery row (payload v2). A device that
/// did not publish the row (it holds no recovery phrase, or its
/// best-effort half failed) completes the pairing but yields no
/// recovery payload — surfaced as a retryable error.
///
/// `onRecovered` may throw (`PhraseMismatch` /
/// `IdentityVerificationUnavailable` from the repository's fail-closed
/// verify) — surfaced inline.
public struct RestoreViaDevicePanel: View {

    /// Which account the QR names (ADR-0056): always by login handle; the
    /// account id only when this device already knows it.
    public enum Target {
        /// Path (a): signed in by password, parked on the identity prompt.
        case account(accountIdBytes: Data, accountIdB64: String, loginHandle: Data)
        /// Path (b): a fresh device with no session; the account id comes
        /// from the grant redeem.
        case byName(loginHandle: Data)
    }

    public let target: Target
    public let onRecovered: (PairingOrchestrator.JoinRecoveryOutcome) async throws -> Void

    @State private var ui: PanelState = .idle
    @State private var session: PairingOrchestrator.JoinSession?
    @State private var pollTask: Task<Void, Never>?
    @ObservedObject private var l10n = Localization.shared
    /// ADR-0049 section 2 (amended 2026-09-28) — the camera-less route is the
    /// default; the QR is one tap away.
    @State private var mode: Mode = .code
    @State private var link: DeviceLinkJoin?
    @State private var typedCode = ""
    @State private var linkTask: Task<Void, Never>?

    private enum Mode {
        case code
        case qr
    }

    private enum PanelState: Equatable {
        case idle
        case preparing
        case waiting(qrPayload: Data)
        case joining
        case done
        case error(UiText)
    }

    public init(
        target: Target,
        onRecovered: @escaping (PairingOrchestrator.JoinRecoveryOutcome) async throws -> Void
    ) {
        self.target = target
        self.onRecovered = onRecovered
    }

    public var body: some View {
        Group {
            switch mode {
            case .code: codeBody
            case .qr: qrBody
            }
        }
        .onDisappear { cancel() }
    }

    // MARK: - Confirm on another device (code)

    private var loginHandle: Data {
        switch target {
        case .account(_, _, let handle), .byName(let handle): return handle
        }
    }

    @ViewBuilder
    private var codeBody: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("device_link.code.title"))
                .font(.headline)
            Text(l10n.t("device_link.code.intro_native"))
            .font(.footnote)
            .foregroundStyle(.secondary)

            if let link {
                DeviceLinkCodeEntry(
                    link: link, typedCode: $typedCode,
                    onContinue: submitCode, onStartAgain: startCode)
            } else if case .error(let message) = ui {
                Label(message.text, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.footnote)
            } else {
                ProgressView()
            }
            if case .joining = ui {
                ProgressView(DeviceLinkJoinCopy.restoring)
            }
            if case .done = ui {
                Label(l10n.t("device_link.restored"), systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.footnote)
            }
            HStack {
                Spacer()
                Button(l10n.t("device_link.qr.show")) {
                    cancel()
                    mode = .qr
                    ui = .idle
                }
            }
            .font(.footnote)
        }
        .task { if link == nil { startCode() } }
    }

    /// Prepare the pairing material (the payload the QR would carry) and post
    /// a request naming the account by its login handle.
    private func startCode() {
        cancel()
        typedCode = ""
        ui = .preparing
        do {
            let s = try prepareSession()
            session = s
            let flow = DeviceLinkJoin(
                ports: .init(relay: DeviceLinkClient()),
                handle: loginHandle,
                pairingPayload: s.qrPayload)
            link = flow
            ui = .idle
            linkTask = Task { @MainActor in await flow.start() }
        } catch {
            NSLog("[pairing] WARN device-link request not prepared: \(error)")
            ui = .error(.key("device_link.error.generic"))
        }
    }

    /// Continue: run the exchange; only a VERIFIED confirmation from the other
    /// device lets this device wait for, and trust, the Welcome.
    private func submitCode() {
        guard let link, let s = session else { return }
        let typed = typedCode
        linkTask = Task { @MainActor in
            guard await link.submit(code: typed) else { return }
            await receiveWelcome(session: s)
        }
    }

    // MARK: - QR

    private var qrBody: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("device_link.qr.scan_hint"))
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch ui {
            case .idle:
                Button(action: start) {
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
                }
                .frame(maxWidth: .infinity)

            case .joining:
                ProgressView(l10n.t("device_link.transferring"))

            case .done:
                Label(l10n.t("device_link.restored"), systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message.text, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button(l10n.t("common.try_again"), action: { ui = .idle })
                }
            }
            Button(l10n.t("device_link.code.use_instead")) {
                cancel()
                ui = .idle
                link = nil
                mode = .code
            }
            .font(.footnote)
        }
    }

    private func prepareSession() throws -> PairingOrchestrator.JoinSession {
        switch target {
        case .account(let accountIdBytes, let accountIdB64, let loginHandle):
            return try PairingOrchestrator.prepareJoinFlow(
                loginHandle: loginHandle,
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64,
                // UX section 2.1 — the other device names this one by it.
                deviceCardEnvelope: DeviceCardCenter.shared.pairingCardEnvelope()
            )
        case .byName(let loginHandle):
            return try PairingOrchestrator.prepareRestoreByNameFlow(
                loginHandle: loginHandle,
                deviceCardEnvelope: DeviceCardCenter.shared.pairingCardEnvelope())
        }
    }

    /// Wait for the Welcome on the session's rendezvous and finish the join
    /// with the recovery payload. Shared by both routes.
    private func receiveWelcome(session s: PairingOrchestrator.JoinSession) async {
        do {
            let outcome = try await s.awaitWelcome()
            if Task.isCancelled { return }
            switch outcome {
            case .received(let welcome, let welcomeCursor):
                ui = .joining
                let recovery = try await s.accept(
                    welcomeBytes: welcome,
                    welcomeCursor: welcomeCursor,
                    recovery: true
                )
                guard let recovery else {
                    ui = .error(.key("device_link.error.no_identity"))
                    return
                }
                try await onRecovered(recovery)
                ui = .done
            case .timeout:
                ui = .error(.key("device_link.error.timeout"))
            case .cancelled:
                ui = .idle
            }
        } catch {
            if Task.isCancelled { return }
            NSLog("[pairing] WARN restore from another device failed: \(error)")
            ui = .error(UiText { AuthErrorCopy.deviceRestore(error) })
        }
    }

    private func start() {
        ui = .preparing
        do {
            let s = try prepareSession()
            session = s
            ui = .waiting(qrPayload: s.qrPayload)
            pollTask = Task { @MainActor in
                defer { session = nil }
                await receiveWelcome(session: s)
            }
        } catch {
            NSLog("[pairing] WARN pairing code not prepared: \(error)")
            ui = .error(.key("device_link.error.generic"))
        }
    }

    private func cancel() {
        pollTask?.cancel()
        pollTask = nil
        linkTask?.cancel()
        linkTask = nil
        session = nil
    }
}
