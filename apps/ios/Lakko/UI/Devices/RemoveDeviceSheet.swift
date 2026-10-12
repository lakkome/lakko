// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI
import UIKit

// Settings > Devices > Remove (docs/ux/identity-recovery.md section 18.1,
// ADR-0066 section 2.3): the app's own sheet, never a system dialog. It
// carries the proof block; the main button runs the removal as ONE operation
// (``DeviceRemovalRunner``): sign the device out at the server, delete its
// directory row, then the DeviceGroup and the conversations.
//
//   * ``RemoveDeviceCopy``         the catalog keys, the missing ones named;
//   * ``RemoveDeviceSheetState``   what the sheet shows, decided purely;
//   * ``RemoveDeviceSheet``        the view.
//
// The sheet's second choice, "It was lost or stolen" (ADR-0066 section 3.2):
// remove the device AND change the account keys (``KeyRotationRunner``). The
// sheet changes in place: the explanation and its four results, then a step
// list while it runs, then the new recovery phrase.
//
//   * ``RemoveReason`` / ``RotateSheetRules``   the second choice, decided purely.

/// The catalog keys of the remove surfaces (UX 18.6).
enum RemoveDeviceCopy {
    /// "Remove {device}?"; keeps its `{device}` argument.
    static let title = "settings.devices.remove.title"
    static let body = "settings.devices.remove.body"
    /// "Remove device".
    static let confirm = "settings.devices.remove.confirm"
    /// The sheet's button while the removal runs.
    static let working = "settings.devices.remove.working"
    /// The row of a device signed out at the server whose removal is still
    /// finishing.
    static let pending = "devices.remove.pending"
    /// The row's button when the owed step waits for the person's passkey.
    static let finish = "devices.remove.finish"
    /// "Device removed and signed out."
    static let done = "devices.remove.done"
    static let notRevoked = "devices.remove.error.not_revoked"
    static let tryLater = "devices.remove.error.try_later"
    /// The owed copy of a key reset still being published (UX 15.4).
    static let publishOwed = "recovery_phrase.fresh.publish_owed"
    static let offline = "common.error.offline"
    static let rateLimited = "common.error.too_many_attempts"
    /// The ADR-0015 passkey gesture was dismissed: nothing was removed.
    static let passkeyCancelled = "proof.remove_device.cancelled"

    static let all: [String] = [
        title, body, confirm, working, pending, finish, done, notRevoked, tryLater, publishOwed,
        offline, rateLimited, passkeyCancelled,
    ]
}

/// Why the person removes the device (UX 18.1): a radio pair, neither
/// selected at first.
enum RemoveReason: Equatable {
    /// "I do not use it any more": today's plain removal.
    case unused
    /// "It was lost or stolen": remove it and change the account keys.
    case stolen
}

/// The catalog keys of the "remove and change keys" surfaces (UX 18.6).
enum RotateDeviceCopy {
    static let reasonUnused = "settings.devices.remove.reason_unused"
    static let reasonStolen = "settings.devices.remove.reason_stolen"
    static let title = "devices.rotate.title"
    static let lede = "devices.rotate.lede"
    static let effectDevices = "devices.rotate.effect_devices"
    static let effectPhrase = "devices.rotate.effect_phrase"
    static let effectPasskeys = "devices.rotate.effect_passkeys"
    static let effectContacts = "devices.rotate.effect_contacts"
    /// ADR-0066 section 3.5a rule 5 — the links and the unanswered requests
    /// made before the rotation stop working.
    static let effectLinks = "devices.rotate.effect_links"
    static let confirm = "devices.rotate.confirm"
    static let stepRemove = "devices.rotate.step_remove"
    static let stepKeys = "devices.rotate.step_keys"
    static let stepDevices = "devices.rotate.step_devices"
    static let done = "devices.rotate.done"
    static let notStarted = "devices.rotate.error.not_started"
    static let retry = "devices.rotate.retry"
    static let notNow = "devices.rotate.not_now"
    /// The new-phrase screen of section 9.1; its title is the reset's.
    static let phraseTitle = "recovery_phrase.fresh.title"
    /// The words are withheld while the publish is owed (UX 15.4's copy).
    static let publishOwed = "recovery_phrase.fresh.publish_owed"

    static let effects: [String] = [
        effectDevices, effectPhrase, effectPasskeys, effectContacts, effectLinks,
    ]
    static let steps: [(stage: KeyRotationPlan.Stage, key: String)] = [
        (.removingDevice, stepRemove), (.changingKeys, stepKeys), (.updatingDevices, stepDevices),
    ]

    static let all: [String] = [
        reasonUnused, reasonStolen, title, lede, effectDevices, effectPhrase, effectPasskeys,
        effectContacts, effectLinks, confirm, stepRemove, stepKeys, stepDevices, done, notStarted, retry, notNow,
        phraseTitle, publishOwed,
    ]
}

/// What the sheet shows for the second choice, pure.
enum RotateSheetState: Equatable {
    /// The explanation, the four results, the proof block.
    case idle
    /// The step list; Cancel is disabled from the first step.
    case working
    /// "The device was removed, but your account keys were not changed":
    /// `Try again` and `Not now`. `line` adds the cause when there is one to
    /// name (a limit, offline).
    case notStarted(line: String?)
    /// The keys were changed; publishing is still owed: the words are
    /// withheld, it retries by itself and `Try again` repeats only that.
    case publishOwed
    /// The new recovery phrase.
    case words(String)
}

enum RotateSheetRules {

    /// The main button's label.
    static func confirmKey(_ reason: RemoveReason?) -> String {
        reason == .stolen ? RotateDeviceCopy.confirm : RemoveDeviceCopy.confirm
    }

    /// The main button is disabled until a reason is chosen (UX 18.1).
    static func canConfirm(reason: RemoveReason?, busy: Bool, blocked: Bool, proofReady: Bool) -> Bool {
        reason != nil && !busy && !blocked && proofReady
    }

    /// The radio pair is locked once anything runs or the second choice has
    /// left its first state.
    static func reasonLocked(busy: Bool, rotate: RotateSheetState) -> Bool {
        busy || rotate != .idle
    }

    /// What the sheet shows after the rotation's interactive run, or `nil`
    /// when the plain removal's own state takes over (`notRemoved`) or the
    /// proof block comes back (`freshAuthRequired`).
    static func state(for result: KeyRotationPlan.Result) -> RotateSheetState? {
        switch result {
        case .rotated(let phrase): return .words(phrase)
        case .rotatedPublishOwed: return .publishOwed
        case .removedNotRotated(let outcome): return .notStarted(line: causeLine(outcome))
        case .notRemoved, .freshAuthRequired: return nil
        // ADR-0067: not phrase-protected, so no words and no withheld-words
        // state; the sheet closes as done.
        case .rotatedNoWords: return nil
        }
    }

    /// The cause named under "the keys were not changed", when there is one.
    static func causeLine(_ outcome: KeyRotationPlan.CommitOutcome?) -> String? {
        switch outcome {
        case .rateLimited: return RemoveDeviceCopy.rateLimited
        case .offline: return RemoveDeviceCopy.offline
        case .accountKeyRequired: return RemoveDeviceCopy.tryLater
        case .committed, .freshAuthRequired, .sessionEnded, .failed, .none: return nil
        }
    }

    /// The step list's rows: which is active, which are done.
    enum StepMark: Equatable {
        case done
        case active
        case waiting
    }

    static func mark(of step: KeyRotationPlan.Stage, active: KeyRotationPlan.Stage?) -> StepMark {
        let order: [KeyRotationPlan.Stage] = [.removingDevice, .changingKeys, .updatingDevices]
        guard let active, let activeIndex = order.firstIndex(of: active),
              let index = order.firstIndex(of: step) else { return .waiting }
        if index < activeIndex { return .done }
        return index == activeIndex ? .active : .waiting
    }

    /// The line under the device list once the sheet closed (UX 18.1,
    /// "Done").
    static let doneNote = DeviceRemovalNote(key: RotateDeviceCopy.done, isError: false)
}

/// The one line under the device list after a removal step, pure.
struct DeviceRemovalNote: Equatable {
    let key: String
    let isError: Bool

    /// After the sheet closed: the device is out of everything, or nothing
    /// to say (the row shows `Removing...` until the rest lands).
    static func afterRemove(_ result: DeviceRemovalPlan.Result) -> DeviceRemovalNote? {
        result == .removed ? DeviceRemovalNote(key: RemoveDeviceCopy.done, isError: false) : nil
    }

    /// After `Finish removing`.
    static func afterFinish(_ result: DeviceRemovalPlan.FinishResult) -> DeviceRemovalNote? {
        switch result {
        case .removed: return DeviceRemovalNote(key: RemoveDeviceCopy.done, isError: false)
        // A dismissed passkey sheet changes nothing: the row keeps its
        // button, and no line claims the device "was not removed" (it is
        // already signed out at the server).
        case .cancelled, .pending, .nothingOwed: return nil
        }
    }
}

/// What the sheet shows under the proof block, and whether its button works.
enum RemoveDeviceSheetState: Equatable {
    case idle
    case working
    /// A line under the block; the button stays (retry is the same button).
    case failed(key: String)

    /// The line for a removal that did not happen, or `nil` when the sheet
    /// has nothing to say (the proof block says it, or the sheet closes).
    static func line(for result: DeviceRemovalPlan.Result) -> String? {
        switch result {
        case .removed, .removedPending:
            return nil
        case .notStarted(.blockedPublishOwed):
            return RemoveDeviceCopy.publishOwed
        case .notStarted:
            return RemoveDeviceCopy.notRevoked
        case .notRevoked(let outcome):
            switch outcome {
            case .freshAuthRequired: return nil
            case .accountKeyRequired: return RemoveDeviceCopy.tryLater
            case .rateLimited: return RemoveDeviceCopy.rateLimited
            case .offline: return RemoveDeviceCopy.offline
            case .revoked, .sessionEnded, .failed: return RemoveDeviceCopy.notRevoked
            }
        }
    }

    /// Whether the sheet closes on `result`: the device is signed out at the
    /// server (whatever is still finishing), so the row takes over.
    static func closes(on result: DeviceRemovalPlan.Result) -> Bool {
        result == .removed || result == .removedPending
    }
}

/// The ADR-0015 passkey gesture bound to the removed device, when the account
/// has a passkey: the attestation the DeviceGroup Remove commit carries.
enum RemoveDeviceAttestation {
    enum Outcome: Equatable {
        /// The account has no passkey: the removal carries no attestation.
        case notNeeded
        case obtained(Data)
        /// The sheet was dismissed, or the ceremony failed: nothing is
        /// removed (fail closed).
        case cancelled
    }

    @MainActor
    static func obtain(sessionToken: String, deviceKey: Data) async -> Outcome {
        let client = WebauthnClient()
        client.setSessionToken(sessionToken)
        do {
            let result = try await client.assertAction(
                actionHashB64: actionHashForRemoveDevice(signaturePublicKey: deviceKey))
            guard let attestation = base64UrlDecodeNoPad(result.attestation_b64) else { return .cancelled }
            return .obtained(attestation)
        } catch let error as WebauthnClientError {
            // 404 = the account has no passkey (the server's NoCredentials
            // path).
            if case .http(404, _, _) = error { return .notNeeded }
            return .cancelled
        } catch {
            return .cancelled
        }
    }
}

struct RemoveDeviceSheet: View {
    let deviceTitle: String
    let deviceKey: Data
    let sessionToken: String
    @ObservedObject var runner: DeviceRemovalRunner
    /// ADR-0066 section 3.2 — the second choice. `nil` hides the reason pair
    /// (a caller that has no rotation to offer removes the device plainly).
    @ObservedObject var rotation: OptionalKeyRotationRunner
    /// The sheet opens straight on the second choice, for `Try again` after
    /// "the device was removed, but your account keys were not changed".
    let startsWithStolen: Bool
    /// The removal signed the device out (fully or with the rest pending).
    let onRemoved: (DeviceRemovalPlan.Result) -> Void
    /// The second choice finished: the keys were changed (`true`) or the
    /// sheet was left with the device removed and the keys unchanged.
    let onRotationClosed: (_ rotated: Bool) -> Void
    let onClose: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @State private var state: RemoveDeviceSheetState = .idle
    @State private var reason: RemoveReason?
    @State private var rotate: RotateSheetState = .idle
    @ObservedObject private var l10n = Localization.shared

    init(
        deviceTitle: String,
        deviceKey: Data,
        sessionToken: String,
        authRepository: AuthRepository,
        runner: DeviceRemovalRunner,
        rotation: KeyRotationRunner? = nil,
        startsWithStolen: Bool = false,
        onRemoved: @escaping (DeviceRemovalPlan.Result) -> Void,
        onRotationClosed: @escaping (_ rotated: Bool) -> Void = { _ in },
        onClose: @escaping () -> Void
    ) {
        self.deviceTitle = deviceTitle
        self.deviceKey = deviceKey
        self.sessionToken = sessionToken
        self.runner = runner
        self._rotation = ObservedObject(wrappedValue: OptionalKeyRotationRunner(rotation))
        self.startsWithStolen = startsWithStolen
        self.onRemoved = onRemoved
        self.onRotationClosed = onRotationClosed
        self.onClose = onClose
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .removeDevice, repository: authRepository))
        self._reason = State(initialValue: rotation == nil ? .unused : (startsWithStolen ? .stolen : nil))
    }

    private var busy: Bool { state == .working || rotate == .working || proof.layout.working }

    /// A key reset is still being published: no removal runs in that window.
    private var blocked: Bool { runner.canStart(deviceId: deviceKey) == .blockedPublishOwed }

    private var stolen: Bool { reason == .stolen }

    var body: some View {
        if case .words(let phrase) = rotate {
            // UX 18.1, "Done": the new recovery phrase screen of section 9.1,
            // then back to Devices.
            RecoveryPhraseGuide(
                phrase: phrase,
                title: l10n.t(RotateDeviceCopy.phraseTitle),
                intro: l10n.t(RotateDeviceCopy.effectPhrase),
                extra: { EmptyView() },
                onDone: {
                    rotation.runner?.acknowledgePhrase()
                    proof.closed()
                    onRotationClosed(true)
                }
            )
            .environment(\.layoutDirection, l10n.layoutDirection)
            .interactiveDismissDisabled(true)
        } else {
            form
        }
    }

    private var form: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(stolen
                     ? l10n.t(RotateDeviceCopy.title)
                     : l10n.t(RemoveDeviceCopy.title, texts: ["device": deviceTitle]))
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .accessibilityAddTraits(.isHeader)
                Text(l10n.t(stolen ? RotateDeviceCopy.lede : RemoveDeviceCopy.body))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)

                if rotation.runner != nil {
                    reasonPair
                }
                if stolen {
                    rotateBody
                } else {
                    removeBody
                }
            }
            .padding(24)
        }
        .background(Color.lakkoBackground)
        .environment(\.layoutDirection, l10n.layoutDirection)
        .interactiveDismissDisabled(busy || rotate != .idle)
        .task { await proof.opened() }
    }

    // MARK: The reason pair

    /// UX 18.1 — a radio pair, one group with a label, neither selected at
    /// first.
    private var reasonPair: some View {
        let locked = RotateSheetRules.reasonLocked(busy: busy, rotate: rotate)
        return VStack(alignment: .leading, spacing: 0) {
            reasonRow(.unused, key: RotateDeviceCopy.reasonUnused)
            reasonRow(.stolen, key: RotateDeviceCopy.reasonStolen)
        }
        .disabled(locked)
        .opacity(locked ? 0.6 : 1)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t(RemoveDeviceCopy.title, texts: ["device": deviceTitle]))
    }

    private func reasonRow(_ value: RemoveReason, key: String) -> some View {
        Button {
            reason = value
            state = .idle
        } label: {
            HStack(spacing: 10) {
                Image(systemName: reason == value ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(reason == value ? Color.lakkoAccent : Color.lakkoTextMuted)
                    .accessibilityHidden(true)
                Text(l10n.t(key))
                    .font(.body)
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(reason == value ? [.isSelected] : [])
    }

    // MARK: "I do not use it any more"

    @ViewBuilder private var removeBody: some View {
        if blocked {
            AuthHint(text: l10n.t(RemoveDeviceCopy.publishOwed), isError: true)
        } else {
            FreshAuthBlock(model: proof, onSubmit: confirm)
        }
        if case .failed(let key) = state {
            AuthHint(text: l10n.t(key), isError: true)
        }
        confirmButton
        AuthSecondaryButton(title: l10n.t("common.cancel"), disabled: busy) {
            proof.closed()
            onClose()
        }
    }

    // MARK: "It was lost or stolen"

    @ViewBuilder private var rotateBody: some View {
        switch rotate {
        case .idle:
            VStack(alignment: .leading, spacing: 8) {
                ForEach(RotateDeviceCopy.effects, id: \.self) { key in
                    HStack(alignment: .top, spacing: 8) {
                        Text("-").foregroundStyle(Color.lakkoTextMuted).accessibilityHidden(true)
                        Text(l10n.t(key))
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoText)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            if blocked {
                AuthHint(text: l10n.t(RemoveDeviceCopy.publishOwed), isError: true)
            } else {
                FreshAuthBlock(model: proof, onSubmit: confirm)
            }
            if case .failed(let key) = state {
                AuthHint(text: l10n.t(key), isError: true)
            }
            confirmButton
            AuthSecondaryButton(title: l10n.t("common.cancel"), disabled: busy) {
                proof.closed()
                onClose()
            }
        case .working:
            stepList
            // Cancel is disabled from the first step.
            AuthSecondaryButton(title: l10n.t("common.cancel"), disabled: true) {}
        case .notStarted(let line):
            AuthHint(text: l10n.t(RotateDeviceCopy.notStarted), isError: true)
            if let line {
                AuthHint(text: l10n.t(line), isError: true)
            }
            FreshAuthBlock(model: proof, onSubmit: confirm)
            Button(action: confirm) {
                Text(l10n.t(RotateDeviceCopy.retry))
                    .font(.body.weight(.medium))
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(busy || !proof.layout.ready)
            AuthSecondaryButton(title: l10n.t(RotateDeviceCopy.notNow), disabled: busy) {
                rotation.runner?.abandonUnstarted()
                proof.closed()
                onRotationClosed(false)
            }
        case .publishOwed:
            AuthHint(text: l10n.t(RotateDeviceCopy.publishOwed), isError: true)
            Button(action: retryPublish) {
                Text(l10n.t(RotateDeviceCopy.retry))
                    .font(.body.weight(.medium))
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(busy)
            AuthSecondaryButton(title: l10n.t("common.close"), disabled: busy) {
                proof.closed()
                onRotationClosed(true)
            }
        case .words:
            EmptyView()
        }
    }

    /// UX 18.1 — a step list replaces the bullets, one line active at a
    /// time. No percentage. Each step is announced once when it becomes
    /// active.
    private var stepList: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(RotateDeviceCopy.steps, id: \.key) { step in
                let mark = RotateSheetRules.mark(of: step.stage, active: rotation.runner?.stage)
                HStack(spacing: 10) {
                    switch mark {
                    case .done:
                        Image(systemName: "checkmark").foregroundStyle(Color.lakkoAccent)
                    case .active:
                        ProgressView().controlSize(.small)
                    case .waiting:
                        Image(systemName: "circle").foregroundStyle(Color.lakkoTextMuted)
                    }
                    Text(l10n.t(step.key))
                        .font(.subheadline.weight(mark == .active ? .semibold : .regular))
                        .foregroundStyle(mark == .waiting ? Color.lakkoTextMuted : Color.lakkoText)
                }
                .accessibilityElement(children: .combine)
            }
        }
        .onChange(of: rotation.runner?.stage) { _, stage in
            guard let stage, let key = RotateDeviceCopy.steps.first(where: { $0.stage == stage })?.key
            else { return }
            UIAccessibility.post(notification: .announcement, argument: l10n.t(key))
        }
    }

    private var confirmButton: some View {
        let enabled = RotateSheetRules.canConfirm(
            reason: reason, busy: busy, blocked: blocked, proofReady: proof.layout.ready)
        return Button(action: confirm) {
            HStack(spacing: 8) {
                if busy { ProgressView().controlSize(.small) }
                Text(busy && !stolen
                     ? l10n.t(RemoveDeviceCopy.working)
                     : l10n.t(RotateSheetRules.confirmKey(reason)))
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
    }

    private func confirm() {
        switch reason {
        case .unused: remove()
        case .stolen: rotateKeys()
        case .none: break
        }
    }

    private func remove() {
        guard !busy, !blocked, proof.layout.ready else { return }
        Task { @MainActor in
            // 1. The proof. Nothing of the removal starts before it passes.
            guard await proof.ensure() else { return }
            state = .working
            // The ADR-0015 gesture bound to this device, when the account has
            // a passkey. A dismissed sheet removes nothing.
            let attestation: Data?
            switch await RemoveDeviceAttestation.obtain(sessionToken: sessionToken, deviceKey: deviceKey) {
            case .notNeeded: attestation = nil
            case .obtained(let bytes): attestation = bytes
            case .cancelled:
                state = .failed(key: RemoveDeviceCopy.passkeyCancelled)
                return
            }
            // 2 to 5. Sign out at the server, then everything else.
            let result = await runner.remove(deviceId: deviceKey, attestation: attestation)
            if RemoveDeviceSheetState.closes(on: result) {
                proof.closed()
                onRemoved(result)
                return
            }
            if result == .notRevoked(.freshAuthRequired) {
                // The proof ran out on the way: the block comes back, with
                // no error styling.
                proof.proofExpired()
                state = .idle
                return
            }
            state = RemoveDeviceSheetState.line(for: result).map { .failed(key: $0) } ?? .idle
        }
    }

    /// "Remove and change keys": the proof, the ADR-0015 gesture, then the
    /// one owed operation (``KeyRotationRunner``).
    private func rotateKeys() {
        guard !busy, !blocked, proof.layout.ready, let rotationRunner = rotation.runner else { return }
        Task { @MainActor in
            guard await proof.ensure() else { return }
            let before = rotate
            rotate = .working
            let attestation: Data?
            switch await RemoveDeviceAttestation.obtain(sessionToken: sessionToken, deviceKey: deviceKey) {
            case .notNeeded: attestation = nil
            case .obtained(let bytes): attestation = bytes
            case .cancelled:
                rotate = before
                state = .failed(key: RemoveDeviceCopy.passkeyCancelled)
                return
            }
            state = .idle
            let result = await rotationRunner.rotate(deviceId: deviceKey, attestation: attestation)
            if let next = RotateSheetRules.state(for: result) {
                rotate = next
                return
            }
            rotate = .idle
            switch result {
            case .rotatedNoWords:
                proof.closed()
                onRotationClosed(true)
            case .freshAuthRequired:
                proof.proofExpired()
            case .notRemoved(let removal):
                if removal == .notRevoked(.freshAuthRequired) {
                    proof.proofExpired()
                } else {
                    state = RemoveDeviceSheetState.line(for: removal).map { .failed(key: $0) } ?? .idle
                }
            case .rotated, .rotatedPublishOwed, .removedNotRotated:
                break
            }
        }
    }

    /// `Try again` while the publish is owed: only what is left, never a
    /// second rotation.
    private func retryPublish() {
        guard !busy, let rotationRunner = rotation.runner else { return }
        Task { @MainActor in
            rotate = .working
            await rotationRunner.resumeOwed()
            if let phrase = rotationRunner.wordsToShow() {
                rotate = .words(phrase)
            } else if rotationRunner.resume == .nothing {
                // Nothing is owed and no words are shown (ADR-0067).
                rotate = .idle
                onRotationClosed(true)
            } else {
                rotate = .publishOwed
            }
        }
    }
}

/// An optional ``KeyRotationRunner`` a view can observe.
final class OptionalKeyRotationRunner: ObservableObject {
    let runner: KeyRotationRunner?
    private var forward: AnyCancellable?

    init(_ runner: KeyRotationRunner?) {
        self.runner = runner
        forward = runner?.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }
}
