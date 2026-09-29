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
    @ObservedObject private var l10n = Localization.shared

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
        }
        .onDisappear { cancel() }
    }

    private func start() {
        ui = .preparing
        do {
            let s: PairingOrchestrator.JoinSession
            switch target {
            case .account(let accountIdBytes, let accountIdB64, let loginHandle):
                s = try PairingOrchestrator.prepareJoinFlow(
                    loginHandle: loginHandle,
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64
                )
            case .byName(let loginHandle):
                s = try PairingOrchestrator.prepareRestoreByNameFlow(
                    loginHandle: loginHandle
                )
            }
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
        } catch {
            NSLog("[pairing] WARN pairing code not prepared: \(error)")
            ui = .error(.key("device_link.error.generic"))
        }
    }

    private func cancel() {
        pollTask?.cancel()
        pollTask = nil
        session = nil
    }
}
