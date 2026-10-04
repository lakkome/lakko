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
/// process-scoped stores the app reads statically (``AcceptedContactsStore``,
/// whose rows are per account like everything here):
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
    /// ADR-0048 section 4a — called with the own profile envelope when this
    /// device saves or clears the own name, so the account's other devices
    /// get it over the DeviceGroup `ServiceEnvelope` (set by the app shell to
    /// the DeviceGroup sync runtime's own send). Never called for a profile
    /// adopted from another device: that one is already on the journal.
    var onOwnDeviceEnvelope: ((Data) -> Void)?

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
        if verifier == nil {
            self.verifier.verifyBinding = { [weak self] keyB64, username in
                guard let self else { return false }
                return try await self.verifyCertificate(signatureKeyB64: keyB64, username: username)
            }
        }
        // The renames of whichever account is bound; empty before a session.
        // ``activate(accountIdB64:username:)`` reloads them per account.
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
        // This account's own names for its contacts, never the previous one's.
        setRenames(from: AcceptedContactsStore.load(accountIdB64: accountIdB64))
        bump()
    }

    /// No active account (logout). Stored data stays on disk.
    public func deactivate() {
        accountIdB64 = nil
        ownUsername = nil
        ownProfile = nil
        setRenames(from: [])
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
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.signed_out"))
        }
        let normalized = try checkDisplayName(input)
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        let saved = ownStore.save(displayName: normalized, accountIdB64: account, nowMs: nowMs)
        if account == self.accountIdB64 {
            ownProfile = saved
            bump()
            // The account's other devices learn the change too (ADR-0048
            // section 4a). A save for an account not yet active (a signup
            // name stored before the session activates the directory) has no
            // other device to reach; a device added later is handed it by
            // ``ownDeviceEnvelopes(accountIdB64:)``.
            if let envelope = ownDeviceEnvelope() {
                onOwnDeviceEnvelope?(envelope)
            }
        }
        return saved
    }

    // MARK: - Own profile between the account's devices (ADR-0048 section 4a)

    /// The own profile as the envelope the account's OTHER devices get over
    /// the DeviceGroup, or `nil` without an active account or when the name
    /// was never set (stamp 0: a receiver holds the same and would refuse it).
    ///
    /// Composed with the STORED stamp, never a fresh one (a re-send carries
    /// its original stamp, the ADR-0044 lesson), with the display name (none
    /// when cleared), this account's id so a receiver can refuse another
    /// account's, and no username, certificate or grants: a receiver keeps
    /// only the display name, the username stays its own session's.
    func ownDeviceEnvelope() -> Data? {
        guard let accountIdB64, let own = ownProfile, own.updatedAtMs > 0 else { return nil }
        let profile = FfiProfile(
            displayName: own.displayName,
            username: nil,
            updatedAtMs: own.updatedAtMs,
            accountId: Data(base64Encoded: accountIdB64).flatMap { $0.count == 16 ? $0 : nil },
            usernameCertificate: nil,
            grants: []
        )
        do {
            return try composeProfileEnvelope(profile: profile)
        } catch {
            NSLog("ADR-0048 own profile device envelope refused: \(error)")
            return nil
        }
    }

    /// The own profile envelope for a device this one has just admitted to
    /// the DeviceGroup: ``ownDeviceEnvelope()`` when `accountIdB64` is the
    /// active account, else nothing.
    func ownDeviceEnvelopes(accountIdB64: String) -> [Data] {
        guard self.accountIdB64 == accountIdB64, let envelope = ownDeviceEnvelope() else {
            return []
        }
        return [envelope]
    }

    /// What ``adoptOwnProfileFromDevice(envelope:)`` did.
    enum OwnProfileAdoption: Equatable {
        /// Strictly newer than the stored own profile: kept, stamp as received.
        case adopted
        /// Equal to or older than the stored one: nothing changed.
        case notNewer
        /// It names an account other than this one: dropped.
        case foreignAccount
        /// Not a profile envelope the core accepts: dropped.
        case notAProfile
        /// No account is active.
        case inactive
    }

    /// Adopt the own profile another of this account's devices sent over the
    /// DeviceGroup (ADR-0048 section 4a). Only the display name is kept, and
    /// only when the received stamp is strictly greater than the stored one
    /// (`profileIsNewer`); the stamp is kept as received. A profile that names
    /// another account is dropped. Not republished to any group: the device
    /// that made the change already did, and not handed back to
    /// ``onOwnDeviceEnvelope``.
    @discardableResult
    func adoptOwnProfileFromDevice(envelope: Data) -> OwnProfileAdoption {
        guard let accountIdB64 else { return .inactive }
        guard let incoming = try? decodeProfileEnvelope(plaintext: envelope) else {
            return .notAProfile
        }
        if let claimed = incoming.accountId, claimed != Data(base64Encoded: accountIdB64) {
            return .foreignAccount
        }
        guard profileIsNewer(
            incomingUpdatedAtMs: incoming.updatedAtMs,
            storedUpdatedAtMs: ownProfile?.updatedAtMs ?? 0)
        else { return .notNewer }
        let adopted = OwnProfile(displayName: incoming.displayName, updatedAtMs: incoming.updatedAtMs)
        ownStore.store(adopted, accountIdB64: accountIdB64)
        ownProfile = adopted
        bump()
        return .adopted
    }

    /// The own profile as an envelope ready to publish into one conversation,
    /// or `nil` without an active account. A guest publishes a name with no
    /// username.
    ///
    /// ADR-0060 section 3 — profile v3 carries the grants of THIS
    /// conversation: `contact` + `member` in a one-to-one, `member` (plus
    /// `group` where the setting allows it) in a group; and the username
    /// only where the setting lets this conversation's members find the
    /// owner (UX section 6.2), always with its certificate.
    func ownProfileEnvelope(groupId: Data? = nil, oneToOne: Bool = false) -> Data? {
        guard var profile = ownProfileForRequest() else { return nil }
        if let groupId {
            let reach = ReachCenter.shared
            profile.grants = reach.profileGrants(groupId: groupId, oneToOne: oneToOne)
            if !oneToOne, !reach.allowsGroup(groupId) {
                profile.username = nil
                profile.usernameCertificate = nil
            }
        }
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
    ///
    /// ADR-0060 section 5 — the username travels with its certificate, and
    /// only when this device holds one: a username without a certificate is
    /// never shown by a v3 receiver, so it is not sent either.
    func ownProfileForRequest() -> FfiProfile? {
        guard let accountIdB64 else { return nil }
        let cert = ReachCenter.shared.ownUsernameCertificate
        let username = cert == nil ? nil : ownUsername
        return FfiProfile(
            displayName: ownProfile?.displayName,
            username: username,
            updatedAtMs: ownProfile?.updatedAtMs ?? 0,
            accountId: Data(base64Encoded: accountIdB64).flatMap { $0.count == 16 ? $0 : nil },
            usernameCertificate: username == nil ? nil : cert,
            grants: []
        )
    }

    // MARK: - Received profiles

    /// Offer a profile received from someone else, bound to the signature key
    /// the carrier authenticated (C-68). Kept only when strictly newer.
    @discardableResult
    func acceptReceived(_ claim: ProfileClaim, signatureKeyB64: String?) -> ProfileAcceptance {
        // Grants are scoped to the conversation they arrived in and handed to
        // ``ReachCenter`` by the caller; the profile store never holds them.
        let outcome = received.accept(claim.withoutGrants, signatureKeyB64: signatureKeyB64)
        if outcome == .stored {
            bump()
            if let key = signatureKeyB64, let username = claim.username {
                startVerification(signatureKeyB64: key, username: username)
            }
        }
        return outcome
    }

    /// Every received profile, by signature key (b64): what local people
    /// search reads (ADR-0060 section 3).
    func receivedProfiles() -> [String: ProfileClaim] {
        received.all()
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
        var nextAliases: [String: String] = [:]
        for contact in accepted {
            // ADR-0065 phase 2 (UX 16.8) — a flagged contact's pending key
            // names the same person: the resolver falls through to the record
            // by the previous key, so a bubble from the new keys carries the
            // contact's name rather than "Member" with a warning under it.
            if let reset = contact.keyReset {
                nextAliases[reset.pendingKeyB64] = base64Encode(contact.request.fromPubkey)
            }
            guard let name = contact.displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !name.isEmpty else { continue }
            next[base64Encode(contact.request.fromPubkey)] = name
        }
        guard next != renames || nextAliases != aliases else { return }
        renames = next
        aliases = nextAliases
        bump()
    }

    /// ADR-0065 phase 2 — `pending key -> previous key` for every contact in
    /// the `reset-unverified` state; see ``setRenames(from:)``.
    private var aliases: [String: String] = [:]

    // MARK: - Resolving

    /// The resolved name for the person holding `signatureKey`. Starts a
    /// username verification when the stored claim has no verdict yet.
    public func resolve(signatureKey: Data) -> ResolvedDisplayName {
        let keyB64 = base64Encode(signatureKey)
        // A key nothing is known under that is a flagged contact's pending
        // key resolves as the contact (UX 16.1, "resolved through the account
        // id"); a profile the new keys later publish wins over the alias.
        if renames[keyB64] == nil, received.profile(signatureKeyB64: keyB64) == nil,
           let previous = aliases[keyB64], let previousKey = Data(base64Encoded: previous) {
            return resolve(signatureKey: previousKey)
        }
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

    /// The account id a member's own profile claimed (profile envelope v2), or
    /// `nil`. A claim: ADR-0057 uses it only to fetch an identity bundle whose
    /// signature key must then match the member's.
    public func accountId(signatureKeyB64: String) -> Data? {
        received.profile(signatureKeyB64: signatureKeyB64)?.accountId
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

    // MARK: - ADR-0060 username certificate verification

    /// The pinned (or, when no key is pinned at build time, fetched) username
    /// certificate verifying key.
    private var certKey: Data?

    /// Verify the username a stored profile claims against the certificate it
    /// carried: the certificate's signature under the discovery key, bound to
    /// the lookup key of the claimed username (one evaluation) and to the
    /// account id the profile arrived with. No certificate, or no account id,
    /// is a refusal: the username is never shown (invariant #4).
    func verifyCertificate(signatureKeyB64: String, username: String) async throws -> Bool {
        guard let claim = received.profile(signatureKeyB64: signatureKeyB64),
              claim.username == username,
              let cert = claim.usernameCertificate,
              let accountId = claim.accountId else { return false }
        guard case .valid(let canonical) = checkUsername(username) else { return false }
        let key = try await usernameCertKey()
        // The certificate binds the LOOKUP KEY of the username (ADR-0060
        // section 5), never the OPRF output.
        let lookupKey = try await DiscoveryClient().usernameOprf(canonical).lookupKey
        do {
            _ = try verifyUsernameBinding(
                certificate: cert, verifyingKey: key, expectedOprfHash: lookupKey,
                expectedAccountId: accountId)
            return true
        } catch {
            return false
        }
    }

    /// Verify one username certificate for an already-computed lookup key
    /// (`OprfResult.lookupKey`, never the output) and account id (a search
    /// result). `false` on any failure.
    func verifyCertificate(certificate: Data, lookupKey: Data, accountId: Data) async -> Bool {
        guard let key = try? await usernameCertKey() else { return false }
        return (try? verifyUsernameBinding(
            certificate: certificate, verifyingKey: key, expectedOprfHash: lookupKey,
            expectedAccountId: accountId)) != nil
    }

    private func usernameCertKey() async throws -> Data {
        if let certKey { return certKey }
        if let pinned = Self.pinnedUsernameCertKey {
            certKey = pinned
            return pinned
        }
        guard let fetched = try await DiscoveryClient().fetchUsernameCertKey(), fetched.count == 32
        else { throw DisplayNameRuleViolation(message: "username certificate key unavailable") }
        certKey = fetched
        return fetched
    }

    /// Info.plist `LakkoUsernameCertKey` — base64url (or base64) of the 32-byte
    /// Ed25519 key discovery signs username certificates with, pinned at build
    /// time (the ADR-0015 pattern). Empty -> fetched from
    /// `/.well-known/lakko-username-cert-key`.
    nonisolated static var pinnedUsernameCertKey: Data? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoUsernameCertKey") as? String,
              !raw.isEmpty else { return nil }
        let key = base64UrlDecodeNoPad(raw) ?? Data(base64Encoded: raw)
        return key?.count == 32 ? key : nil
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
        // ADR-0061: the copy is the catalog's, in the interface language.
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
