// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 4 — the PEER side's write to a reset person's continuity
// mailbox, and the fetch that keeps the verified target. iOS mirror of
// `continuityCandidate` / `continuityFor` (`apps/web/src/reach/mailbox-write.ts`)
// and `refreshContinuityTarget` (`apps/web/src/reach/continuity-target.ts`).

/// The accepted reset behind a NEW account key, as the pin store reads it.
struct AcceptedContinuity: Equatable {
    let accountIdB64: String
    let accountId: Data
    let keyGeneration: UInt64
    let signedAtMs: Int64
}

extension ReachCenter {
    /// The accepted reset whose new (pinned) key is `newKey`, only while its
    /// continuity key is usable (counted from the attestation's `signed_at`
    /// this device verified). `nil` otherwise.
    ///
    /// ADR-0101 sections 10-11 (2026-11-28): only a SEALED reset in force
    /// counts (``PeerPinStore/sealedResetInForce(_:)``). After an unsealed
    /// reset nothing is written to the account's continuity mailbox, also
    /// once its new keys were accepted as a new contact.
    func acceptedContinuity(ofNewKey newKey: Data, nowMs: Int64, pins: PeerPinStore = PeerPinStore())
        -> AcceptedContinuity? {
        let keyB64 = base64Encode(newKey)
        for (accountIdB64, pin) in pins.allPins() where pin.pinnedSignaturePubkeyB64 == keyB64 {
            guard let sealed = PeerPinStore.sealedResetInForce(pin),
                  ContinuityTargets.continuityKeyUsable(signedAtMs: sealed.signedAtMs, nowMs: nowMs),
                  let accountId = base64Decode(accountIdB64) else { continue }
            return AcceptedContinuity(
                accountIdB64: accountIdB64, accountId: accountId,
                keyGeneration: sealed.keyGeneration, signedAtMs: sealed.signedAtMs)
        }
        return nil
    }

    /// The write to the continuity mailbox of the person whose CURRENT key
    /// is `person`, signed with the continuity reach key of the accepted
    /// reset, or `nil` when this device holds no live, verified continuity
    /// mailbox for that key (or the key's window ended). The target was
    /// verified under the attested new key when it was kept.
    func continuityPlan(toPersonKey person: Data, nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000))
        -> WritePlan? {
        guard let own = boundAccountIdB64,
              let accepted = acceptedContinuity(ofNewKey: person, nowMs: nowMs),
              let held = ContinuityTargetStore(accountIdB64: own).target(of: base64Encode(person), nowMs: nowMs),
              let mailboxPk = base64Decode(held.mailboxPkB64),
              let address = try? mailboxAddress(mailboxPk: mailboxPk),
              let seed = try? continuityReachSeed(accountId: accepted.accountId, keyGeneration: accepted.keyGeneration)
        else { return nil }
        return WritePlan(
            target: MailboxTarget(mailboxPk: mailboxPk, address: address, binding: Data(), accountKey: person),
            candidates: [DsMailboxWriteAuth(reachSeed: seed)])
    }

    /// The continuity plan alone (a channel return goes ONLY there: the real
    /// mailbox drops one).
    func continuityOnlyPlan(toPersonKey person: Data) -> WritePlan? {
        continuityPlan(toPersonKey: person)
    }
}

enum ContinuityTargetRefresh {
    /// Fetch a reset person's identity bundle and keep the continuity
    /// mailbox its binding names, verified under the key this device
    /// ACCEPTED for the account. A failed fetch keeps what is held; a bundle
    /// naming another key forgets it. `nil` when there is no live target.
    @MainActor
    static func refresh(
        accountIdB64: String, attestedKeyB64: String, ownAccountIdB64: String,
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000),
        discovery: DiscoveryClient = DiscoveryClient()
    ) async -> ContinuityTarget? {
        let store = ContinuityTargetStore(accountIdB64: ownAccountIdB64)
        guard let accountId = base64Decode(accountIdB64), let attested = base64Decode(attestedKeyB64) else {
            return nil
        }
        let bundle: IdentityFetchResponse?
        do {
            bundle = try await discovery.identityFetch(accountId: accountId)
        } catch {
            return store.target(of: attestedKeyB64, nowMs: nowMs)
        }
        guard let bundle, bundle.signaturePubkey == attestedKeyB64 else {
            store.remember(newKeyB64: attestedKeyB64, target: nil, nowMs: nowMs)
            return nil
        }
        let target = ContinuityTargets.peerTarget(
            bindingB64: bundle.continuityBindingB64, attestedKey: attested, nowMs: nowMs,
            verify: { try verifyContinuityBinding(binding: $0, identityPk: $1, nowMs: $2) })
        store.remember(newKeyB64: attestedKeyB64, target: target, nowMs: nowMs)
        return target
    }
}
