// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Who authored a message — the pure resolution rule behind the ADR-0023
/// author-only edit / delete gate, the ADR-0023 reaction attribution and the
/// ADR-0024 read watermark.
///
/// iOS mirror of `apps/web/src/chat/author-identity.ts`.
///
/// ## Why a signature key and not a leaf index
///
/// An MLS leaf index is a **position in the ratchet tree, not a person**.
/// OpenMLS hands a vacated leaf to the next member added, so `remove Bob, add
/// Dave` — which ADR-0045 composes into ONE commit, landing in a single poll
/// batch — leaves Dave holding Bob's index. Every rule that compares raw leaf
/// indices then reads Dave as Bob: Dave may edit and delete Bob's history,
/// toggle off Bob's reactions, and inherit Bob's read watermark.
///
/// On iOS the edit / delete half was worse than transient. The author leaf was
/// **persisted** in the message log and read back verbatim, so the
/// mis-binding survived a restart for the whole retained history.
///
/// The Ed25519 **signature public key** is the stable identity: it names the
/// member, it survives a renumber, and it is what every rule written after
/// ADR-0023 already keys on for exactly this reason — the role table, the
/// committer rank, the ADR-0045 beacon observation store, the ADR-0042
/// push-routing authority gate, the ADR-0043 membership-shrink diff. ADR-0023
/// predates that convention; this brings it in line.
///
/// ## Why resolution happens at receipt
///
/// A leaf only names a member **in the epoch the message was sealed at**.
/// ``ChatClient/applyFetchedMessages(identity:group:raw:after:)`` calling
/// `processIncomingVerifyingRoles` and getting an Application frame back is the
/// one moment the group's roster IS that epoch's roster (a Commit that
/// renumbers leaves is a separate journal row, and OpenMLS decrypts an
/// Application message only at the current epoch). So the leaf is resolved to a
/// key THERE, in the receive loop, and the KEY is what travels onward and gets
/// persisted — never the leaf, resolved later against whatever roster happens
/// to be current then.
///
/// That "resolve now, carry the key" shape is the one `PushGroupRouting` and
/// the ADR-0045 beacon store already use.
public enum AuthorIdentity {

    /// Sentinel identity for an own reaction whose signature key the client
    /// could not resolve (a torn roster read). It still toggles and still
    /// counts, exactly as the old numeric leaf did.
    ///
    /// `?` is outside the base64 alphabet, so this can never collide with a
    /// real key — the property the old leaf value space had against a real
    /// leaf.
    public static let ownReactionSentinel: MemberIdentity = "?own"

    /// Resolve one MLS leaf index to the member's signature key, against the
    /// roster the caller is holding — which the caller must have read in the
    /// epoch the message was sealed at.
    ///
    /// `nil` on a miss — an unknown leaf, or a roster the caller could not
    /// read. Fail-closed is the whole contract: every consumer treats an
    /// unresolved identity the way it treats an unknown author, which is to
    /// refuse.
    public static func resolve(
        roster: [RosterEntry],
        leafIndex: UInt32
    ) -> MemberIdentity? {
        guard let hit = roster.first(where: { $0.leafIndex == leafIndex }) else {
            return nil
        }
        return base64Encode(hit.signaturePublicKey)
    }

    /// The live-`Group` form of ``resolve(roster:leafIndex:)``.
    ///
    /// A torn / unreadable roster resolves every leaf to `nil` — the
    /// fail-closed direction: an unattributable message can never be edited or
    /// deleted by anyone, which is strictly what an unknown author already
    /// meant.
    public static func resolve(group: Group, leafIndex: UInt32) -> MemberIdentity? {
        resolve(roster: roster(of: group), leafIndex: leafIndex)
    }

    /// This device's own identity in `group`, resolved THROUGH the roster
    /// rather than stored, so a leaf renumber cannot make it name someone else.
    ///
    /// This is the value every authorship decision on our own side uses: the
    /// outgoing row's author stamp, our own reaction attribution, and the
    /// "read by every other member" exclusion.
    public static func own(group: Group) -> MemberIdentity? {
        guard let leaf = try? group.ownLeafIndex() else { return nil }
        return resolve(group: group, leafIndex: leaf)
    }

    /// The group's roster as the resolver reads it. Fail-closed to an empty
    /// roster, which resolves every leaf to `nil`.
    public static func roster(of group: Group) -> [RosterEntry] {
        guard let members = try? group.members() else { return [] }
        // ADR-0050 — a leaf names a DEVICE; the author is the PERSON, so each
        // leaf resolves to its account key (else its own key when the
        // credential did not resolve). One person is one author across all
        // of their devices.
        return members.map {
            RosterEntry(leafIndex: $0.leafIndex, signaturePublicKey: PersonKey.of($0))
        }
    }

    /// The author-only integrity gate for edit / delete (ADR-0023, invariant
    /// #4). `true` ONLY when both identities are known and equal. Fail-closed:
    /// an unknown author (a message stored before the author identity was
    /// persisted, a control frame) or an unknown op sender is never authorised.
    ///
    /// The comparison is on the **signature key**, and the reason is
    /// load-bearing rather than stylistic: comparing MLS leaf indices here is
    /// what let a member added into a departed member's vacated leaf edit and
    /// delete that member's history. See the type doc.
    ///
    /// Reactions do NOT go through this gate — any member may react.
    public static func editDeleteAuthorized(
        author: MemberIdentity?,
        opSender: MemberIdentity?
    ) -> Bool {
        guard let author, let opSender else { return false }
        return author == opSender
    }

    /// Read an author identity back off a persisted message-log row.
    ///
    /// The stored field is `String?` and is **absent entirely on a row written
    /// before it existed** — both the `nil` and the absent case collapse to
    /// `nil`, and `nil` is what the edit / delete gate refuses. That is exactly
    /// the posture every reloaded row had before this field existed. There is
    /// no migration and none is owed.
    ///
    /// The tempting alternative for an OUTGOING row — "it is ours, so fall back
    /// to our current key" — is wrong: it manufactures a confident author out
    /// of a record that has none. Fail closed, and let the gate refuse.
    public static func persisted(_ stored: String?) -> MemberIdentity? {
        stored
    }

    /// The inverse, for the persist sweep. An unresolved identity is written as
    /// an explicit `nil` rather than as anything a later reader could mistake
    /// for a person.
    public static func toPersisted(_ identity: MemberIdentity?) -> String? {
        identity
    }
}

/// A member's stable identity: their Ed25519 signature public key,
/// base64-standard — the same encoding the role table, the roster views, the
/// accepted-contact map and the observation stores use, so one member has one
/// string everywhere in the client.
public typealias MemberIdentity = String

/// The fields ``AuthorIdentity`` reads off a roster. Declared structurally
/// rather than taken as `GroupMember` so the pure rule stays testable without a
/// live MLS group.
public struct RosterEntry: Equatable {
    public let leafIndex: UInt32
    public let signaturePublicKey: Data

    public init(leafIndex: UInt32, signaturePublicKey: Data) {
        self.leafIndex = leafIndex
        self.signaturePublicKey = signaturePublicKey
    }
}
