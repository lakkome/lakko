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

    public let accountIdBytes: Data
    public let accountIdB64: String
    public let onRecovered: (PairingOrchestrator.JoinRecoveryOutcome) async throws -> Void

    @State private var ui: PanelState = .idle
    @State private var session: PairingOrchestrator.JoinSession?
    @State private var pollTask: Task<Void, Never>?

    private enum PanelState: Equatable {
        case idle
        case preparing
        case waiting(qrPayload: Data)
        case joining
        case done
        case error(String)
    }

    public init(
        accountIdBytes: Data,
        accountIdB64: String,
        onRecovered: @escaping (PairingOrchestrator.JoinRecoveryOutcome) async throws -> Void
    ) {
        self.accountIdBytes = accountIdBytes
        self.accountIdB64 = accountIdB64
        self.onRecovered = onRecovered
    }

    public var body: some View {
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
        }
        .onDisappear { cancel() }
    }

    private func start() {
        ui = .preparing
        do {
            let s = try PairingOrchestrator.prepareJoinFlow(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64
            )
            session = s
            ui = .waiting(qrPayload: s.qrPayload)
            pollTask = Task { @MainActor in
                defer { session = nil }
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
                        ui = .error(
                            "Timed out waiting for the other device. " +
                            "Try again."
                        )
                    case .cancelled:
                        ui = .idle
                    }
                } catch {
                    if Task.isCancelled { return }
                    ui = .error(
                        (error as? LocalizedError)?.errorDescription
                            ?? error.localizedDescription
                    )
                }
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
        session = nil
    }
}
