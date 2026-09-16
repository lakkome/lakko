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
            error = nil
        }
    }

    /// ADR-0018 — non-nil once the AnotherDevice mode resolved the
    /// typed username to an account id via discovery.
    @Published public private(set) var deviceRestoreTarget: DeviceRestoreTarget?
    @Published public var phrase: String = ""
    @Published public var sharesInput: String = ""
    @Published public var password: String = ""
    @Published public var confirm: String = ""
    /// ADR-0017 — username for the fresh-device recover path. When no
    /// account is cached on this device, the recover flow resolves the
    /// account_id via `DiscoveryClient.lookup(username)`, so the user
    /// must type their handle. Pre-filled with ``cachedUsername`` when
    /// present (a cached account wins and ignores this field).
    @Published public var username: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    @Published public private(set) var error: String?
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
            error = "Passwords do not match."
            return
        }
        if password.count < Self.minPasswordLength {
            error = "Password must be at least \(Self.minPasswordLength) characters."
            return
        }
        // ADR-0017 — on a fresh device (no cached account) the username
        // is required to resolve the account_id via discovery.
        let trimmedUsername = username.trimmingCharacters(in: .whitespacesAndNewlines)
        if cachedUsername == nil && trimmedUsername.isEmpty {
            error = "Enter your username so we can find your account."
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
            error = "Use the QR flow above to restore."
            return
        case .totp:
            // ADR-0018 factor (c) — the Authenticator mode submits
            // via `submitTotp` (no new password on that path).
            // Defensive no-op.
            error = "Use the authenticator-code form."
            return
        case .contact:
            // ADR-0018 factor (d) — the Email/SMS mode submits via
            // `sendContactCode` / `submitContactCode` (no new password
            // on that path). Defensive no-op.
            error = "Use the email / SMS code form."
            return
        case .phrase:
            if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                error = "Recovery phrase must be non-empty."
                return
            }
            phraseSnapshot = phrase
        case .shares:
            let lines = sharesInput
                .split(whereSeparator: \.isNewline)
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }
            if lines.count < 2 {
                error = "At least 2 shares required (Lakko default is 3-of-5)."
                return
            }
            do {
                phraseSnapshot = try ShamirBridge.combineSharesToMnemonic(
                    encodedShares: lines
                )
            } catch {
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Couldn't combine shares."
                return
            }
        }

        busy = true
        error = nil

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
                error = nil
                phrase = ""
                sharesInput = ""
                password = ""
                confirm = ""
            } catch let err as FfiError {
                // BIP39 mnemonic parse failure. UniFFI surfaces both
                // unknown-word and bad-checksum cases through the
                // single `Mnemonic` variant of `FfiError` — by
                // design (preserves the no-probing property). Swift
                // case names are PascalCase per the UniFFI codegen
                // shape (the Kotlin side uses `FfiException` with
                // PascalCase sub-classes — same vocabulary, different
                // surface).
                if case .Mnemonic = err {
                    busy = false
                    error = "Invalid recovery phrase. Check the spelling and order of all 24 words."
                } else {
                    busy = false
                    self.error = err.localizedDescription
                }
            } catch let err as AuthClientError {
                busy = false
                error = userFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Recovery failed."
            }
        }
    }

    // MARK: - ADR-0018 factor (a), path (b) — another-device restore

    /// Resolve the typed username to an account id via the discovery
    /// service so the AnotherDevice restore can build the pairing QR.
    /// Anonymous (OPRF-blinded lookup, no session needed). Mirror of
    /// Android's `RecoverViewModel.lookupForDeviceRestore`.
    public func lookupForDeviceRestore() {
        guard !busy else { return }
        let typed = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !typed.isEmpty else {
            error = "Enter your username first."
            return
        }
        busy = true
        error = nil
        Task {
            do {
                let discovery = DiscoveryClient()
                let resolved = try await discovery.lookup(username: typed)
                guard let account = resolved.account else {
                    busy = false
                    error = "No account found for that username."
                    return
                }
                deviceRestoreTarget = DeviceRestoreTarget(
                    accountIdBytes: account,
                    accountIdB64: base64Encode(account),
                    username: resolved.normalisedUsername
                )
                busy = false
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Lookup failed. Try again."
            }
        }
    }

    /// Complete the no-password restore with the identity + redeemed
    /// session token received from the other device. Async passthrough
    /// to ``AuthRepository/completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseSeed:sessionToken:sessionExpiresAtMs:)``;
    /// verification failures propagate to the calling panel. Requires
    /// the redeemed token (without it this device has no ACCESS) —
    /// refused otherwise with a clear message. Mirror of Android's
    /// `RecoverViewModel.completeFromDeviceRestore`.
    public func completeFromDeviceRestore(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        guard let target = deviceRestoreTarget else {
            throw AuthClientError.argument(
                "no resolved account — look up your username first"
            )
        }
        guard let token = outcome.sessionToken,
              let expiry = outcome.sessionExpiresAtMs else {
            // Seed-only outcome: the grant redeem failed, so this
            // device has IDENTITY but no ACCESS. Refuse rather than
            // leave a half-restored profile; the user retries (a
            // fresh pairing mints a fresh grant). Best-effort wipe of
            // the local seed copy.
            var seed = outcome.phraseSeed
            seed.resetBytes(in: 0..<seed.count)
            throw AuthClientError.argument(
                "The other device transferred your identity but the " +
                "sign-in grant could not be redeemed. Try again."
            )
        }
        try await repository.completeFromDeviceRestore(
            accountIdBytes: target.accountIdBytes,
            accountIdB64: target.accountIdB64,
            username: target.username,
            phraseSeed: outcome.phraseSeed,
            sessionToken: token,
            sessionExpiresAtMs: expiry
        )
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
            error = "Enter the username of the account to recover."
            return
        }
        guard let normalized = normalizeTotpCode(totpCode) else {
            error = "Enter the 6-digit code from your authenticator app."
            return
        }
        busy = true
        error = nil
        Task {
            do {
                try await repository.recoverWithTotp(
                    username: user,
                    code: normalized
                )
                // The repository flipped the auth state; wipe the
                // form buffer before the router replaces this screen.
                busy = false
                error = nil
                totpCode = ""
            } catch let err as AuthClientError {
                busy = false
                error = totpUserFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Recovery failed."
            }
        }
    }

    /// Map a `recoverWithTotp` failure onto friendly inline copy. The
    /// server is enumeration-safe: 401 covers unknown-account /
    /// unconfirmed-enrollment / wrong-code / replayed-code uniformly
    /// (mirror of Android's `totpUserFacingHttpError`).
    private func totpUserFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 401:
                return "Wrong or expired code."
            case 429:
                return "Too many attempts — wait 15 minutes."
            case 503:
                return "Authenticator recovery is not available on this server."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Recovery failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
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
            error = "Enter the username of the account to recover."
            return
        }
        // Light client-side shape check — saves a PoW solve on an
        // obviously-malformed contact. The server is authoritative.
        guard isPlausibleContact(kind: contactKind, contact: contact) else {
            error = contactKind == .email
                ? "Enter a valid email address."
                : "Enter a phone number in international format (e.g. +15551234567)."
            return
        }
        busy = true
        error = nil
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
                error = nil
            } catch let err as AuthClientError {
                busy = false
                error = contactUserFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Couldn't send the code."
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
            error = "Enter the 6-digit code we sent."
            return
        }
        busy = true
        error = nil
        Task {
            do {
                try await repository.recoverWithContactFinish(code: normalized)
                // The repository flipped the auth state; wipe the form
                // buffer before the router replaces this screen.
                busy = false
                error = nil
                contactCode = ""
            } catch let err as AuthClientError {
                busy = false
                error = contactUserFacing(err)
            } catch {
                busy = false
                self.error = (error as? LocalizedError)?.errorDescription
                    ?? "Recovery failed."
            }
        }
    }

    /// Map a phone / email OTP recover failure onto friendly inline
    /// copy. The server is enumeration-safe: 401 covers unknown account
    /// / no-live-challenge / wrong-code / replayed-code uniformly.
    /// Mirror of the web `contactRecoverErrorMessage`.
    private func contactUserFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 400:
                return "That contact looks malformed. Check it and try again."
            case 401:
                return "Wrong or expired code. Check the code we sent and try again."
            case 429:
                return "Too many attempts — wait, then request a fresh code."
            case 503:
                return "Email / SMS recovery is not available on this server."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Recovery failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        }
    }

    // MARK: - Error mapping

    /// Map server-side HTTP errors to user-facing copy. The
    /// `/password/reset/{start,finish}` pair returns 401 on a wrong
    /// mnemonic (the OPAQUE recovery login fails at AKE
    /// finalisation). Surface generic copy — wrong-mnemonic and
    /// wrong-account collapse into the same response by design.
    private func userFacing(_ err: AuthClientError) -> String {
        switch err {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 401:
                return "Invalid recovery phrase."
            case 429:
                return "Too many attempts. Please wait a moment and retry."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "Recovery failed (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        }
    }

    // MARK: - Constants

    /// Mirror of ``SignupViewModel/minPasswordLength``. The recovery
    /// flow sets a **new** password so the same floor applies.
    public static let minPasswordLength: Int = 8
}

/// ADR-0018 factor (a), path (b) — the resolved restore target the
/// AnotherDevice mode pairs against. Mirror of Android's
/// `DeviceRestoreTarget`.
public struct DeviceRestoreTarget: Equatable {
    public let accountIdBytes: Data
    public let accountIdB64: String
    public let username: String

    public init(accountIdBytes: Data, accountIdB64: String, username: String) {
        self.accountIdBytes = accountIdBytes
        self.accountIdB64 = accountIdB64
        self.username = username
    }
}
