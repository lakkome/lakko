// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0048 — the app-scoped composition root for profile names: the own
/// profile, the received-profile store, username verification, and the local
/// renames the resolver ranks first.
///
/// One instance per process (``shared``), in the same posture as the other
/// process-scoped stores the app reads statically (``AcceptedContactsStore``):
/// names appear on a dozen surfaces owned by different view models, and a
/// single observable ``revision`` lets every one of them re-render in place
/// when a profile or a verdict arrives. The decision logic lives in the pure,
/// separately tested pieces it composes — ``ReceivedProfileStore``,
/// ``UsernameVerifier``, ``resolveDisplayName(signatureKey:localRename:profile:verification:)``.
///
/// App target only. The NotificationServiceExtension does not compile this
/// file: it never writes a profile (C-59) and never verifies a username.
@MainActor
public final class ProfileDirectory: ObservableObject {
    public static let shared = ProfileDirectory()

    /// Bumped whenever anything a resolved name depends on changes.
    @Published public private(set) var revision: UInt64 = 0
    /// The own profile of the active account.
    @Published public private(set) var ownProfile: OwnProfile?
    /// The active account's canonical username, `nil` for a guest account.
    @Published public private(set) var ownUsername: String?

    private(set) var accountIdB64: String?
    private let received: ReceivedProfileStore
    private let verifier: UsernameVerifier
    private let ownStore: OwnProfileStore
    /// base64 signature key -> the viewer's own name for that contact.
    private var renames: [String: String] = [:]
    /// Whether a lookup may be started from a render. Off in tests that drive
    /// the directory without a network.
    var verifiesOnResolve = true

    init(
        received: ReceivedProfileStore? = nil,
        verifier: UsernameVerifier? = nil,
        ownStore: OwnProfileStore = OwnProfileStore()
    ) {
        self.received = received ?? ReceivedProfileStore(
            persistence: UserDefaultsProfilePersistence(),
            isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }
        )
        self.verifier = verifier ?? UsernameVerifier(resolve: ProfileDirectory.discoveryResolve)
        self.ownStore = ownStore
        self.verifier.onChange = { [weak self] in self?.bump() }
        setRenames(from: AcceptedContactsStore.load())
    }

    // MARK: - Account lifecycle

    /// Make `accountIdB64` the active account. Idempotent.
    public func activate(accountIdB64: String, username: String?) {
        let canonical = username.flatMap { $0.isEmpty ? nil : $0 }
        if self.accountIdB64 == accountIdB64, ownUsername == canonical { return }
        self.accountIdB64 = accountIdB64
        ownUsername = canonical
        received.activate(accountIdB64: accountIdB64)
        verifier.activate(accountIdB64: accountIdB64)
        ownProfile = ownStore.load(accountIdB64: accountIdB64)
        bump()
    }

    /// No active account (logout). Stored data stays on disk.
    public func deactivate() {
        accountIdB64 = nil
        ownUsername = nil
        ownProfile = nil
        received.activate(accountIdB64: nil)
        verifier.activate(accountIdB64: nil)
        bump()
    }

    /// Forget-device: delete everything kept for `accountIdB64`.
    public func forget(accountIdB64: String) {
        received.clear(accountIdB64: accountIdB64)
        verifier.clear(accountIdB64: accountIdB64)
        ownStore.delete(accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 { deactivate() }
    }

    // MARK: - Own profile

    /// Set the own display name from what the user typed. Blank clears it.
    /// Throws ``DisplayNameRuleViolation`` with the reason copy.
    @discardableResult
    public func setOwnDisplayName(_ input: String, accountIdB64: String? = nil) throws -> OwnProfile {
        guard let account = accountIdB64 ?? self.accountIdB64 else {
            throw DisplayNameRuleViolation(message: "Sign in to set your name.")
        }
        let normalized = try checkDisplayName(input)
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        let saved = ownStore.save(displayName: normalized, accountIdB64: account, nowMs: nowMs)
        if account == self.accountIdB64 {
            ownProfile = saved
            bump()
        }
        return saved
    }

    /// The own profile as an envelope ready to publish, or `nil` without an
    /// active account. A guest publishes a name with no username.
    func ownProfileEnvelope() -> Data? {
        guard let profile = ownProfileForRequest() else { return nil }
        do {
            return try composeProfileEnvelope(profile: profile)
        } catch {
            // A stored name that no longer passes the core rule (or a username
            // that does not) is not published half-way: the envelope is
            // refused as a whole, exactly as a receiver would refuse it.
            NSLog("ADR-0048 own profile envelope refused: \(error)")
            return nil
        }
    }

    /// The own profile for a contact request, or `nil` without an account.
    ///
    /// Carries the account's 16 raw bytes (profile envelope v2) so a receiver
    /// can match this device's owner to a group-call tile: the SFU's per-call
    /// `peer_tag` (ADR-0021) is derived over the account id, which a signature
    /// key alone cannot produce. The id is already public to every peer that
    /// holds the identity bundle; here it only travels inside the E2EE
    /// envelope. An account id that is not exactly 16 bytes is sent as none —
    /// `composeProfileEnvelope` refuses any other length.
    func ownProfileForRequest() -> FfiProfile? {
        guard let accountIdB64 else { return nil }
        return FfiProfile(
            displayName: ownProfile?.displayName,
            username: ownUsername,
            updatedAtMs: ownProfile?.updatedAtMs ?? 0,
            accountId: Data(base64Encoded: accountIdB64).flatMap { $0.count == 16 ? $0 : nil }
        )
    }

    // MARK: - Received profiles

    /// Offer a profile received from someone else, bound to the signature key
    /// the carrier authenticated (C-68). Kept only when strictly newer.
    @discardableResult
    func acceptReceived(_ claim: ProfileClaim, signatureKeyB64: String?) -> ProfileAcceptance {
        let outcome = received.accept(claim, signatureKeyB64: signatureKeyB64)
        if outcome == .stored {
            bump()
            if let key = signatureKeyB64, let username = claim.username {
                startVerification(signatureKeyB64: key, username: username)
            }
        }
        return outcome
    }

    func storedProfile(signatureKeyB64: String) -> ProfileClaim? {
        received.profile(signatureKeyB64: signatureKeyB64)
    }

    /// Every received profile that names an account id, as
    /// `(signature key, account id)`. What a group-call roster folds into its
    /// per-call `peer_tag` map alongside the accepted contacts, so a member
    /// who is not a contact is still named on their tile.
    func profilesWithAccountId() -> [(signatureKeyB64: String, accountId: Data)] {
        received.profilesWithAccountId()
    }

    // MARK: - Local renames

    /// Refresh the rename map from the accepted contacts list.
    func setRenames(from accepted: [AcceptedContact]) {
        var next: [String: String] = [:]
        for contact in accepted {
            guard let name = contact.displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !name.isEmpty else { continue }
            next[base64Encode(contact.request.fromPubkey)] = name
        }
        guard next != renames else { return }
        renames = next
        bump()
    }

    // MARK: - Resolving

    /// The resolved name for the person holding `signatureKey`. Starts a
    /// username verification when the stored claim has no verdict yet.
    public func resolve(signatureKey: Data) -> ResolvedDisplayName {
        let keyB64 = base64Encode(signatureKey)
        let profile = received.profile(signatureKeyB64: keyB64)
        var state: UsernameVerificationState?
        if let username = profile?.username {
            state = verifier.state(signatureKeyB64: keyB64, username: username)
            if verifier.needsVerification(signatureKeyB64: keyB64, username: username) {
                startVerification(signatureKeyB64: keyB64, username: username)
            }
        }
        return resolveDisplayName(
            signatureKey: signatureKey,
            localRename: renames[keyB64],
            profile: profile,
            verification: UsernameVerificationStateView(state)
        )
    }

    /// ``resolve(signatureKey:)`` for a base64 key (a `MemberIdentity`).
    public func resolve(signatureKeyB64: String) -> ResolvedDisplayName? {
        guard let key = Data(base64Encoded: signatureKeyB64) else { return nil }
        return resolve(signatureKey: key)
    }

    private func startVerification(signatureKeyB64: String, username: String) {
        guard verifiesOnResolve else { return }
        Task { @MainActor [verifier] in
            await verifier.verify(signatureKeyB64: signatureKeyB64, username: username)
        }
    }

    private func bump() {
        revision &+= 1
    }

    // MARK: - Production username resolution

    /// `lookup -> account_id -> identityFetch -> signature public key`
    /// (ADR-0048 §3). A claim failing the username rule is treated as not
    /// registered without any network call — no conforming client can look it
    /// up, and it must never be shown.
    nonisolated static func discoveryResolve(_ username: String) async throws -> UsernameResolution {
        guard case .valid(let canonical) = checkUsername(username) else { return .notRegistered }
        let discovery = DiscoveryClient()
        let lookup = try await discovery.lookup(username: canonical)
        guard let accountId = lookup.account else { return .notRegistered }
        guard let bundle = try await discovery.identityFetch(accountId: accountId) else {
            return .notRegistered
        }
        guard let key = Data(base64Encoded: bundle.signaturePubkey), key.count == 32 else {
            return .notRegistered
        }
        return .signatureKey(key)
    }
}

// MARK: - Display-name rule

/// A display name the core refused, with the copy to show.
public struct DisplayNameRuleViolation: Error, LocalizedError, Equatable {
    public let message: String

    public var errorDescription: String? { message }
}

/// Normalise what the user typed as a display name: trimmed, blank -> `nil`,
/// or a throw with the reason. The rule is the core's `normalizeDisplayName`;
/// only the copy is here.
func checkDisplayName(
    _ input: String,
    normalize: (String) throws -> String? = { try normalizeDisplayName(input: $0) }
) throws -> String? {
    do {
        return try normalize(input)
    } catch let error as FfiError {
        switch error {
        // ADR-0058: the copy is the catalog's, in the interface language.
        case .DisplayNameLineBreak:
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.line_break"))
        case .DisplayNameControlOrFormatCharacter:
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.control"))
        case .DisplayNameTooLong:
            throw DisplayNameRuleViolation(
                message: Localization.text("display_name.error.too_long", counts: ["count": 64]))
        case .DisplayNameTooManyBytes:
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.too_many_bytes"))
        default:
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.unknown"))
        }
    }
}
