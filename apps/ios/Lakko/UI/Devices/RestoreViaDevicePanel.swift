// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

/// Shared "restore from your other device" panel (ADR-0018 factor (a))
/// — the QR-join flow with the recovery payload opted in. Used by both
/// completion paths:
///
///   * path (a): ``RestoreIdentityPromptScreen`` — the user
///     password-authenticated on this fresh device but has no cached
///     identity seed; `onRecovered` drives
///     `AuthRepository.completeFromPairing`.
///   * path (b): the Recover screen's no-password mode; `onRecovered`
///     drives `AuthRepository.completeFromDeviceRestore`.
///
/// Mirror of `apps/web/src/devices/RestoreViaDevicePanel.tsx` + the
/// Android `RestoreViaDevicePanel.kt`. The panel renders the pairing
/// QR; the user scans it with their already-linked device, which adds
/// this device to the DeviceGroup AND (best-effort) publishes the
/// sealed `(grant ‖ identity seed)` recovery row. A scanner that did
/// not publish the row (older build, or its best-effort half failed)
/// completes the pairing but yields no recovery payload — surfaced as
/// a retryable error suggesting the phrase instead.
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
        case error(String)
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
            Text("Confirm on another device")
                .font(.headline)
            Text(
                "Open Lakko in a browser or on your phone, on a device that is " +
                "already signed in. It will show a six-digit code. Enter it here."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            if let link {
                DeviceLinkCodeEntry(
                    link: link, typedCode: $typedCode,
                    onContinue: submitCode, onStartAgain: startCode)
            } else if case .error(let message) = ui {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.footnote)
            } else {
                ProgressView()
            }
            if case .joining = ui {
                ProgressView(DeviceLinkJoinCopy.restoring)
            }
            if case .done = ui {
                Label("Identity restored.", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.footnote)
            }
            HStack {
                Spacer()
                Button("Show a QR code instead") {
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
            ui = .error((error as? LocalizedError)?.errorDescription ?? "Could not prepare the request")
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
            Text(
                "Show this QR code to a device that still has your " +
                "account. It links this device and securely transfers " +
                "your identity — no phrase entry needed."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)

            switch ui {
            case .idle:
                Button(action: start) {
                    Text("Show QR code").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .preparing:
                ProgressView("Preparing pairing material…")

            case .waiting(let payload):
                VStack {
                    QrImageView(payload: payload)
                        .padding(.vertical, 8)
                    Text("Waiting for your other device to scan…")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)

            case .joining:
                ProgressView("Linking and restoring your identity…")

            case .done:
                Label("Identity restored.", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.footnote)

            case .error(let message):
                VStack(alignment: .leading, spacing: 8) {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                        .font(.footnote)
                    Button("Try again", action: { ui = .idle })
                }
            }
            Button("Confirm on another device instead") {
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
                accountIdB64: accountIdB64
            )
        case .byName(let loginHandle):
            return try PairingOrchestrator.prepareRestoreByNameFlow(loginHandle: loginHandle)
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
                    ui = .error(
                        "The other device linked this one but " +
                        "didn't send recovery data. Update it, " +
                        "or restore with your recovery phrase " +
                        "instead."
                    )
                    return
                }
                try await onRecovered(recovery)
                ui = .done
            case .timeout:
                ui = .error("Timed out waiting for the other device. Try again.")
            case .cancelled:
                ui = .idle
            }
        } catch {
            if Task.isCancelled { return }
            ui = .error((error as? LocalizedError)?.errorDescription ?? error.localizedDescription)
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
            ui = .error(
                (error as? LocalizedError)?.errorDescription
                    ?? "Could not prepare the QR code"
            )
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
