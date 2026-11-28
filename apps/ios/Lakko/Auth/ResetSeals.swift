// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 10 — the account's seal announcement and the reset seal,
// on this device.
//
// A contact's device moves a conversation to an account's new keys after a
// key reset only when the reset is SEALED: signed by a seal key the account
// announced, under its old account key, before the reset. A seal key comes
// from a secret the server never holds: the password (its OPAQUE export
// key), a passkey's own secret (its WebAuthn `prf` output at a salt of its
// own), or the recovery phrase.
//
// This file is:
//
//   * `SealAnnouncementPlan`   the pure rule: which entries the next
//                              announcement lists, and whether one is owed;
//   * `AccountSeals`           what the account's current announcement says
//                              (a saved phrase, the passkeys that keep the
//                              chats), and the per-device record of it;
//   * `ResetSealPreview`       whether a reset made now would be sealed
//                              (UX 15.1c), decided on the device;
//   * `SealAnnouncer`          the I/O: read the account's own bundle, sign,
//                              publish.
//
// The codecs, the key derivations and the checks are the core's
// (`lakko_attest::reset_seal`); nothing here re-implements them.

// MARK: - What changed

/// Why an announcement is being looked at.
enum SealChange: Equatable {
    /// A session landing on a device that holds the account keys: publish
    /// only when the account has none at this generation. (The landing
    /// after a key reset is this case: the new generation has none, the
    /// passkeys were deleted, and the new phrase is not saved.)
    case landing
    /// The password was proven on THIS device just now (sign-up, a password
    /// sign-in, a password change): its seal replaces the announced one.
    case password
    /// A passkey whose authenticator gave its secret was added.
    case passkeyAdded(credentialId: Data, prfOutput: Data)
    /// A passkey was removed.
    case passkeyRemoved(credentialId: Data)
    /// The recovery phrase was saved (the three-word check passed).
    case phraseSaved
    /// The account's keys were ROTATED on this device (ADR-0074): a first
    /// announcement at the new generation. The password and the passkeys
    /// are the ones of before, so their seals are carried; the phrase is a
    /// new one and is listed only once it is saved.
    case rotated
}

// MARK: - The pure rule

enum SealAnnouncementPlan {

    enum Outcome: Equatable {
        /// The announced entries already say this: nothing to publish.
        case nothing
        /// Publish these entries at `seq`.
        case publish(entries: [FfiSealEntry], seq: UInt64)
        /// No password seal can be made: this device holds no export key and
        /// the account announced none at this generation. Nothing is
        /// published (an announcement lists exactly one password entry).
        case noPasswordSeal
    }

    /// At most this many passkey entries (`MAX_PASSKEY_SEALS`).
    static let maxPasskeySeals = 8

    /// - Parameters:
    ///   - current: the account's announcement at THIS generation, verified
    ///     under its own key; `nil` when it has none.
    ///   - highestSeq: the highest `seq` seen for the account, over every
    ///     generation (0 when none).
    ///   - carried: entries to start from when there is no announcement at
    ///     this generation (the previous generation's password and passkey
    ///     entries after a rotation, which keeps both; none after a reset,
    ///     which deletes the passkeys and is made with a password proven
    ///     now).
    ///   - passwordEntry: the entry of the export key this device holds.
    ///   - phraseEntry: the entry of the phrase this device holds.
    static func decide(
        current: FfiSealAnnouncement?,
        highestSeq: UInt64,
        carried: [FfiSealEntry] = [],
        passwordEntry: FfiSealEntry?,
        phraseEntry: FfiSealEntry?,
        change: SealChange
    ) -> Outcome {
        var entries = current?.entries ?? carried

        // The password entry: exactly one. A password proven here replaces
        // the announced one; otherwise the announced one stands, and only an
        // account with none takes this device's.
        let announcedPassword = entries.first { $0.kind == .password }
        let password: FfiSealEntry?
        switch change {
        case .password: password = passwordEntry ?? announcedPassword
        default: password = announcedPassword ?? passwordEntry
        }
        guard let password else { return .noPasswordSeal }
        entries.removeAll { $0.kind == .password }
        entries.append(password)

        switch change {
        case .passkeyAdded(let credentialId, let prfOutput):
            let tag = resetSealPasskeyTag(credentialId: credentialId)
            if let entry = try? ResetSealKey.fromPasskeyPrf(prfOutput: prfOutput)
                .announcementEntry(credentialId: credentialId) {
                entries.removeAll { $0.kind == .passkey && $0.tag == tag }
                if entries.filter({ $0.kind == .passkey }).count < maxPasskeySeals {
                    entries.append(entry)
                }
            }
        case .passkeyRemoved(let credentialId):
            let tag = resetSealPasskeyTag(credentialId: credentialId)
            entries.removeAll { $0.kind == .passkey && $0.tag == tag }
        case .phraseSaved:
            if let phraseEntry {
                entries.removeAll { $0.kind == .phrase }
                entries.append(phraseEntry)
            }
        case .rotated:
            // The phrase of before the change seals nothing any more; the
            // new one is listed only once it is saved.
            entries.removeAll { $0.kind == .phrase }
        case .landing, .password:
            break
        }

        if let current, sameEntries(current.entries, entries) { return .nothing }
        if current != nil, change == .landing { return .nothing }
        return .publish(entries: entries, seq: highestSeq &+ 1)
    }

    /// Two entry lists that name the same seals, in any order.
    static func sameEntries(_ a: [FfiSealEntry], _ b: [FfiSealEntry]) -> Bool {
        a.count == b.count && Set(a) == Set(b)
    }
}

// MARK: - What the announcement says about the account

/// What the account's published protection lists (UX 19.1c, 19.1d).
public struct AccountSeals: Codable, Equatable, Sendable {
    /// A phrase seal is listed: the phrase was saved.
    public var phraseSaved: Bool
    /// The tags of the passkeys that keep the chats, standard base64.
    public var passkeyTagsB64: [String]

    /// An announcement that lists the password alone. (Not named `none`:
    /// on an `AccountSeals?` that would read as "no value".)
    public static let passwordOnly = AccountSeals(phraseSaved: false, passkeyTagsB64: [])

    public init(phraseSaved: Bool, passkeyTagsB64: [String]) {
        self.phraseSaved = phraseSaved
        self.passkeyTagsB64 = passkeyTagsB64
    }

    init(_ entries: [FfiSealEntry]) {
        phraseSaved = entries.contains { $0.kind == .phrase }
        passkeyTagsB64 = entries.filter { $0.kind == .passkey }.map { $0.tag.base64EncodedString() }.sorted()
    }

    /// What keeps the chats after a reset (UX 19.1c status line).
    public enum KeptBy: Equatable, Sendable {
        case phrase
        case passkey
        case passwordOnly
    }

    public var keptBy: KeptBy {
        if phraseSaved { return .phrase }
        return passkeyTagsB64.isEmpty ? .passwordOnly : .passkey
    }

    /// Whether the passkey with this credential id keeps the chats.
    public func passkeyKeepsChats(credentialId: Data) -> Bool {
        passkeyTagsB64.contains(resetSealPasskeyTag(credentialId: credentialId).base64EncodedString())
    }
}

/// The per-device record of ``AccountSeals``, in `UserDefaults`: public
/// facts of the account's own announcement, never a secret. Written at
/// every look at the account's own protection; cleared by the account wipe.
struct AccountSealsStore {
    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        AccountProtectionStore.key("seals", accountIdB64: accountIdB64)
    }

    static func noticeKey(accountIdB64: String) -> String {
        AccountProtectionStore.key("password-only-notice", accountIdB64: accountIdB64)
    }

    /// `nil` until this device read the account's announcement once.
    func read(accountIdB64: String) -> AccountSeals? {
        defaults.data(forKey: Self.key(accountIdB64: accountIdB64))
            .flatMap { try? JSONDecoder().decode(AccountSeals.self, from: $0) }
    }

    func write(_ seals: AccountSeals, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(seals) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
        NotificationCenter.default.post(name: Self.changed, object: nil)
    }

    /// Posted after the record was written: a screen that draws from it
    /// (a passkey's "Does not keep your chats." line) reads it again.
    static let changed = Notification.Name("me.lakko.account-seals.changed")

    /// UX 19.1d place 3: the once-per-device dialog was answered `Not now`
    /// (or acted on) and is not shown here again.
    func passwordOnlyNoticeClosed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.noticeKey(accountIdB64: accountIdB64))
    }

    func closePasswordOnlyNotice(accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(true, forKey: Self.noticeKey(accountIdB64: accountIdB64))
    }

    /// The new words of a reset or a rotation were confirmed on this device
    /// and the account's protection does not list them yet: the next look
    /// publishes the phrase seal.
    func phraseSealOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }

    func setPhraseSealOwed(_ owed: Bool, accountIdB64: String) {
        guard owed else {
            defaults.removeObject(forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
            return
        }
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(true, forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }

    static func phraseOwedKey(accountIdB64: String) -> String {
        AccountProtectionStore.key("phrase-seal-owed", accountIdB64: accountIdB64)
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
        defaults.removeObject(forKey: Self.noticeKey(accountIdB64: accountIdB64))
        defaults.removeObject(forKey: Self.phraseOwedKey(accountIdB64: accountIdB64))
    }
}

// MARK: - Will a reset made now be sealed?

/// UX 15.1c — decided on the device before the person types the word: does
/// what was just proven (the password typed, the passkey used, a phrase
/// typed) match a seal the account published?
enum ResetSealPreview {

    /// One thing the resetting device holds that could sign the seal.
    struct Candidate {
        let key: ResetSealKey
        /// The passkey's credential id, for a passkey key.
        let credentialId: Data?
    }

    struct Sealed {
        let key: ResetSealKey
        /// The `seq` of the announcement that lists it.
        let announcementSeq: UInt64
        /// The generation the announcement is at: the one the reset leaves.
        let fromGeneration: UInt64
    }

    /// The first candidate the announcement lists under its kind, or `nil`:
    /// the reset would be unsealed.
    static func sealed(announcement: FfiSealAnnouncement?, candidates: [Candidate]) -> Sealed? {
        guard let announcement else { return nil }
        for candidate in candidates {
            let kind = candidate.key.kind()
            let publicKey = candidate.key.publicKey()
            let listed = announcement.entries.contains { entry in
                guard entry.kind == kind, entry.sealPk == publicKey else { return false }
                guard kind == .passkey else { return true }
                return candidate.credentialId.map { resetSealPasskeyTag(credentialId: $0) == entry.tag } ?? false
            }
            if listed {
                return Sealed(
                    key: candidate.key, announcementSeq: announcement.seq,
                    fromGeneration: announcement.keyGeneration)
            }
        }
        return nil
    }

    /// Whether the announcement lists a passkey that keeps the chats
    /// (UX 15.1c: "Use a passkey that keeps your chats to bring them back.").
    static func listsPasskey(_ announcement: FfiSealAnnouncement?) -> Bool {
        announcement?.entries.contains { $0.kind == .passkey } ?? false
    }
}

// MARK: - Reading and publishing

/// Reads the account's own announcement and publishes the next one.
@MainActor
struct SealAnnouncer {

    struct Ports {
        /// `GET /discovery/identity/{own id}`.
        var fetchOwnBundle: (_ accountId: Data, _ sessionToken: String) async throws -> IdentityFetchResponse?
        /// `POST /discovery/identity` with the same keys and the
        /// announcement.
        var publish: (_ accountId: Data, _ identity: Identity, _ announcement: Data, _ sessionToken: String)
            async throws -> Void
        var nowMs: () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

        static func live(directory: AccountDirectory = DiscoveryAccountDirectory()) -> Ports {
            Ports(
                fetchOwnBundle: { accountId, token in
                    try await DiscoveryClient(sessionToken: token).identityFetch(accountId: accountId)
                },
                publish: { accountId, identity, announcement, token in
                    try await directory.publishSealAnnouncement(
                        accountId: accountId,
                        signaturePubkey: identity.signaturePublicKey(),
                        x25519Pubkey: identity.x25519PublicKey(),
                        x25519Binding: signX25519Binding(accountIdentity: identity),
                        announcement: announcement,
                        sessionToken: token)
                })
        }
    }

    enum Result: Equatable {
        case published(AccountSeals)
        /// Nothing was owed; the account's seals as read.
        case unchanged(AccountSeals)
        /// Nothing could be read or published now; tried again at the next
        /// landing. The account's seals as this device last knew them.
        case failed
        /// This device cannot make a password seal and the account has none.
        case noPasswordSeal
    }

    let ports: Ports
    var store = AccountSealsStore()

    /// The announcement of `bundle` at its own generation, verified under
    /// its own key; `nil` when absent or not verifying.
    static func currentAnnouncement(_ bundle: IdentityFetchResponse?) -> FfiSealAnnouncement? {
        guard let bundle, let raw = bundle.resetSeals.flatMap({ Data(base64Encoded: $0) }),
              let key = Data(base64Encoded: bundle.signaturePubkey),
              let opened = try? openSealAnnouncement(announcement: raw, accountKey: key),
              opened.keyGeneration == bundle.keyGeneration else { return nil }
        return opened
    }

    /// The highest `seq` either announcement of `bundle` carries.
    static func highestSeq(_ bundle: IdentityFetchResponse?) -> UInt64 {
        [bundle?.resetSeals, bundle?.previousResetSeals]
            .compactMap { $0.flatMap { Data(base64Encoded: $0) } }
            .compactMap { try? decodeSealAnnouncement(announcement: $0).seq }
            .max() ?? 0
    }

    /// Look at the account's own announcement and publish the next one when
    /// `change` owes it. `identity` is the CURRENT account identity (it
    /// signs); `exportKey` the 64-byte export key this device holds, `phrase`
    /// its recovery phrase. Never throws: a failure is ``Result/failed``.
    func ensure(
        accountId: Data, accountIdB64: String, identity: Identity, keyGeneration: UInt64,
        sessionToken: String, exportKey: Data?, phrase: RecoveryMnemonic?, change: SealChange
    ) async -> Result {
        for attempt in 0..<2 {
            let bundle: IdentityFetchResponse?
            do {
                bundle = try await ports.fetchOwnBundle(accountId, sessionToken)
            } catch {
                return .failed
            }
            // The bundle must be this identity's: an announcement is signed
            // by the key the row holds.
            guard let bundle, Data(base64Encoded: bundle.signaturePubkey) == identity.signaturePublicKey(),
                  bundle.keyGeneration == keyGeneration else { return .failed }
            let current = Self.currentAnnouncement(bundle)
            var carried: [FfiSealEntry] = []
            // (A landing after a rotation whose own look did not land sees
            // the proof the rotation published, and carries the same.)
            let afterRotation = change == .rotated
                || ((change == .landing || change == .password) && bundle.supersessionProof != nil)
            if current == nil, afterRotation,
               let previous = bundle.previousResetSeals.flatMap({ Data(base64Encoded: $0) }),
               let decoded = try? decodeSealAnnouncement(announcement: previous) {
                // A rotation keeps the password and the passkeys, and so
                // their seals.
                carried = decoded.entries.filter { $0.kind == .password || $0.kind == .passkey }
            }
            let passwordEntry = exportKey.flatMap {
                try? ResetSealKey.fromPasswordExportKey(exportKey: $0).announcementEntry(credentialId: nil)
            }
            let phraseEntry = phrase.flatMap {
                try? ResetSealKey.fromMnemonic(mnemonic: $0).announcementEntry(credentialId: nil)
            }
            let outcome = SealAnnouncementPlan.decide(
                current: current, highestSeq: Self.highestSeq(bundle), carried: carried,
                passwordEntry: passwordEntry, phraseEntry: phraseEntry, change: change)
            switch outcome {
            case .noPasswordSeal:
                return .noPasswordSeal
            case .nothing:
                let seals = AccountSeals(current?.entries ?? [])
                store.write(seals, accountIdB64: accountIdB64)
                return .unchanged(seals)
            case .publish(let entries, let seq):
                do {
                    let announcement = try signSealAnnouncement(
                        accountIdentity: identity, accountId: accountId, keyGeneration: keyGeneration,
                        seq: seq, issuedAtMs: ports.nowMs(), entries: entries)
                    try await ports.publish(accountId, identity, announcement, sessionToken)
                } catch {
                    // `409 reset-seals-stale`: another device published
                    // meanwhile. Read again once.
                    if attempt == 0, SealUploadRefusal.isStale(error) { continue }
                    NSLog("[seals] the account's protection was not published (%@)",
                          String(describing: type(of: error)))
                    return .failed
                }
                let seals = AccountSeals(entries)
                store.write(seals, accountIdB64: accountIdB64)
                return .published(seals)
            }
        }
        return .failed
    }
}

/// The refusals discovery answers to an upload that carries an announcement
/// or a reset seal (ADR-0101 section 10, "As built in the services").
enum SealUploadRefusal {
    static func code(_ error: Error) -> String? {
        guard case .http(_, _, let body) = error as? DiscoveryClientError else { return nil }
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return json?["error"] as? String
    }

    /// `409 reset-seals-stale`.
    static func isStale(_ error: Error) -> Bool { code(error) == "reset-seals-stale" }
    /// `400 reset-seal-invalid`.
    static func isSealInvalid(_ error: Error) -> Bool { code(error) == "reset-seal-invalid" }
}
