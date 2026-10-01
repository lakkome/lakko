// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// State + dispatcher for ``RecoverScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Recover.tsx`'s local signals (1.17d8)
///   * `apps/android/.../ui/auth/RecoverViewModel.kt` (1.15d6)
///
/// Single-stage form (mnemonic + new password + confirm). On submit
/// drives ``AuthRepository/recover(mnemonicPhrase:newPassword:)``,
/// which rotates the password envelope on the server, re-derives
/// the device root key from the new OPAQUE export key, and flips
/// the auth state.
///
/// The ``cachedUsername`` constructor arg drives the "Account on
/// this device: @alice" line in the UI. When it's `nil` the screen
/// renders a "no cached account" stub instead (the recovery flow
/// can't run without an `accountId` on disk; that's a 1.14e /
/// discovery-service problem).
@MainActor
public final class RecoverViewModel: ObservableObject {

    // MARK: - Form fields

    /// 1.21c2 — which input mode the user is filling. `phrase`
    /// reuses the original 24-word path; `shares` parses one
    /// Shamir share per line and combines them into the same
    /// canonical phrase via the BIP39 `from_entropy` round-trip.
    /// ADR-0018 adds `anotherDevice`: restore via an already-linked
    /// device (no password, no phrase typing) — and `totp`
    /// (factor (c)): username + the current 6-digit code from a
    /// previously-enrolled authenticator app. TOTP grants ACCESS
    /// only; identity restore then runs the ADR-0017 state machine
    /// (warm cached seed, or the `needsRecoveryPhrase` fail-safe).
    /// ADR-0018 factor (d) adds `contact`: recover ACCESS with a
    /// one-time code sent to a previously-enrolled phone or email. Like
    /// `totp`, it grants ACCESS only; identity restore then runs the
    /// ADR-0017 state machine.
    public enum Mode: Hashable { case phrase, shares, anotherDevice, totp, contact }

    @Published public var mode: Mode = .phrase {
        didSet {
            // ADR-0018 — clear the resolved restore target + any
            // stale error when leaving the AnotherDevice mode
            // (mirror of Android's `onModeChange`).
            if mode != .anotherDevice {
                deviceRestoreTarget = nil
            }
            // ADR-0018 factor (d) — reset the contact flow to its first
            // step when leaving the Email/SMS mode so a re-entry starts
            // clean (mirror of the AnotherDevice reset above).
            if mode != .contact {
                contactStep = .request
            }
            errorText = nil
        }
    }

    /// ADR-0018 / ADR-0056 — non-nil once the AnotherDevice mode turned the
    /// typed username into its login handle (no lookup).
    @Published public private(set) var deviceRestoreTarget: DeviceRestoreTarget?
    @Published public var phrase: String = ""
    @Published public var sharesInput: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""
    /// ADR-0017 — username for the fresh-device recover path. Every
    /// recovery factor names the account by the login handle of this name
    /// (ADR-0054 S5; no discovery lookup before a session), so on a device
    /// with no cached account the user must type it. Pre-filled with
    /// ``cachedUsername`` when present.
    @Published public var username: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }
    /// ADR-0018 factor (c) — the 6-digit authenticator code field.
    @Published public var totpCode: String = ""

    // MARK: - ADR-0018 factor (d) — phone / email OTP form state

    /// Two-step contact-recovery flow: collect the username + contact
    /// and send a code (`request`), then collect the 6-digit code and
    /// finish (`code`).
    public enum ContactStep: Equatable { case request, code }

    /// Which step of the phone / email OTP flow the user is on.
    @Published public private(set) var contactStep: ContactStep = .request
    /// Whether the contact is an email or a phone number.
    @Published public var contactKind: ContactKind = .email
    /// The raw phone / email the user typed.
    @Published public var contact: String = ""
    /// The 6-digit code field for the contact flow's `code` step.
    @Published public var contactCode: String = ""

    // MARK: - Read-only inputs

    /// Cached username for the on-device account, surfaced in the
    /// "Account on this device: @alice" line. When `nil` the screen
    /// renders a "no cached account" stub.
    public let cachedUsername: String?

    // MARK: - Wiring

    private let repository: AuthRepository
    private let l10n = Localization.shared

    public init(repository: AuthRepository, cachedUsername: String?) {
        self.repository = repository
        self.cachedUsername = cachedUsername
        // Pre-fill the username field with the cached handle when one
        // exists; the fresh-device path leaves it empty for the user.
        self.username = cachedUsername ?? ""
    }

    // MARK: - Submit

    /// Submit the recover form.
    ///
    /// Validates locally (new password ≥ 8, confirm matches, phrase
    /// non-empty), drives
    /// ``AuthRepository/recover(mnemonicPhrase:newPassword:)``,
    /// surfaces success as an ``AuthState/session(_:)`` transition
    /// (which the app-level router catches via `@Published`
    /// observation), and surfaces failures as inline form messages.
    ///
    /// The mnemonic-parse failure (bad checksum / unknown word)
    /// collapses to one user-facing message — same posture as the
    /// web + Android sides, preserves BIP39's no-probing property.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap

        if password != confirm {
            errorText = .key("auth.password.error.mismatch")
            return
        }
        if password.count < Self.minPasswordLength {
            errorText = .key("auth.password.error.too_short", counts: ["count": UInt64(Self.minPasswordLength)])
            return
        }
        // ADR-0017 / ADR-0054 S5 — on a fresh device (no cached account)
        // the username is required: the account is named by its handle.
        let trimmedUsername = username.trimmingCharacters(in: .whitespacesAndNewlines)
        if cachedUsername == nil && trimmedUsername.isEmpty {
            errorText = .key("recovery.username_required")
            return
        }

        // 1.21c2 — resolve the phrase based on input mode.
        // Shares mode reconstructs the canonical phrase via
        // `ShamirBridge.combineSharesToMnemonic`; the recovered
        // phrase then drives the existing OPAQUE recovery flow.
        let phraseSnapshot: String
        switch mode {
        case .anotherDevice:
            // ADR-0018 — the AnotherDevice mode never reaches the
            // OPAQUE submit: the RestoreViaDevicePanel drives
            // completeFromDeviceRestore directly. Defensive no-op.
            errorText = .key("recovery.error.generic")
            return
        case .totp:
            // ADR-0018 factor (c) — the Authenticator mode submits
            // via `submitTotp` (no new password on that path).
            // Defensive no-op.
            errorText = .key("recovery.totp.error.enter_code")
            return
        case .contact:
            // ADR-0018 factor (d) — the Email/SMS mode submits via
            // `sendContactCode` / `submitContactCode` (no new password
            // on that path). Defensive no-op.
            errorText = .key("recovery.error.generic")
            return
        case .phrase:
            if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                errorText = .key("recovery_phrase.error.incomplete")
                return
            }
            phraseSnapshot = phrase
        case .shares:
            let lines = sharesInput
                .split(whereSeparator: \.isNewline)
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }
            if lines.count < 2 {
                errorText = .key("recovery.shares.error.too_few")
                return
            }
            do {
                phraseSnapshot = try ShamirBridge.combineSharesToMnemonic(
                    encodedShares: lines
                )
            } catch {
                NSLog("[recover] WARN shares did not combine: \(error)")
                self.errorText = .key("recovery.shares.error.combine_failed")
                return
            }
        }

        busy = true
        errorText = nil

        let passwordBytes = Data(password.utf8)

        let usernameSnapshot = trimmedUsername.isEmpty ? nil : trimmedUsername

        Task {
            do {
                try await repository.recover(
                    mnemonicPhrase: phraseSnapshot,
                    newPassword: passwordBytes,
                    username: usernameSnapshot
                )
                // `repository.recover` flipped AuthState to .session.
                // The screen will be replaced by the home destination
                // on the next render; wipe textfield buffers so they
                // don't hold the phrase / password past the
                // navigation.
                busy = false
                errorText = nil
                phrase = ""
                sharesInput = ""
                password = ""
                confirm = ""
            } catch {
                // A bad phrase (unknown word or checksum) reads as one
                // sentence; a wrong phrase and an unknown account too.
                busy = false
                self.errorText = UiText { AuthErrorCopy.recover(error) }
            }
        }
    }

    // MARK: - ADR-0018 factor (a), path (b) — another-device restore

    /// Turn the typed username into the login handle the pairing code names
    /// the account by (ADR-0056), so the AnotherDevice restore can show its
    /// QR. No network: nothing before a session looks the account up, and
    /// the account id is learned from the grant redeem after the other
    /// device confirms. A name that breaks the username rule gets its
    /// reason copy.
    public func prepareDeviceRestore() {
        guard !busy else { return }
        do {
            let name = try LoginName.make(
                username,
                emptyError: AuthClientError.argument("empty username")
            )
            deviceRestoreTarget = DeviceRestoreTarget(
                username: name.canonical,
                loginHandle: name.handle
            )
            errorText = nil
        } catch {
            self.errorText = UiText { AuthErrorCopy.rule(error) ?? Localization.text("recovery.username_required") }
        }
    }

    /// Complete the no-password restore with the identity + redeemed
    /// session token received from the other device. The account id is the
    /// one the grant redeem returned (ADR-0056). Async passthrough to
    /// ``AuthRepository/completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseSeed:sessionToken:sessionExpiresAtMs:persistJoin:)``;
    /// verification failures propagate to the calling panel. Requires
    /// the redeemed token and account id (without them this device has no
    /// ACCESS) — refused otherwise with a clear message.
    public func completeFromDeviceRestore(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        guard let target = deviceRestoreTarget else {
            outcome.wipeSeed()
            throw AuthClientError.argument(
                "no username — enter your username first"
            )
        }
        guard let token = outcome.sessionToken,
              let expiry = outcome.sessionExpiresAtMs,
              let accountIdBytes = outcome.accountId else {
            // Seed-only outcome: the grant redeem failed, so this
            // device has IDENTITY but no ACCESS. Refuse rather than
            // leave a half-restored profile; the user retries (a
            // fresh pairing mints a fresh grant). The seed is zeroed
            // where it lives, in the outcome.
            outcome.wipeSeed()
            throw DeviceRestoreNotLetIn()
        }
        // ADR-0056 security pass: the repository verifies the seed, checks
        // the typed name at discovery, and only then runs the pairing's
        // held writes (`persistJoin`) and its own. On any refusal nothing
        // was written; the seed is zeroed here, in the outcome, once the
        // repository's own references are gone.
        do {
            try await repository.completeFromDeviceRestore(
                accountIdBytes: accountIdBytes,
                accountIdB64: base64Encode(accountIdBytes),
                username: target.username,
                phraseSeed: outcome.phraseSeed,
                sessionToken: token,
                sessionExpiresAtMs: expiry,
                persistJoin: outcome.persistJoin ?? {}
            )
        } catch {
            outcome.wipeSeed()
            throw error
        }
    }

    /// ADR-0018 factor (c) — submit the Authenticator mode: username
    /// (cached account wins, else the typed field) + the current
    /// 6-digit code. `AuthRepository.recoverWithTotp` drives the
    /// state flip — `.session` on the warm same-device path,
    /// `.needsRecoveryPhrase` on a fresh device — and the app router
    /// follows automatically. Mirror of Android's `onSubmitTotp`.
    public func submitTotp() {
        guard !busy else { return }
        let user = (cachedUsername ?? username)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !user.isEmpty else {
            errorText = .key("recovery.username_required_to_recover")
            return
        }
        guard let normalized = normalizeTotpCode(totpCode) else {
            errorText = .key("recovery.totp.error.enter_code")
            return
        }
        busy = true
        errorText = nil
        Task {
            do {
                try await repository.recoverWithTotp(
                    username: user,
                    code: normalized
                )
                // The repository flipped the auth state; wipe the
                // form buffer before the router replaces this screen.
                busy = false
                errorText = nil
                totpCode = ""
            } catch {
                busy = false
                let typedUsername = cachedUsername == nil
                self.errorText = UiText { AuthErrorCopy.totp(error, typedUsername: typedUsername) }
            }
        }
    }

    // MARK: - ADR-0018 factor (d) — phone / email OTP

    /// Step 1 — send a one-time code to a previously-enrolled phone /
    /// email. ENUMERATION-SAFE: a non-throwing return advances to the
    /// code step REGARDLESS of whether the contact matched a confirmed
    /// row (the server reveals nothing either way). Mirror of the web
    /// `ContactRecoverSection.onSendCode`.
    public func sendContactCode() {
        guard !busy else { return }
        let user = (cachedUsername ?? username)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !user.isEmpty else {
            errorText = .key("recovery.username_required_to_recover")
            return
        }
        // Light client-side shape check — saves a round trip on an
        // obviously-malformed contact. The server is authoritative.
        guard isPlausibleContact(kind: contactKind, contact: contact) else {
            errorText = .key(contactKind == .email
                ? "recovery.contact.error.invalid_email"
                : "recovery.contact.error.invalid_phone")
            return
        }
        busy = true
        errorText = nil
        let kindSnapshot = contactKind
        let contactSnapshot = contact
        Task {
            do {
                try await repository.recoverWithContactStart(
                    username: user,
                    kind: kindSnapshot,
                    contact: contactSnapshot
                )
                // Enumeration-safe: advance regardless of match.
                contactCode = ""
                contactStep = .code
                busy = false
                errorText = nil
            } catch {
                busy = false
                self.errorText = UiText { AuthErrorCopy.contact(error, sending: true) }
            }
        }
    }

    /// Step 2 — submit the delivered 6-digit code.
    /// `AuthRepository.recoverWithContactFinish` verifies it
    /// server-side and drives the state flip (`.session` on the warm
    /// same-device path, `.needsRecoveryPhrase` on a fresh device); the
    /// app router follows automatically. Mirror of the web
    /// `ContactRecoverSection.onSubmitCode`.
    public func submitContactCode() {
        guard !busy else { return }
        guard let normalized = normalizeTotpCode(contactCode) else {
            errorText = .key("recovery.contact.error.enter_code")
            return
        }
        busy = true
        errorText = nil
        Task {
            do {
                try await repository.recoverWithContactFinish(code: normalized)
                // The repository flipped the auth state; wipe the form
                // buffer before the router replaces this screen.
                busy = false
                errorText = nil
                contactCode = ""
            } catch {
                busy = false
                self.errorText = UiText { AuthErrorCopy.contact(error, sending: false) }
            }
        }
    }

    // MARK: - Constants

    /// Mirror of ``SignupViewModel/minPasswordLength``. The recovery
    /// flow sets a **new** password so the same floor applies.
    public static let minPasswordLength: Int = 8
}

/// ADR-0018 factor (a), path (b) — who the AnotherDevice mode pairs as:
/// the canonical username typed and its login handle (ADR-0056). No account
/// id: the fresh device learns it from the grant redeem.
public struct DeviceRestoreTarget: Equatable {
    public let username: String
    public let loginHandle: Data

    public init(username: String, loginHandle: Data) {
        self.username = username
        self.loginHandle = loginHandle
    }
}
