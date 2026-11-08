// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The other person's name on a call surface — the status line, the tile under
/// their video, the encryption sheet's sentence (`docs/ux/calls.md` sections 4
/// and 5).
///
/// ## Why this exists
///
/// The 1x1 call roster was synthesised with `displayName: ""` and the tile's
/// own fallback for an empty name was the literal word **"Unknown"**, so every
/// connected 1x1 call on iOS labelled the person being spoken to "Unknown" —
/// while the incoming banner, reading the same directory two files away, had
/// their name. Nothing was missing; nothing was ever asked for.
///
/// The chain is ordered by how much the viewer chose it:
///
///   1. the **resolved profile name** (a local rename, then their own profile
///      name, then a verified username) — `ProfileDirectory`'s answer, passed
///      in as `nil` when it is the neutral placeholder rather than a name;
///   2. the **conversation title** — what the chat list and the chat header
///      call this conversation, which for a 1x1 is the same person by another
///      route and for a renamed conversation is the name the user gave it;
///   3. nothing — the caller renders ``placeholder``.
///
/// "Unknown" is never an answer. It says the person is unknowable when what is
/// true is that this device has not been told their name yet.
public enum CallPeerName {

    /// The neutral label for a person nothing is known about. The app's one
    /// placeholder (``ResolvedDisplayName/placeholderMember``), so a call
    /// roster and a chat roster never disagree about the same stranger.
    ///
    /// Rendered in the interface language through the same key the chat
    /// roster uses for it (`chat.members.unnamed`).
    public static var placeholder: String { Localization.text("chat.members.unnamed") }

    /// The other person's name, or `nil` when neither rung has one.
    ///
    /// - Parameters:
    ///   - profileName: the resolved display name, or `nil` when the resolver
    ///     had only a placeholder (pass `resolved.isPlaceholder ? nil :
    ///     resolved.name` — a placeholder is not a name and must fall through).
    ///   - chatTitle: the conversation's title, or `nil`.
    public static func resolve(profileName: String?, chatTitle: String?) -> String? {
        for candidate in [profileName, chatTitle] {
            guard let trimmed = candidate?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !trimmed.isEmpty
            else { continue }
            return trimmed
        }
        return nil
    }

    /// The label a call tile carries: "You" for the local participant, the
    /// resolved name for anyone else, and ``placeholder`` when there is none.
    public static func tileLabel(displayName: String, isLocal: Bool) -> String {
        let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        return isLocal ? Localization.text("call.participant.you") : placeholder
    }

    /// The name at the top of a call: the resolved name when there is one, the
    /// neutral placeholder for a nameless 1x1 call, and `Group call` for a call
    /// nobody is the other person on.
    ///
    /// Here rather than inside the call screen because the collapsed bar shows
    /// the same label (`docs/ux/calls.md` section 4.5 — *"the same label the
    /// call screen uses"*), and two surfaces computing the same label from the
    /// same value is two surfaces that can be changed apart.
    ///
    /// - Parameter groupCall: `true` when nobody is "the other person" — a
    ///   group call, or a call from a link.
    public static func callLabel(profileName: String?, groupCall: Bool) -> String {
        resolve(profileName: profileName, chatTitle: nil)
            ?? (groupCall ? Localization.text("call.label.group") : placeholder)
    }

    /// Whether the encryption sheet speaks to two people (`docs/ux/calls.md`
    /// section 5). A group call or a call from a link never does, whatever its
    /// size, and neither does a call with more than one other person in it,
    /// whatever it was placed as.
    ///
    /// - Parameters:
    ///   - groupCall: `true` for a group call or a call from a link.
    ///   - othersInCall: the other people in the call now.
    public static func isOneToOneEncryptionSheet(groupCall: Bool, othersInCall: Int) -> Bool {
        !groupCall && othersInCall <= 1
    }

    /// The encryption sheet's one plain sentence (`docs/ux/calls.md` section
    /// 5). Word for word what `encryptionSheetSentence` says on web and
    /// `callEncryptionSentence` on Android.
    ///
    /// A one-to-one call names the other person when they have a name. A group
    /// or link call never names one person: everyone in it compares the same
    /// number, and naming one participant claims a two-person call. With no
    /// name the sentence says something true of any call rather than inventing
    /// one.
    public static func encryptionSentence(
        peerName: String?, groupCall: Bool, othersInCall: Int
    ) -> String {
        guard isOneToOneEncryptionSheet(groupCall: groupCall, othersInCall: othersInCall),
              let name = resolve(profileName: peerName, chatTitle: nil)
        else {
            return Localization.text("call.encryption.sentence_group")
        }
        return Localization.text("call.encryption.sentence_named", texts: ["name": name])
    }

    /// The line under the safety number (`docs/ux/calls.md` section 5): two
    /// people read it to each other; in a group or link call everyone sees the
    /// same number. Never "phones": people join from computers too.
    public static func encryptionHint(groupCall: Bool, othersInCall: Int) -> String {
        isOneToOneEncryptionSheet(groupCall: groupCall, othersInCall: othersInCall)
            ? Localization.text("call.encryption.compare_hint_one_to_one")
            : Localization.text("call.encryption.compare_hint_group")
    }
}
