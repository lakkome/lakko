// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 sections 1 to 3 — the I/O of the reset side's announcement and of
// the previous own key (``ResetContinuity`` holds the record and the rules).
// iOS mirror of `announceContinuity` / `readPreviousOwnKey` in
// `apps/web/src/auth/AuthContext.tsx`.

enum ResetAnnouncement {
    /// The account key of before a reset: the key the directory serves for
    /// the account now, confirmed by the KT log
    /// (``ResetContinuityRules/previousKeyStanding(servedKeyB64:logStatus:newKeyB64:)``).
    /// Read BEFORE the reset is sent (the log serves only the latest entry).
    /// `nil` when it cannot be established. Never throws.
    static func readPreviousOwnKey(
        accountId: Data, newKeyB64: String,
        fetchBundle: (Data) async throws -> IdentityFetchResponse?,
        transparency: TransparencyVerify = TransparencyVerify()
    ) async -> String? {
        guard let bundle = try? await fetchBundle(accountId) else { return nil }
        let served = bundle.signaturePubkey
        var standing: ResetContinuityRules.LogStanding?
        if let key = base64Decode(served) {
            let verdict = await transparency.verifyIdentityInLog(
                accountId: accountId, publishedSignaturePubkey: key)
            switch verdict.status {
            case .confirmed: standing = .confirmed
            case .off: standing = .off
            default: standing = .other
            }
        }
        return ResetContinuityRules.previousKeyStanding(
            servedKeyB64: served, logStatus: standing, newKeyB64: newKeyB64)
    }

    /// The account key a 32-byte account Ed25519 seed signs with (a recall
    /// knows the previous key outright: it held the old seed), or `nil`.
    static func previousKey(ofEd25519Seed seed: Data) -> String? {
        (try? reachVerifyingKey(seed: seed)).map { base64Encode($0) }
    }

    /// The announcement a reset owes once its keys are published: claim the
    /// continuity mailbox, register on it the continuity reach key alone,
    /// and upload the binding. Each part is idempotent. Throws what failed;
    /// the record stays owed. Does nothing when nothing is owed.
    static func announce(
        accountIdB64: String, accountId: Data, accountIdentitySeed: Data, sessionToken: String,
        store: ResetContinuityStore = ResetContinuityStore(),
        ds: DsClient = DsClient(),
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) async throws {
        guard let record = store.read(accountIdB64: accountIdB64),
              ResetContinuityRules.announcementOwed(record, nowMs: nowMs) else { return }
        let identity = try Identity.fromSeed(seed: accountIdentitySeed)
        let signer = dsContinuityMailboxSigner(accountIdentity: identity)
        let address = signer.mailboxAddress()
        try await ds.claimMailbox(pseudonym: address, bodyJson: signer.mailboxClaimBodyJson())
        let reachPk = try reachVerifyingKey(
            seed: try continuityReachSeed(accountId: accountId, keyGeneration: record.keyGeneration))
        try await ds.updateReach(
            pseudonym: address,
            add: [DsReachKeyAdd(publicKey: reachPk, expiresAtMs: UInt64(record.expiresAtMs))],
            remove: [], open: nil, signer: signer)
        let binding = signContinuityBinding(accountIdentity: identity, expiresAtMs: UInt64(record.expiresAtMs))
        try await DiscoveryClient(sessionToken: sessionToken).putContinuityBinding(binding)
        store.markAnnounced(accountIdB64: accountIdB64)
        NSLog("[auth] the continuity mailbox of the key reset is announced")
    }
}
