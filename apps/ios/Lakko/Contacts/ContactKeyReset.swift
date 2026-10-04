// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 phase 2 — "a contact reset their account keys", the contact half
// (`docs/ux/identity-recovery.md` section 16.1 / 16.2 / 16.8).
//
// Everything here is a pure decision over values the repositories already
// hold; the state itself rides on ``AcceptedContact/keyReset`` and the
// repositories do the reading and writing. Nothing in this file touches a
// server: a reset is noticed through the fail-closed identity gate the client
// already runs (ADR-0016) and the KT log's `is_reset` marker it already reads
// (ADR-0019 / ADR-0036); verification is a local act that tells the contact
// nothing.

/// The record a contact carries from the moment a reset is noticed until the
/// person verifies the new safety number (16.8, "per contact").
///
/// `previousKeyB64` is the pin the contact had before the reset — the key its
/// leaves in every shared group still resolve to, and the key the eviction of
/// 16.3 names. `pendingKeyB64` is the published key the log confirmed as a
/// reset; nothing trusts it until ``ContactsRepository/confirmKeyReset(messageId:)``
/// re-pins it. `observedAtMs` is this device's clock at the observation, for
/// the record only.
public struct ContactKeyReset: Codable, Equatable {
    public let previousKeyB64: String
    public let pendingKeyB64: String
    public let observedAtMs: Int64

    public init(previousKeyB64: String, pendingKeyB64: String, observedAtMs: Int64) {
        self.previousKeyB64 = previousKeyB64
        self.pendingKeyB64 = pendingKeyB64
        self.observedAtMs = observedAtMs
    }
}

/// 16.8 — `keyState`. Two values and no third: a changed key the log does not
/// confirm as a reset is an error at the action (today's copy) and no state.
public enum ContactKeyState: Equatable {
    case normal
    case resetUnverified
}

// MARK: - 16.1 detection

/// The phase-2 trigger, as ADR-0065 states it: the pin differs from the
/// published key, there is no proof, and the log's entry for the new key
/// carries `is_reset`.
///
/// A refusal for any other reason — an invalid proof, a proof naming a third
/// key, a backward marker — is not a reset: a proof was PRESENT and failed,
/// which is the compromise-shaped case the gate exists for. And a confirmed
/// `false` or an unknown `nil` marker keeps today's error copy: the client
/// never records a reset the log did not confirm.
public enum KeyResetDetection {
    public static func isReset(reason: SupersessionRefused.Reason, loggedReset: Bool?) -> Bool {
        reason == .proofAbsent && loggedReset == true
    }
}

// MARK: - 16.1 what is blocked

/// What an unverified reset blocks, and only that. Everything in the table of
/// 16.1 is one question — "may this device act towards this contact's keys" —
/// asked from five places; one predicate keeps the five from drifting.
public enum KeyResetBlocking {
    /// Start a new one-to-one, add to a group, approve a join request,
    /// include in a recreated group, call from the contact screen.
    public static func blocksActions(_ state: ContactKeyState) -> Bool {
        state == .resetUnverified
    }
}

// MARK: - 16.3 / 16.4 / 16.5 which branch a shared group falls in

/// What the client does in ONE shared group when a contact's reset is noticed
/// (16.3 eviction, 16.4 the ended one-to-one, 16.5 the owned group, and the
/// group with no role table of 16.10).
public enum SharedGroupResetBranch: Equatable {
    /// A one-to-one cannot lose a person (ADR-0062): the chat ends (16.4).
    case closeOneToOne
    /// A role-table group the old identity did not own, and this device's
    /// account is owner or admin: propose the removal of every old device,
    /// silently (16.3).
    case evict
    /// The old identity OWNED the group: `owner_survives` makes the eviction
    /// uncommittable and the group can get no new owner. The notice of 16.5,
    /// with `Recreate group` for an admin.
    case ownedGroup(canRecreate: Bool)
    /// Flag the contact and propose nothing: a plain member of a role-table
    /// group (the core refuses their proposal), or a group with no role table
    /// at all (no proposal path).
    case flagOnly
}

/// The role table as the branch needs it: the owner's key and whether this
/// device's account may propose a removal.
public struct SharedGroupRoleFacts: Equatable {
    /// The owner's account key (base64), or `nil` for a group with no role
    /// table.
    public let ownerKeyB64: String?
    /// `true` when this device's account is the owner or an admin.
    public let ownAccountIsOwnerOrAdmin: Bool

    public init(ownerKeyB64: String?, ownAccountIsOwnerOrAdmin: Bool) {
        self.ownerKeyB64 = ownerKeyB64
        self.ownAccountIsOwnerOrAdmin = ownAccountIsOwnerOrAdmin
    }
}

/// Decide the branch for one shared group.
///
/// The one-to-one test runs first because ADR-0062 fixes it in the group's
/// own context and nothing about roles changes it. Then the owner test, which
/// is about the OLD key: a role table names account keys, and the old
/// identity's key stays in it after the reset (a `Remove` rewrites no
/// `GroupContext`, and here nothing is even removed).
public func sharedGroupResetBranch(
    isOneToOne: Bool,
    roles: SharedGroupRoleFacts,
    previousKeyB64: String
) -> SharedGroupResetBranch {
    if isOneToOne { return .closeOneToOne }
    guard let owner = roles.ownerKeyB64 else { return .flagOnly }
    if owner == previousKeyB64 {
        return .ownedGroup(canRecreate: roles.ownAccountIsOwnerOrAdmin)
    }
    return roles.ownAccountIsOwnerOrAdmin ? .evict : .flagOnly
}

// MARK: - 16.3 the `applied` test

/// Whether the eviction of a contact's old devices has applied in a group:
/// no leaf of the roster resolves to `previousKeyB64` any more
/// (`proposal-based-membership.md` section 4, compared on the person key the
/// device binding resolves to, never on a leaf index).
///
/// A `nil` roster is a torn read and claims nothing: the eviction is not
/// reported applied on evidence that was never read.
public func keyResetEvictionApplied(
    rosterPersonKeysB64: [String]?,
    previousKeyB64: String
) -> Bool? {
    guard let rosterPersonKeysB64 else { return nil }
    return !rosterPersonKeysB64.contains(previousKeyB64)
}

// MARK: - 16.8 the add arm of the re-admission check

/// Which accounts the ADR-0050 re-admission arm must NOT propose an Add for:
/// an account whose contact is `reset-unverified`, or whose published key is
/// not this device's pin for it. Without this every member's client would
/// propose adding the new identity's devices to every shared group within
/// ten minutes of the reset, ahead of any verification.
///
/// `pinMatchesPublished == nil` is "not checked" and suppresses too: the arm
/// adds only what it has positively confirmed.
public struct ReadmissionAccountFacts: Equatable {
    public let personKeyB64: String
    public let keyState: ContactKeyState
    public let pinMatchesPublished: Bool?

    public init(personKeyB64: String, keyState: ContactKeyState, pinMatchesPublished: Bool?) {
        self.personKeyB64 = personKeyB64
        self.keyState = keyState
        self.pinMatchesPublished = pinMatchesPublished
    }
}

public func readmissionSuppressedPersons(_ facts: [ReadmissionAccountFacts]) -> Set<String> {
    Set(
        facts
            .filter { $0.keyState == .resetUnverified || $0.pinMatchesPublished != true }
            .map(\.personKeyB64))
}

// MARK: - 16.4 a late message into a closed one-to-one

/// What the receive path does with a journal row of a one-to-one this device
/// closed: dropped before it is stored, never shown. The chat is closed;
/// rendering a late message under the closed row would reopen it in the
/// person's eyes, and the message is from keys that are no longer the
/// contact's. Every row kind is dropped — a call offer, a typing signal and a
/// read receipt are "live anything", which the closed row has none of.
public enum ClosedOneToOneRowDisposition: Equatable {
    case store
    case drop
}

public func closedOneToOneRowDisposition(closed: Bool) -> ClosedOneToOneRowDisposition {
    closed ? .drop : .store
}

// MARK: - 16.2 the re-pin marker

/// The monotone marker the confirmation re-pins under (ADR-0016 rule #4).
///
/// The store refuses a marker below the one it holds, and the marker a proof
/// wrote is a proof expiry in unix seconds; a key generation (a small
/// integer) would read as backwards. So the confirmation advances to the
/// later of the stored marker and now: never backwards, and a later
/// re-verification of a later reset is never refused as backwards either.
public func keyResetRepinMarker(existingMarker: Int64?, nowSecs: Int64) -> Int64 {
    max(existingMarker ?? 0, nowSecs)
}
