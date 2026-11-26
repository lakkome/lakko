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
    public static let shared = ProfileDirectory(deletions: .shared)

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
    /// ADR-0101 section 8 — the ACCOUNT identity (ADR-0050, never a device
    /// leaf) that signs every own profile envelope (profile v6), for the
    /// account named; `nil` when this device holds none for it. Set by the
    /// app shell; with none set no own profile envelope can be composed.
    var accountIdentityProvider: (@MainActor (_ accountIdB64: String) -> Identity?)?
    /// ADR-0098 — who is a deleted account and whose username may be shown.
    /// `nil` (a directory built by a test) shows names as before.
    let deletions: DeletedAccounts?
    private var deletionsWatch: AnyCancellable?

    init(
        received: ReceivedProfileStore? = nil,
        verifier: UsernameVerifier? = nil,
        ownStore: OwnProfileStore = OwnProfileStore(),
        deletions: DeletedAccounts? = nil
    ) {
        self.deletions = deletions
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
        // A person learned deleted (or confirmed) re-renders every name.
        deletionsWatch = deletions?.$revision.dropFirst().sink { [weak self] _ in
            MainActor.assumeIsolated { self?.bump() }
        }
    }

    // MARK: - Account lifecycle

    /// Make `accountIdB64` the active account. Idempotent.
    public func activate(accountIdB64: String, username: String?) {
        let canonical = username.flatMap { $0.isEmpty ? nil : $0 }
        if self.accountIdB64 == accountIdB64, ownUsername == canonical { return }
        self.accountIdB64 = accountIdB64
        ownUsername = canonical
        deletions?.activate(accountIdB64: accountIdB64)
        received.activate(accountIdB64: accountIdB64)
        verifier.activate(accountIdB64: accountIdB64)
        ownProfile = ownStore.load(accountIdB64: accountIdB64)
        // This account's own names for its contacts, never the previous one's.
        setRenames(from: AcceptedContactsStore.load(accountIdB64: accountIdB64))
        // ADR-0079 — and this account's pending contacts, the names their
        // invite links stated. ``ContactsRepository`` keeps them current.
        setPendingContacts(PendingContactStore.load(accountIdB64: accountIdB64))
        // ADR-0074 section 3.5 — the proven-predecessor map of this account's
        // contacts, re-read whenever the pin gate records a proven rotation.
        provenPredecessors = ProvenPredecessorStore(ownerAccountIdB64: accountIdB64).load()
        if predecessorObserver == nil {
            predecessorObserver = NotificationCenter.default.addObserver(
                forName: ProvenPredecessorStore.changed, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.reloadProvenPredecessors() }
            }
        }
        bump()
    }

    /// ADR-0074 section 3.5 — `predecessor key -> current key` for every
    /// contact whose key change the ADR-0016 gate accepted BY PROOF. The same
    /// person under two keys: a name known under either resolves for both.
    /// Never fed by an unproven change.
    private var provenPredecessors = ProvenPredecessors()
    private var predecessorObserver: NSObjectProtocol?

    private func reloadProvenPredecessors() {
        guard let accountIdB64 else { return }
        let next = ProvenPredecessorStore(ownerAccountIdB64: accountIdB64).load()
        guard next != provenPredecessors else { return }
        provenPredecessors = next
        bump()
    }

    /// Replace the proven-predecessor map (tests).
    func setProvenPredecessors(_ map: ProvenPredecessors) {
        guard map != provenPredecessors else { return }
        provenPredecessors = map
        bump()
    }

    /// The other key of the same person, across a proven rotation, that
    /// something is known under: a local rename first, then a received
    /// profile, then a pending contact's link statement. `nil` when `keyB64`
    /// itself is known, or nothing is.
    private func provenAlias(of keyB64: String) -> String? {
        guard !provenPredecessors.isEmpty, !knowsSomething(keyB64) else { return nil }
        let others = provenPredecessors.allKeys(of: keyB64).filter { $0 != keyB64 }
        return others.first { renames[$0] != nil }
            ?? others.first { received.profile(signatureKeyB64: $0) != nil }
            ?? others.first { pendingClaims[$0] != nil }
    }

    /// `true` when the viewer knows SOMETHING about `keyB64` itself: a local
    /// rename, a received profile, or the name a pending contact's invite
    /// link stated (ADR-0079).
    private func knowsSomething(_ keyB64: String) -> Bool {
        renames[keyB64] != nil
            || profileKey(for: keyB64) != nil
            || pendingClaims[keyB64] != nil
    }

    /// No active account (logout). Stored data stays on disk.
    public func deactivate() {
        accountIdB64 = nil
        ownUsername = nil
        ownProfile = nil
        setRenames(from: [])
        setPendingContacts([])
        received.activate(accountIdB64: nil)
        verifier.activate(accountIdB64: nil)
        deletions?.deactivate()
        bump()
    }

    /// Forget-device: delete everything kept for `accountIdB64`.
    public func forget(accountIdB64: String) {
        received.clear(accountIdB64: accountIdB64)
        verifier.clear(accountIdB64: accountIdB64)
        ownStore.delete(accountIdB64: accountIdB64)
        deletions?.forget(accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 { deactivate() }
    }

    // MARK: - Own profile

    /// Set the own display name from what the user typed. Blank clears it.
    /// Throws ``DisplayNameRuleViolation`` with the reason copy.
    @discardableResult
    public func setOwnDisplayName(_ input: String, accountIdB64: String? = nil) throws -> OwnProfile {
        let account = accountIdB64 ?? self.accountIdB64
        let avatar = account.flatMap { ownStore.load(accountIdB64: $0)?.avatar }
        return try setOwnProfile(displayName: input, avatar: avatar, accountIdB64: account)
    }

    /// ADR-0076 — the username is part of the profile, so a rename is a new
    /// profile: the stored name and photo saved again with a fresh stamp.
    /// With the old stamp every receiver took the republished envelope for
    /// one it already held, and a rename never reached a contact or a group
    /// (base scenario 3, 2026-11-11; Android re-stamps the same way).
    public func restampOwnProfile() {
        guard let account = accountIdB64 else { return }
        let own = ownStore.load(accountIdB64: account)
        do {
            try setOwnProfile(displayName: own?.displayName ?? "", avatar: own?.avatar, accountIdB64: account)
        } catch {
            NSLog("ADR-0076: the own profile was not re-stamped after a rename")
        }
    }

    /// ADR-0084 — set the own display name and profile photo together, as one
    /// new profile with a fresh stamp (a new or removed photo is an update
    /// exactly like a new name). Throws ``DisplayNameRuleViolation``.
    @discardableResult
    public func setOwnProfile(
        displayName input: String, avatar: ProfileAvatar?, accountIdB64: String? = nil
    ) throws -> OwnProfile {
        guard let account = accountIdB64 ?? self.accountIdB64 else {
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.signed_out"))
        }
        let normalized = try checkDisplayName(input)
        // A name the store refuses (the account's session ended,
        // ``AccountStoreGate``) is not kept and never sent to the account's
        // other devices: refused here, before the write, as signed out.
        guard let gate = AccountStoreGate.enter(account) else {
            throw DisplayNameRuleViolation(message: Localization.text("display_name.error.signed_out"))
        }
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        let saved = ownStore.save(
            displayName: normalized, avatar: avatar, accountIdB64: account, nowMs: nowMs,
            cardShareable: account == self.accountIdB64 ? ownCardShareable : nil)
        // The backup carries the name and the photo.
        AutoBackupStore.markDirty(accountIdB64: account, nowMs: Int64(nowMs))
        gate.leave()
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

    /// The account identity that signs an own profile envelope of
    /// `accountIdB64` (ADR-0101 section 8), or `nil`.
    func signingIdentity(accountIdB64: String) -> Identity? {
        accountIdentityProvider?(accountIdB64)
    }

    /// Compose an own profile envelope of `accountIdB64`, signed by its
    /// account identity. Throws when no account identity is held.
    private func composeOwnEnvelope(_ profile: FfiProfile, accountIdB64: String) throws -> Data {
        guard let identity = signingIdentity(accountIdB64: accountIdB64) else {
            throw ProfileSigningError.noAccountIdentity
        }
        return try composeProfileEnvelope(profile: profile, accountIdentity: identity)
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
            grants: [],
            avatar: own.avatar?.ffi,
            cardShareable: ownCardShareable
        )
        do {
            return try composeOwnEnvelope(profile, accountIdB64: accountIdB64)
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
        // ADR-0084 — the photo travels with the name between the account's
        // devices: the newer profile carries both.
        let adopted = OwnProfile(
            displayName: incoming.displayName, updatedAtMs: incoming.updatedAtMs,
            avatar: ProfileAvatar(incoming.avatar), cardShareable: incoming.cardShareable)
        ownStore.store(adopted, accountIdB64: accountIdB64)
        ownProfile = adopted
        bump()
        return .adopted
    }

    // MARK: - ADR-0101 section 8 — the profile after a key reset

    /// Told after a returned own profile was adopted: the app republishes it
    /// in every conversation. Set by the app shell.
    var onOwnProfileReturned: (() -> Void)?

    /// The newest raw envelope held under `signatureKeyB64`, with its stamp,
    /// or `nil` (no row, or a row held without its envelope).
    func heldEnvelope(signatureKeyB64: String) -> (envelope: Data, updatedAtMs: UInt64)? {
        guard let claim = received.profile(signatureKeyB64: signatureKeyB64), let envelope = claim.envelope else {
            return nil
        }
        return (envelope, claim.updatedAtMs)
    }

    /// A profile received in a conversation, routed by its signer
    /// (``ProfileReturn/route(verifiesUnderSender:verifiesUnderPreviousOwnKey:)``):
    /// filed under the sender (resolved at receipt, C-68) only when the
    /// sender signed it, offered as this account's own when its previous key
    /// did (a peer carried it back after this account's key reset), dropped
    /// otherwise.
    func fileReceivedGroupProfile(_ claim: ProfileClaim, senderKeyB64: String?, accountIdB64 receivedFor: String?) {
        guard let envelope = claim.envelope, let receivedFor else { return }
        let previous = ResetContinuityStore().read(accountIdB64: receivedFor)?.previousKeyB64
        switch ProfileReturn.route(
            verifiesUnderSender: ProfileReturn.verifies(envelope, underKeyB64: senderKeyB64),
            verifiesUnderPreviousOwnKey: ProfileReturn.verifies(envelope, underKeyB64: previous)) {
        case .file:
            acceptReceived(claim, signatureKeyB64: senderKeyB64, accountIdB64: receivedFor)
        case .returnedOwn:
            offerReturnedOwnProfile(envelope: envelope, accountIdB64: receivedFor)
        case .drop:
            break
        }
    }

    /// Offer one returned profile envelope for the signed-in account.
    /// Adopted only when this device recorded a previous key for the reset,
    /// the envelope verifies under it and is strictly newer than the own
    /// profile held; then its name and photo references (never the username,
    /// which belongs to the account) are kept, sent to the account's other
    /// devices and republished everywhere. Returns whether it was adopted.
    @discardableResult
    func offerReturnedOwnProfile(envelope: Data, accountIdB64 forAccount: String) -> Bool {
        guard forAccount == accountIdB64,
              let previous = ResetContinuityStore().read(accountIdB64: forAccount)?.previousKeyB64,
              let key = base64Decode(previous),
              let verified = try? verifyProfileEnvelope(plaintext: envelope, identityPk: key),
              ProfileReturn.adopts(candidateStamp: verified.updatedAtMs, heldStamp: ownProfile?.updatedAtMs ?? 0),
              let gate = AccountStoreGate.enter(forAccount)
        else { return false }
        let adopted = OwnProfile(
            displayName: verified.displayName, updatedAtMs: verified.updatedAtMs,
            avatar: ProfileAvatar(verified.avatar), cardShareable: ownCardShareable)
        ownStore.store(adopted, accountIdB64: forAccount)
        AutoBackupStore.markDirty(accountIdB64: forAccount, nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        gate.leave()
        ownProfile = adopted
        bump()
        if let own = ownDeviceEnvelope() { onOwnDeviceEnvelope?(own) }
        onOwnProfileReturned?()
        NSLog("ADR-0101: the own profile came back after the key reset")
        return true
    }

    /// The own profile as the envelope a backup carries for `accountIdB64`,
    /// stored stamp and all, or `nil` when it was never set. Read from the
    /// store, so a baseline sent before the session mounted carries it too;
    /// the username and its certificate go along when this account is the
    /// active one. No grants: they belong to a conversation.
    func backupProfileEnvelope(accountIdB64: String) -> Data? {
        guard let own = ownStore.load(accountIdB64: accountIdB64), own.updatedAtMs > 0 else { return nil }
        let username = accountIdB64 == self.accountIdB64 ? ownUsername : nil
        let profile = FfiProfile(
            displayName: own.displayName,
            username: username,
            updatedAtMs: own.updatedAtMs,
            accountId: Data(base64Encoded: accountIdB64).flatMap { $0.count == 16 ? $0 : nil },
            usernameCertificate: username == nil ? nil : ReachCenter.shared.ownUsernameCertificate,
            grants: [],
            avatar: own.avatar?.ffi,
            cardShareable: accountIdB64 == self.accountIdB64 ? ownCardShareable : (own.cardShareable ?? true)
        )
        do {
            return try composeOwnEnvelope(profile, accountIdB64: accountIdB64)
        } catch {
            NSLog("ADR-0048 own profile backup envelope refused: \(error)")
            return nil
        }
    }

    /// The own profile a restored backup carried, for `accountIdB64`, which
    /// may not be the active account yet (a route restores before its session
    /// lands). Fail closed: bytes the core refuses, or a profile of another
    /// account, change nothing. Kept only when the store holds nothing newer,
    /// stamp as received.
    @discardableResult
    func adoptRestoredOwnProfile(envelope: Data, accountIdB64: String) -> OwnProfileAdoption {
        guard let incoming = try? decodeProfileEnvelope(plaintext: envelope) else {
            return .notAProfile
        }
        if let claimed = incoming.accountId, claimed != Data(base64Encoded: accountIdB64) {
            return .foreignAccount
        }
        guard profileIsNewer(
            incomingUpdatedAtMs: incoming.updatedAtMs,
            storedUpdatedAtMs: ownStore.load(accountIdB64: accountIdB64)?.updatedAtMs ?? 0)
        else { return .notNewer }
        let adopted = OwnProfile(
            displayName: incoming.displayName, updatedAtMs: incoming.updatedAtMs,
            avatar: ProfileAvatar(incoming.avatar), cardShareable: incoming.cardShareable)
        ownStore.store(adopted, accountIdB64: accountIdB64)
        if accountIdB64 == self.accountIdB64 {
            ownProfile = adopted
            bump()
        }
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
    func ownProfileEnvelope(groupId: Data? = nil, oneToOne: Bool = false, contactGrant: Bool = true) -> Data? {
        guard let accountIdB64, var profile = ownProfileForRequest() else { return nil }
        if let groupId {
            let reach = ReachCenter.shared
            profile.grants = reach.profileGrants(groupId: groupId, oneToOne: oneToOne, contactGrant: contactGrant)
            if !oneToOne, !reach.allowsGroup(groupId) {
                profile.username = nil
                profile.usernameCertificate = nil
            }
        }
        do {
            return try composeOwnEnvelope(profile, accountIdB64: accountIdB64)
        } catch {
            // A stored name that no longer passes the core rule (or a username
            // that does not) is not published half-way: the envelope is
            // refused as a whole, exactly as a receiver would refuse it.
            NSLog("ADR-0048 own profile envelope refused: \(error)")
            return nil
        }
    }

    /// The own profile as the envelope announced inside a link call
    /// (`docs/ux/calls.md`, "Every tile carries the person's name"), or `nil`
    /// without an active account.
    ///
    /// ``ownProfileForRequest()`` with one difference: the username travels
    /// even while this device holds no certificate for it. A call tile shows
    /// an uncertified username as plain text, which is still the person's
    /// name, and an account that set no display name would otherwise announce
    /// nothing to name its tile by. No grants: a call is not a conversation.
    func ownProfileEnvelopeForCall() -> Data? {
        guard let accountIdB64, var profile = ownProfileForRequest() else { return nil }
        if profile.username == nil { profile.username = ownUsername }
        do {
            return try composeOwnEnvelope(profile, accountIdB64: accountIdB64)
        } catch {
            NSLog("ADR-0048 own profile call envelope refused: \(error)")
            return nil
        }
    }

    /// The own profile for a contact request, or `nil` without an account.
    ///
    /// Carries the account's 16 raw bytes (profile envelope v2), the account
    /// a username certificate names. The id is already public to every peer
    /// that holds the identity bundle; here it only travels inside the E2EE
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
            grants: [],
            avatar: ownProfile?.avatar?.ffi,
            cardShareable: ownCardShareable
        )
    }

    // MARK: - Sharing the own contact card (ADR-0092 section 7)

    /// Whether this account lets others share its contact card: off exactly
    /// when its "who can find me" setting is `Nobody`. Every own profile
    /// envelope carries it.
    var ownCardShareable: Bool {
        Self.cardShareable(for: ReachCenter.shared.setting)
    }

    /// The one rule: a card may be shared unless the setting is `Nobody`.
    nonisolated static func cardShareable(for setting: FfiVisibilitySetting) -> Bool {
        setting.mode != .nobody
    }

    /// The setting moved to or from `Nobody` since the stored own profile was
    /// stamped: give it a new stamp (name and photo unchanged) so receivers,
    /// which keep a profile only when it is strictly newer, take the new
    /// flag; and hand it to the account's other devices. Returns whether it
    /// did. Called before the profiles are republished after a setting
    /// change, and at session start (a setting changed in an older build).
    @discardableResult
    func restampIfCardShareableChanged() -> Bool {
        guard let accountIdB64 else { return false }
        let current = ownCardShareable
        let stored = ownProfile ?? ownStore.load(accountIdB64: accountIdB64)
        guard (stored?.cardShareable ?? true) != current else { return false }
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        let saved = ownStore.save(
            displayName: stored?.displayName, avatar: stored?.avatar, accountIdB64: accountIdB64,
            nowMs: nowMs, cardShareable: current)
        AutoBackupStore.markDirty(accountIdB64: accountIdB64, nowMs: Int64(nowMs))
        gate.leave()
        ownProfile = saved
        bump()
        if let envelope = ownDeviceEnvelope() {
            onOwnDeviceEnvelope?(envelope)
        }
        return true
    }

    /// The person holding `signatureKeyB64` lets others share their contact
    /// card, by the newest profile this device holds from them. `true` when
    /// nothing is held (ADR-0092 section 7: a person who never sent a profile
    /// counts as shareable).
    func cardShareable(signatureKeyB64: String) -> Bool {
        storedProfile(signatureKeyB64: signatureKeyB64)?.cardShareable ?? true
    }

    // MARK: - Received profiles

    /// Offer a profile received from someone else, bound to the signature key
    /// the carrier authenticated (C-68). Kept only when strictly newer.
    ///
    /// `accountIdB64` is the account whose work received it (the poll, the
    /// call, the channel read that carried it), captured when that work
    /// began. Kept only while it is the active account: a late profile from
    /// account A's work, arriving after B signed in, is not filed under B
    /// (``ProfileAcceptance/inactive``).
    @discardableResult
    func acceptReceived(
        _ claim: ProfileClaim, signatureKeyB64: String?, accountIdB64 receivedFor: String?
    ) -> ProfileAcceptance {
        guard let receivedFor, receivedFor == accountIdB64 else { return .inactive }
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

    /// `docs/ux/who-can-find-me.md` section 6.1 — file the name a person was
    /// found under for the key that just became a contact, so the new row is
    /// never a stand-in. Any profile of their own replaces it. Kept like a
    /// received profile, and only while `receivedFor` is the active account.
    ///
    /// A username here was shown by the search, which verified it: its verdict
    /// is written with it. The check a stored username otherwise gets reads
    /// the certificate a profile carried, and this claim has none, so it would
    /// refuse the pair for good.
    @discardableResult
    func fileFoundName(
        _ claim: ProfileClaim, signatureKeyB64: String, accountIdB64 receivedFor: String?
    ) -> ProfileAcceptance {
        guard let receivedFor, receivedFor == accountIdB64 else { return .inactive }
        let outcome = received.accept(claim, signatureKeyB64: signatureKeyB64)
        guard outcome == .stored else { return outcome }
        if let username = claim.username {
            verifier.noteVerified(signatureKeyB64: signatureKeyB64, username: username)
        }
        bump()
        return outcome
    }

    /// Whether the person still counts as unnamed for the profile request
    /// (ADR-0048 section 2, trigger 4): the placeholder, or a name this device
    /// found for them, which stands in for a profile it has not got. Counting
    /// that one as a name would stop the asking that brings the real one.
    func countsAsUnnamed(signatureKey: Data) -> Bool {
        let resolved = resolve(signatureKey: signatureKey)
        if resolved.isPlaceholder { return true }
        return resolved.source != .localRename
            && profileKey(for: base64Encode(signatureKey))
                .flatMap { received.profile(signatureKeyB64: $0) }?.isFoundName == true
    }

    /// ADR-0050 section 2.9, bundle v3 — the profiles a pairing state bundle
    /// carries to a new device of `accountIdB64`: every received profile kept
    /// for that account (the caller leaves out the account's own keys).
    func profilesForBundle(accountIdB64: String) -> [String: ProfileClaim] {
        received.all(accountIdB64: accountIdB64).compactMapValues(\.forStateBundle)
    }

    /// ADR-0050 section 2.9, bundle v3 — store the profiles a pairing state
    /// bundle carried, each under its person key, through the store a
    /// received profile lands in (strictly newer wins: a newer profile held
    /// here is kept). Works whether or not `accountIdB64` is the active
    /// account (a join by name stores before its session starts). Returns how
    /// many were stored.
    @discardableResult
    func adoptCarriedProfiles(
        _ claims: [(personKeyB64: String, claim: ProfileClaim)], accountIdB64: String
    ) -> Int {
        var stored = 0
        for entry in claims {
            let outcome = received.accept(
                entry.claim.withoutGrants, signatureKeyB64: entry.personKeyB64, intoAccount: accountIdB64)
            guard outcome == .stored else { continue }
            stored += 1
            if accountIdB64 == self.accountIdB64, let username = entry.claim.username {
                startVerification(signatureKeyB64: entry.personKeyB64, username: username)
            }
        }
        if stored > 0, accountIdB64 == self.accountIdB64 { bump() }
        return stored
    }

    /// Every received profile, by signature key (b64): what local people
    /// search reads (ADR-0060 section 3).
    func receivedProfiles() -> [String: ProfileClaim] {
        received.all()
    }

    /// Every received profile kept for `accountIdB64`, active or not (a
    /// backup can be built before the session lands).
    func receivedProfiles(accountIdB64: String) -> [String: ProfileClaim] {
        received.all(accountIdB64: accountIdB64)
    }

    func storedProfile(signatureKeyB64: String) -> ProfileClaim? {
        received.profile(signatureKeyB64: signatureKeyB64)
    }

    /// Every received profile that names an account id, as
    /// `(signature key, account id)`: the account id a one-to-one peer's
    /// contact row carries.
    func profilesWithAccountId() -> [(signatureKeyB64: String, accountId: Data)] {
        received.profilesWithAccountId()
    }

    // MARK: - Local renames

    /// Refresh the rename map from the accepted contacts list.
    func setRenames(from accepted: [AcceptedContact]) {
        var next: [String: String] = [:]
        var nextAliases: [String: String] = [:]
        for contact in accepted {
            // ADR-0073 R.2 "the re-pin" — the keys an accepted reset retired
            // are DISPLAY aliases for stored history only (C-68: an author is
            // a key resolved at receipt, so an old bubble is attributed
            // through the alias). Never accepted for new traffic.
            for previous in contact.previousKeysB64 ?? [] {
                nextAliases[previous] = base64Encode(contact.request.fromPubkey)
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

    /// ADR-0073 R.2 — `retired key -> current key` for every contact whose
    /// reset this device accepted; see ``setRenames(from:)``.
    private var aliases: [String: String] = [:]

    /// The key whose received profile names `keyB64`
    /// (``profileKey(for:aliases:stampOf:)`` over this device's aliases and
    /// received profiles), or `nil`.
    private func profileKey(for keyB64: String) -> String? {
        Self.profileKey(for: keyB64, aliases: aliases) { [received] in
            received.profile(signatureKeyB64: $0)?.updatedAtMs
        }
    }

    /// ADR-0073 R.2 / UX `identity-recovery.md` 16.3 — under which key the
    /// profile that names a person is filed. iOS mirror of web
    /// `profile/profile-key.ts`.
    ///
    /// A contact's accepted key change (a reset, or a proven rotation)
    /// re-keys their contact row; the retired keys stay as aliases of the row
    /// key (`retired key -> current key`). A profile is filed under the key
    /// that SENT it, so after the change the person's only profile is, for a
    /// while, the one their old key sent: their new identity has not sent one
    /// yet (a reset device knows nobody until a conversation reaches it).
    ///
    /// The lookup therefore goes both ways along the alias:
    ///
    /// | asked about          | order tried                                              |
    /// |----------------------|----------------------------------------------------------|
    /// | the current key      | that key, then the retired keys of the row, newest first |
    /// | a retired key        | that key, then the current key, then its siblings        |
    /// | a key with no alias  | that key                                                 |
    ///
    /// "Newest" is the profile's own stamp. Before this, a roster naming the
    /// new key (the successor of a one-to-one) read `Member` while the
    /// person's name was held under the key they had a minute earlier (seen
    /// live 2026-11-25). Only keys this device accepted as the same person
    /// are ever aliased, so this names nobody new. `stampOf` answers the held
    /// profile's stamp, `nil` when none is held for that key.
    nonisolated static func profileKey(
        for keyB64: String, aliases: [String: String], stampOf: (String) -> UInt64?
    ) -> String? {
        if stampOf(keyB64) != nil { return keyB64 }
        let canonical = aliases[keyB64] ?? keyB64
        if canonical != keyB64, stampOf(canonical) != nil { return canonical }
        var best: String?
        var bestStamp: UInt64?
        // Sorted, so two equal stamps pick the same key on every run.
        for (retired, current) in aliases.sorted(by: { $0.key < $1.key })
        where current == canonical && retired != keyB64 {
            guard let stamp = stampOf(retired) else { continue }
            if bestStamp.map({ stamp > $0 }) ?? true {
                best = retired
                bestStamp = stamp
            }
        }
        return best
    }

    // MARK: - Pending contacts (ADR-0079)

    /// base64 account key -> what a pending contact's invite link stated about
    /// them, as a profile claim (stamp 0). The link statement is the
    /// inviter's own, inside the capability their account key signed, so it
    /// ranks with a received profile; a received profile still wins because
    /// it is the person's current statement.
    private var pendingClaims: [String: ProfileClaim] = [:]

    /// Refresh the pending-contact names from this account's pending contacts
    /// (added, removed, or handed over to a real contact).
    func setPendingContacts(_ pending: [PendingContact]) {
        var next: [String: ProfileClaim] = [:]
        for contact in pending {
            guard let claim = Self.pendingClaim(contact) else { continue }
            next[base64Encode(contact.signaturePubkey)] = claim
        }
        guard next != pendingClaims else { return }
        pendingClaims = next
        bump()
    }

    /// What a pending contact's link states, as a claim the resolver reads
    /// like a profile, or `nil` when it states neither a name nor a username.
    /// The username stays a claim: it is shown only with a verdict.
    static func pendingClaim(_ contact: PendingContact) -> ProfileClaim? {
        let name = contact.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        let username = contact.username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty || !username.isEmpty else { return nil }
        return ProfileClaim(
            displayName: name.isEmpty ? nil : name,
            username: username.isEmpty ? nil : username,
            updatedAtMs: 0)
    }

    // MARK: - Resolving

    /// The resolved name for the person holding `signatureKey`. Starts a
    /// username verification when the stored claim has no verdict yet.
    public func resolve(signatureKey: Data) -> ResolvedDisplayName {
        resolve(signatureKey: signatureKey, askedKeyB64: base64Encode(signatureKey))
    }

    /// ``resolve(signatureKey:)`` with the key first asked about kept: the
    /// profile is looked up along ITS alias (``profileKey(for:)``).
    private func resolve(signatureKey: Data, askedKeyB64: String) -> ResolvedDisplayName {
        let keyB64 = base64Encode(signatureKey)
        // A key an accepted reset retired resolves as the contact it
        // belonged to, under the contact's CURRENT key: the same person, the
        // same rename, for the history written under the old key.
        if let current = aliases[keyB64], current != keyB64, let currentKey = Data(base64Encoded: current) {
            return resolve(signatureKey: currentKey, askedKeyB64: askedKeyB64)
        }
        // ADR-0074 section 3.5 — a key nothing is known under that is the
        // same person as a known key across a PROVEN rotation resolves as
        // that person. Nothing is shown about the rotation itself.
        if let alias = provenAlias(of: keyB64), let aliasKey = Data(base64Encoded: alias) {
            return resolve(signatureKey: aliasKey)
        }
        // ADR-0098 — a deleted account replaces every name, the viewer's own
        // rename included, and shows no username; a username is otherwise
        // shown only while the account was recently confirmed to exist.
        var usernameAllowed = true
        if let deletions {
            let keys = personKeys(keyB64)
            if deletions.isDeleted(keysB64: keys) {
                return ResolvedDisplayName.deletedAccount(signatureKey: signatureKey)
            }
            usernameAllowed = deletions.mayShowUsername(keysB64: keys)
        }
        // Precedence: local rename > received profile > a pending contact's
        // link statement (ADR-0079) > the placeholder.
        // The profile along the asked key's alias (ADR-0073 R.2, UX 16.3):
        // the key itself, then the current key, then the newest profile a
        // retired key of the same person sent. A username is verified under
        // the key whose profile claims it.
        let profileKeyB64 = profileKey(for: askedKeyB64)
        let receivedProfile = profileKeyB64.flatMap { received.profile(signatureKeyB64: $0) }
        let profile = receivedProfile ?? pendingClaims[keyB64]
        let claimKeyB64 = receivedProfile != nil ? (profileKeyB64 ?? keyB64) : keyB64
        var state: UsernameVerificationState?
        if let username = profile?.username {
            state = verifier.state(signatureKeyB64: claimKeyB64, username: username)
            // A verification is started for a received profile only. The
            // check (ADR-0060 section 5) reads the certificate the received
            // profile carried; a link carries none, so starting one for a
            // pending contact's username would record a refusal for the pair
            // that a later profile with a certificate could not undo. A link
            // username is therefore shown only with a verdict already held.
            if receivedProfile != nil,
               verifier.needsVerification(signatureKeyB64: claimKeyB64, username: username) {
                startVerification(signatureKeyB64: claimKeyB64, username: username)
            }
        }
        return resolveDisplayName(
            signatureKey: signatureKey,
            localRename: renames[keyB64],
            profile: profile,
            verification: usernameAllowed ? UsernameVerificationStateView(state) : nil
        )
    }

    /// Every key the person holding `keyB64` is known by on this device:
    /// itself, the keys of a proven rotation, and the keys an accepted reset
    /// retired. A statement under any of them is about this person.
    func personKeys(_ keyB64: String) -> [String] {
        var keys = [keyB64]
        for other in provenPredecessors.allKeys(of: keyB64) where !keys.contains(other) {
            keys.append(other)
        }
        for (retired, current) in aliases where current == keyB64 && !keys.contains(retired) {
            keys.append(retired)
        }
        return keys
    }

    /// ADR-0098 — whether the person holding `signatureKey` is a deleted
    /// account here (over every key they are known by).
    public func isDeletedAccount(signatureKey: Data) -> Bool {
        let keyB64 = base64Encode(signatureKey)
        if let current = aliases[keyB64], current != keyB64, let currentKey = Data(base64Encoded: current) {
            return isDeletedAccount(signatureKey: currentKey)
        }
        return deletions?.isDeleted(keysB64: personKeys(keyB64)) ?? false
    }

    /// ADR-0098 — whether what was held for the person holding `signatureKey`
    /// (unsent messages, an unanswered request) may be dropped: only on a
    /// verified statement AND a directory `404` after it (`deletionEffects`).
    public func deletionDropsHeldSends(signatureKey: Data) -> Bool {
        let keyB64 = base64Encode(signatureKey)
        if let current = aliases[keyB64], current != keyB64, let currentKey = Data(base64Encoded: current) {
            return deletionDropsHeldSends(signatureKey: currentKey)
        }
        return deletions?.effects(keysB64: personKeys(keyB64)).dropHeldSends ?? false
    }

    /// ADR-0084 — the profile photo the person holding `signatureKey` set,
    /// through the same aliases a name resolves through, or `nil`.
    public func avatar(signatureKey: Data) -> ProfileAvatar? {
        // ADR-0098 — a deleted account shows no photo.
        if isDeletedAccount(signatureKey: signatureKey) { return nil }
        let keyB64 = base64Encode(signatureKey)
        // The same lookup the name uses (``profileKey(for:)``).
        if let profileKeyB64 = profileKey(for: keyB64) {
            return received.profile(signatureKeyB64: profileKeyB64)?.avatar
        }
        let canonical = aliases[keyB64] ?? keyB64
        if let alias = provenAlias(of: canonical), let aliasKey = Data(base64Encoded: alias) {
            return avatar(signatureKey: aliasKey)
        }
        return nil
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

    /// ADR-0087 section 3 — the handle key a group or channel `@name`
    /// certificate binds to the name whose lookup key is `lookupKey`, or
    /// `nil` when the certificate does not verify under discovery's key.
    func handleWriteKey(certificate: Data, lookupKey: Data) async -> Data? {
        guard let key = try? await usernameCertKey(),
              let cert = try? verifyHandleBinding(
                  certificate: certificate, verifyingKey: key, expectedLookupKey: lookupKey)
        else { return nil }
        return cert.writePk
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

/// ADR-0101 section 8 — why an own profile envelope could not be composed.
enum ProfileSigningError: Error {
    /// No account identity is held for the account (profile v6 is signed).
    case noAccountIdentity
}
