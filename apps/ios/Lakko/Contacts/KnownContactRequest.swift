// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.7 case 1 / UX 16.6 - a contact request (or an `/i/` link) from an
// account that is ALREADY a contact.
//
// iOS mirror of `apps/web/src/contacts/known-contact-request.ts`; the tables
// are the same and so are the cases in `KnownContactRequestTests`.
//
// A person who reset their keys lost their contacts and may find the other
// person again before that person's device noticed anything. The device never
// shows a request for someone who is already a contact: it runs the R.2 check
// on the sender's account id and then answers the request itself (the
// conversation's successor is the answer) or drops it. Nothing is shown
// either way, and on a refusal nothing is sent to the keys that sent the
// request.
public enum KnownContactRequest {

    /// How the identity check of the sender's account ended.
    public enum Check: Equatable {
        /// The gate passed: the published key is what this device now trusts
        /// (the pin, a proven successor, or an attested reset just accepted).
        case current
        /// The gate refused the published key.
        case refused
        /// No verdict yet: the directory, the attest key or the key log could
        /// not be reached, or the account publishes nothing.
        case inconclusive
    }

    public enum Verdict: Equatable {
        /// The sender is not a contact: the ordinary request row.
        case show
        /// A contact, and the request is signed by the key this device trusts
        /// for the account: answered silently (ack; the conversation
        /// continues by itself). No request row.
        case answer
        /// A contact, and the request is NOT from the key this device trusts
        /// for the account (the check refused, or it is some other key):
        /// nothing is shown and nothing is sent.
        case drop
        /// A contact, and there is no verdict yet: the request waits,
        /// unshown, for the next check.
        case wait
    }

    /// | sender is a contact | check          | request key is the trusted key | verdict  |
    /// |---------------------|----------------|--------------------------------|----------|
    /// | no                  | -              | -                              | `show`   |
    /// | yes                 | `refused`      | -                              | `drop`   |
    /// | yes                 | `inconclusive` | -                              | `wait`   |
    /// | yes                 | `current`      | yes                            | `answer` |
    /// | yes                 | `current`      | no                             | `drop`   |
    ///
    /// `trustedKeyB64` is the pin AFTER the check ran; `nil` (no pin) cannot
    /// happen for a contact with a known account id once the check passed,
    /// and reads as "not the trusted key".
    public static func verdict(
        senderIsContact: Bool, check: Check, requestKeyB64: String, trustedKeyB64: String?
    ) -> Verdict {
        if !senderIsContact { return .show }
        switch check {
        case .refused: return .drop
        case .inconclusive: return .wait
        case .current: return trustedKeyB64 == requestKeyB64 ? .answer : .drop
        }
    }

    /// The slice of a contact row this reads.
    public struct ContactRow: Equatable {
        /// The row's own account key, standard base64.
        public let signaturePubkeyB64: String
        /// The account id the row carries, hex; `nil` when it has none.
        public let accountIdHex: String?

        public init(signaturePubkeyB64: String, accountIdHex: String?) {
            self.signaturePubkeyB64 = signaturePubkeyB64
            self.accountIdHex = accountIdHex
        }
    }

    /// What this device verified for the account itself.
    public struct Verified: Equatable {
        /// The keys the pin names: the pinned key and its proven predecessors
        /// (`pinKeys`); empty when the account is not pinned.
        public let pinKeysB64: [String]
        /// The key a verified binding names for the account, or `nil`.
        public let bindingKeyB64: String?

        public init(pinKeysB64: [String], bindingKeyB64: String?) {
            self.pinKeysB64 = pinKeysB64
            self.bindingKeyB64 = bindingKeyB64
        }
    }

    /// The contact row that makes `accountIdHex` an ESTABLISHED contact, or
    /// `nil`.
    ///
    /// A contact row's account id can come from the peer's OWN profile
    /// envelope, and a member can write any account id there. Such a row
    /// establishes nothing: a member who claimed another account's id would
    /// otherwise make that account's later, genuine request vanish (answered
    /// silently, never listed). So a row counts only when its key is one this
    /// device verified for the account itself. The lookup is by the ACCOUNT,
    /// never by the key (the key is exactly what a reset changes):
    ///
    /// | a row names the account | the pin names that row's key | a verified binding names that row's key | result   |
    /// |-------------------------|------------------------------|-----------------------------------------|----------|
    /// | no                      | -                            | -                                       | none     |
    /// | yes                     | yes                          | any                                     | that row |
    /// | yes                     | no                           | yes                                     | that row |
    /// | yes                     | no                           | no                                      | none     |
    ///
    /// "The pin names the key" means the pinned key or one of its PROVEN
    /// predecessors (ADR-0074 section 3.5: after a proven rotation the row
    /// keeps the key the person's leaves are bound to). A key a reset retired
    /// is not among them. Every row naming the account is considered, so a
    /// claimed row cannot hide the real one, and a pin written for the
    /// account's REAL key (by this check or any earlier fetch) never
    /// certifies a claimed row: the comparison is with the row's own key.
    ///
    /// Generic over the caller's contact type; `row` reads the slice.
    public static func establishedContactOfAccount<T>(
        accepted: [T],
        accountIdHex: String?,
        verified: Verified,
        row: (T) -> ContactRow
    ) -> T? {
        guard let accountIdHex, !accountIdHex.isEmpty else { return nil }
        let wanted = accountIdHex.lowercased()
        return accepted.first { contact in
            let r = row(contact)
            guard let rowAccount = r.accountIdHex, rowAccount.lowercased() == wanted,
                  !r.signaturePubkeyB64.isEmpty else { return false }
            return verified.pinKeysB64.contains(r.signaturePubkeyB64)
                || r.signaturePubkeyB64 == verified.bindingKeyB64
        }
    }

    /// `establishedContactOfAccount` over plain rows.
    public static func establishedContactOfAccount(
        accepted: [ContactRow], accountIdHex: String?, verified: Verified
    ) -> ContactRow? {
        establishedContactOfAccount(
            accepted: accepted, accountIdHex: accountIdHex, verified: verified, row: { $0 })
    }

    /// Whether a contact row sits under the key that signed a request.
    ///
    /// A request reaches this code with its signature verified, so the
    /// sender holds that key, and a row under the same key is that person
    /// whatever the row says about their account: a row with no account id,
    /// or one no pin or binding vouches for yet, is still a contact and must
    /// be answered. Treating it as a stranger's request drops it silently
    /// (the inbox keeps no request from a contact's key) and leaves the
    /// sender waiting for good. A member who claimed someone else's account
    /// id gains nothing: the comparison is with the key that signed.
    public static func isContactKey(_ requestKeyB64: String, in rowKeysB64: [String]) -> Bool {
        !requestKeyB64.isEmpty && rowKeysB64.contains(requestKeyB64)
    }

    /// The keys a pin holds for an account.
    public struct PinKeys: Equatable {
        public let currentKeyB64: String
        public let provenPredecessorsB64: [String]

        public init(currentKeyB64: String, provenPredecessorsB64: [String]) {
            self.currentKeyB64 = currentKeyB64
            self.provenPredecessorsB64 = provenPredecessorsB64
        }
    }

    /// The keys a pin names for an account: the pinned key and its proven
    /// predecessors; empty when the account is not pinned.
    public static func pinKeys(of keys: PinKeys?) -> [String] {
        guard let keys else { return [] }
        return [keys.currentKeyB64] + keys.provenPredecessorsB64
    }
}

// Which keys a contact request goes out to once the recipient's published
// identity has passed the fail-closed gate (ADR-0074 section 3.5a rule 1).
//
// iOS mirror of `requestTargetAfterFetch` in
// `apps/web/src/contacts/request-target.ts`. The other half of that web
// module, `contactRequestGate`, already has its twin: `ContactRequestGate`
// in `Contacts/ContactsClient.swift`. It is not repeated here.
public enum RequestTarget: Equatable {
    /// The keys in hand are the published ones.
    case asSupplied
    /// The key in hand is a PROVEN predecessor of the published one: seal to
    /// the published keys and send to the current mailbox. Never to the old
    /// key.
    case current
    /// The key in hand is neither: nothing is sent. Old keys do not come
    /// back, and an unproven other key is not this person.
    case refuse

    /// | supplied key is the published key | published is the proven successor of supplied | target       |
    /// |-----------------------------------|-----------------------------------------------|--------------|
    /// | yes                               | any                                           | `asSupplied` |
    /// | no                                | yes                                           | `current`    |
    /// | no                                | no                                            | `refuse`     |
    public static func afterFetch(
        suppliedKeyB64: String, publishedKeyB64: String, publishedIsProvenSuccessor: Bool
    ) -> RequestTarget {
        if suppliedKeyB64 == publishedKeyB64 { return .asSupplied }
        return publishedIsProvenSuccessor ? .current : .refuse
    }
}
