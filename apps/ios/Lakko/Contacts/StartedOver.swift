// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 11 / UX `identity-recovery.md` 16.11 — a contact whose
// keys were reset with no seal started over: a new account under the same
// username. Nothing moves. Two things are shown, the only exception to "a
// contact's reset shows nothing":
//
//   * the one-to-one with the keys of before the reset keeps its history
//     and loses its composer and its call buttons, for one line;
//   * a contact request from the new keys is an ordinary request card with
//     one more line.
//
// The record is in ``PeerPinStore`` (`startedOver`, `startedOverKeysB64`);
// this file is the pure rules the surfaces ask.

/// A contact request against a pending unsealed reset.
enum StartedOverRequest {

    /// The one more line under the name on the request card.
    static let noteKey = "contacts.started_over.request_note"

    /// Whether the request was signed by the keys the account has since its
    /// unsealed reset. Only such a request is listed with the note; one from
    /// any other key of that account is from devices that no longer exist.
    static func isFromNewKeys(requestKeyB64: String, newKeyB64: String) -> Bool {
        !requestKeyB64.isEmpty && requestKeyB64 == newKeyB64
    }

    /// Whether accepting the request takes the account's new keys as its
    /// pin: exactly when an unsealed reset is pending for it and the request
    /// was signed by the keys that reset installed.
    static func adoptsNewKeys(requestKeyB64: String, pendingNewKeyB64: String?) -> Bool {
        guard let pendingNewKeyB64 else { return false }
        return isFromNewKeys(requestKeyB64: requestKeyB64, newKeyB64: pendingNewKeyB64)
    }
}

/// The one-to-one with a person who started over.
enum StartedOverChat {

    /// The line in place of the composer; `{name}` is the person's name as
    /// the conversation shows it.
    static let closedKey = "contacts.started_over.closed"

    /// What the bottom of the conversation shows.
    enum Bottom: Equatable {
        /// The ordinary composer.
        case composer
        /// `contacts.started_over.closed` in place of the composer; the call
        /// buttons are hidden.
        case closedLine
    }

    /// - Parameters:
    ///   - oneToOne: the conversation is a one-to-one (a group is never
    ///     closed for a member who started over).
    ///   - peerKeyB64: the account key of the other person in it.
    ///   - startedOverKeysB64: every key an unsealed reset left behind
    ///     (``PeerPinStore/loadStartedOverKeys()``).
    static func bottom(oneToOne: Bool, peerKeyB64: String?, startedOverKeysB64: Set<String>) -> Bottom {
        guard oneToOne, let peerKeyB64, startedOverKeysB64.contains(peerKeyB64) else { return .composer }
        return .closedLine
    }

    /// Alignment item E — what an unsent message of the conversation does:
    /// in a one-to-one whose person started over it is never sent; it is
    /// `Not sent`, with no Retry.
    enum Unsent: Equatable {
        case send
        case notSent
    }

    static func unsent(oneToOne: Bool, peerKeyB64: String?, startedOverKeysB64: Set<String>) -> Unsent {
        bottom(oneToOne: oneToOne, peerKeyB64: peerKeyB64, startedOverKeysB64: startedOverKeysB64) == .closedLine
            ? .notSent : .send
    }
}

/// Alignment item D (UX 16.11) — an add the person asked for by name, of a
/// person who started over: the pin moves to the new account's keys as a
/// first pin would (the keys of before stay closed, so the old one-to-one
/// stays closed). `false` when no such reset is pending for exactly those
/// keys (the refusal then stands).
enum StartedOverAdd {
    @discardableResult
    static func adopt(_ startedOver: ContactStartedOver, pins: PeerPinStore, nowMs: Int64) -> Bool {
        guard pins.adoptStartedOver(
            accountIdB64: startedOver.accountIdB64, newKeyB64: startedOver.newKeyB64, nowMs: nowMs) else {
            return false
        }
        StartedOverEvents.notify(accountIdB64: startedOver.accountIdB64)
        return true
    }
}

/// UX 16.11 (alignment follow-up) — the card of a person reached through a
/// key an unsealed reset left behind (the old conversation's person) offers
/// no `Add to contacts`: that account no longer exists, and adding it would
/// pin keys whose devices are gone. The person's new account is added by
/// name or from its own request.
enum StartedOverCard {
    static func sections(_ sections: [InfoSection], keyStartedOver: Bool) -> [InfoSection] {
        keyStartedOver ? sections.filter { $0 != .addContact } : sections
    }
}
