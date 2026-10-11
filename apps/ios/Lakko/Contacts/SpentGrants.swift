// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0066 section 3.5a rule 5 — what was granted before a key rotation is
// spent.
//
// The rotating account's `/i/` and `/c/` links and the `reply` grants of its
// unanswered contact requests were derived from the OLD account seed and name
// the old mailbox. They are not honoured through it: the holder of the old
// seed knows the same links. So the device:
//
//   * revokes its own links at their ledger (signed by the key that issued
//     them, i.e. the predecessor) and removes them from its lists;
//   * sends each unanswered contact request again from the new key, with a
//     fresh reply grant for the new mailbox, and replaces the stored entry.
//
// The decisions are here, pure; `ContactsRepository` runs them.

public enum SpentGrants {

    /// One ledger row to revoke.
    public struct LinkToRevoke: Equatable {
        /// The 6-byte ledger id.
        public let blobId: Data
        /// `true` for the call-invite ledger (`/c/`), `false` for the
        /// contact-invite ledger (`/i/`).
        public let call: Bool
        /// The spent reach scope this row belongs to, when this device minted
        /// the link; `nil` for a row only the owner list named.
        public let linkIdB64: String?
    }

    /// Every ledger row the rotation revokes: each spent link scope that has
    /// a ledger id (at the ledger of its kind), then every `/i/` row the
    /// owner list returned that no scope here names (a link another of this
    /// account's devices minted). A scope with no ledger id (an inline link)
    /// has nothing to revoke.
    public static func linksToRevoke(
        spent: [LinkReach], listedInviteBlobIds: [Data]
    ) -> [LinkToRevoke] {
        var out: [LinkToRevoke] = []
        var seen = Set<Data>()
        for link in spent {
            guard let b64 = link.blobIdB64, let blobId = Data(base64Encoded: b64), !blobId.isEmpty,
                  seen.insert(blobId).inserted else { continue }
            out.append(LinkToRevoke(
                blobId: blobId, call: link.kind == LinkReach.callKind, linkIdB64: link.linkIdB64))
        }
        for blobId in listedInviteBlobIds where seen.insert(blobId).inserted {
            out.append(LinkToRevoke(blobId: blobId, call: false, linkIdB64: nil))
        }
        return out
    }

    /// What one revoke attempt under one of the account's old keys answered.
    public enum RevokeAnswer: Equatable {
        case revoked
        /// No row, or the row was issued by another key.
        case notFoundOrNotOwner
        /// No verdict (offline, a server error).
        case failed
    }

    public enum LinkVerdict: Equatable {
        /// The row is gone at the ledger: drop the link everywhere here.
        case gone
        /// No old key of this account issued a row with this id. A link this
        /// device minted is then simply no longer there (expired, revoked
        /// before); a row only the owner list named belongs to the NEW key
        /// and stays.
        case notOurs
        /// No verdict yet: the step stays owed.
        case owed
    }

    /// The verdict over the answers of every old key tried, newest first.
    /// One `revoked` settles it; a failure with no `revoked` keeps it owed
    /// (the key that failed may be the issuer).
    public static func linkVerdict(_ answers: [RevokeAnswer]) -> LinkVerdict {
        if answers.contains(.revoked) { return .gone }
        if answers.contains(.failed) || answers.isEmpty { return .owed }
        return .notOurs
    }

    /// Whether the link leaves this device's lists after `verdict`.
    public static func dropsLocally(_ verdict: LinkVerdict, mintedHere: Bool) -> Bool {
        switch verdict {
        case .gone: return true
        case .notOurs: return mintedHere
        case .owed: return false
        }
    }

    /// The recipients (16-byte account ids, once each, in order) of the
    /// unanswered contact requests to send again: every spent reply scope
    /// issued for a contact request, minus the people who are contacts
    /// already (their request was answered; the scope outlived it).
    public static func requestsToResend(
        spentReplies: [ReplyReach], acceptedAccountIds: [Data]
    ) -> [Data] {
        var out: [Data] = []
        for reply in spentReplies {
            guard let accountId = ReplyPurpose.contactAccountId(of: reply.purpose),
                  !acceptedAccountIds.contains(accountId), !out.contains(accountId) else { continue }
            out.append(accountId)
        }
        return out
    }

    public enum ResendVerdict: Equatable {
        /// Sent from the new key: the stored entry is replaced.
        case sent
        /// It can never be sent (the recipient's mailbox refuses this device,
        /// their keys were reset, they have no identity): the stored entry is
        /// dropped, so no dead request is shown as pending.
        case dead
        /// No verdict yet: the step stays owed.
        case owed
    }

    /// The verdict of one re-send by what it threw (`nil` = it was sent).
    public static func resendVerdict(_ error: Error?) -> ResendVerdict {
        guard let error else { return .sent }
        if SupersessionRefused.find(in: error) != nil { return .dead }
        switch error as? ContactsError {
        case .requestRefused?, .noIdentityBundle?, .usernameUnknown?: return .dead
        default: return .owed
        }
    }

    /// The sent list after the request to `recipientAccountIdB64` got
    /// `verdict`: its row replaced by `replacement`, dropped, or left.
    public static func sentList(
        _ sent: [SentContactRequest], recipientAccountIdB64: String,
        verdict: ResendVerdict, replacement: SentContactRequest?
    ) -> [SentContactRequest] {
        switch verdict {
        case .owed:
            return sent
        case .dead:
            return sent.filter { $0.recipientAccountIdB64 != recipientAccountIdB64 }
        case .sent:
            guard let replacement else { return sent }
            let username = sent.first { $0.recipientAccountIdB64 == recipientAccountIdB64 }?
                .recipientUsername ?? replacement.recipientUsername
            return sent.filter { $0.recipientAccountIdB64 != recipientAccountIdB64 } + [
                SentContactRequest(
                    recipientUsername: username, recipientAccountIdB64: recipientAccountIdB64,
                    messageId: replacement.messageId, createdAt: replacement.createdAt)
            ]
        }
    }
}
