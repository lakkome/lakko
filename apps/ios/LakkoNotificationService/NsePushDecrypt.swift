// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Read-only THROWAWAY MLS decrypt for the iOS silent-push NSE path
/// (ADR-0041 Phase 3).
///
/// The iOS counterpart of the web `public/sw-push-decrypt.js` (commit
/// `148753b`) and Android `PushPreviewDecrypt.kt` (commit `ae9a087`). When a
/// mutable-alert APNs wake lands, `NotificationService.didReceive` builds the
/// real (shared-store / slim-FFI / DS) dependency object and calls
/// ``decryptPushPreview(_:_:)`` to turn the `(group_pseudonym, pointer)` wake
/// hint into the REAL decrypted text of the newest message, so the
/// notification body says what actually arrived instead of a content-free
/// "New message".
///
/// ## Why this file holds no FFI / UserNotifications types
///
/// Every capability it needs — the store reads, the MLS restore + decrypt,
/// the DS fetch — is INJECTED via ``NsePushDecryptDeps``. The file imports
/// only `Foundation`, so it compiles into BOTH the `LakkoNotificationService`
/// (NSE) target — which supplies the real deps over the slim
/// `LakkoCoreExtension` framework (`NseDecryptDeps.swift`) — AND the
/// `LakkoTests` unit-test target, which drives it with hand-rolled fakes and
/// no `LakkoCoreExtension.xcframework` linked. The production wiring that
/// touches the slim FFI + Keychain lives in `NseDecryptDeps.swift`; the pure,
/// FFI-free decrypt logic tested here is this file.
///
/// ## Load-bearing correctness: READ-ONLY THROWAWAY
///
/// This path MUST NOT corrupt the MLS ratchet state the foreground app relies
/// on. (The NSE is a SEPARATE process from the app — there is no shared
/// in-memory handle to corrupt — but it must still never write BACK the
/// advanced snapshot / cursor, or the foreground app, which restores from the
/// SAME last-persisted snapshot and re-processes from its OWN cursor, would
/// desync its ratchet — the G-34 nonce-reuse-on-crash class.) It:
///
///   1. reads the AccountIdentity seed + the last-persisted MLS snapshot
///      (both READ-ONLY),
///   2. restores a TEMPORARY, in-memory throwaway group from that snapshot (a
///      fresh `Identity` + `Group` — NEVER the app's handles),
///   3. fetches the DS journal after a READ-ONLY cursor and processes the
///      rows through the temp group ONLY to extract the newest message's
///      plaintext preview,
///   4. returns the preview and DISCARDS the temp group (``NseThrowawayGroup/close()``).
///
/// It NEVER persists the advanced snapshot, NEVER advances the app's DS
/// cursor, NEVER writes any MLS state. ``NsePushDecryptDeps`` deliberately
/// exposes NO write capability — the protocol simply cannot persist, which is
/// the structural enforcement of the read-only-throwaway discipline. The slim
/// `LakkoCoreExtension` FFI itself exposes no `snapshotStorage` / persist
/// method, so there is no write path to reach even by accident.
///
/// ## Fail-closed (security invariant #4)
///
/// A frame that will not decrypt / authenticate is DROPPED, never shown as a
/// plaintext guess (``NseThrowawayGroup/process(_:)`` returns `nil`). Any
/// missing input (no account match, no seed, no snapshot, non-member group,
/// DS failure, zero decryptable frames) returns `nil`, and the NSE shows the
/// generic "New message" notification.
///
/// ## Scope
///
/// GroupMessage wakes only (chat / edit / attachment-hint text). A Wakeup /
/// MailboxMessage wake (no `groupHex` / no `pointer`) returns `nil` here and
/// stays generic (a follow-up), mirroring the web + Android slices.

/// Max characters of decrypted text shown in the notification body.
private let previewMaxChars = 140

/// Length in hex chars of a 32-byte group_id.
private let groupHexLen = 64

/// The GroupMessage wake hint, already decoded from the APNs `userInfo`
/// payload (`{ aps, d, g, p }`). `d` decides whether the wake is handled at
/// all (ADR-0042, amended 2026-09-30); `g` (group hex) + `p` (DS pointer)
/// drive the preview.
public struct NsePushWake: Equatable {
    /// 64 lowercase-hex chars = the 32-byte opaque `push_gid`, or `nil`. The
    /// decrypt core reverse-maps it to the real group_id; a miss is refused
    /// (the raw-group_id fallback of the pre-ADR-0042 wake is gone).
    public let groupHex: String?
    /// DS message id of the message that triggered the wake, or `nil`.
    public let pointer: Int64?
    /// Hex of the push-gw subscription the wake was delivered for (`s`), or
    /// `nil`. The wake is handled only when the bound account's route row for
    /// `g` holds that id (ADR-0078 section 3,
    /// ``NsePushDecryptDeps/lookupRoute(accountIdB64:pushGidHex:subscriptionIdHex:)``).
    public let subscriptionIdHex: String?

    public init(groupHex: String?, pointer: Int64?, subscriptionIdHex: String? = nil) {
        self.groupHex = groupHex
        self.pointer = pointer
        self.subscriptionIdHex = subscriptionIdHex
    }
}

/// A locally-stored account that owns the wake's group.
public struct NseResolvedAccount: Equatable {
    /// The account key shared by every per-account store (the same
    /// standard-base64 string `KnownGroupsStore` / `AccountIdentitySeedStore`
    /// / `IdentitySnapshotStore` / `MessageLogStore` all key on).
    public let accountIdB64: String
    /// The raw 32-byte group_id (hex-decoded from the wake's `g`).
    public let groupIdBytes: Data

    public init(accountIdB64: String, groupIdBytes: Data) {
        self.accountIdB64 = accountIdB64
        self.groupIdBytes = groupIdBytes
    }
}

/// One DS journal row (server id + raw MLS wire bytes).
public struct NseFetchedRow: Equatable {
    public let id: Int64
    public let payload: Data
    /// The DS row's server-assigned `created_at`, which is the only clock the
    /// ring window may be measured against (`docs/ux/calls.md` section 2.1).
    /// Empty when the fetch could not read one — an age that cannot be
    /// established is treated as past the window.
    public let createdAt: String

    public init(id: Int64, payload: Data, createdAt: String = "") {
        self.id = id
        self.payload = payload
        self.createdAt = createdAt
    }
}

/// Classification of a decrypted application message (mirror of the slim FFI
/// `MessageBodyKind`), kept FFI-free so it can be produced by the fake in the
/// unit test.
///
/// The last two arms are not message bodies at all: they are call signaling
/// (ADR-0009), which rides the same group journal. Until
/// `docs/ux/calls.md` was written this path dropped call signaling outright,
/// which is precisely why a closed iPhone never rang — see
/// ``NseCallSignalingParser``.
public enum NseProcessedKind: Equatable {
    case chat, edit, delete, reaction, typing, readReceipt
    /// A Lakko service envelope the pre-filter did not recognise (a plaintext
    /// opening with `lakko/` that is not a chat envelope: most often a newer
    /// version of a profile, group name, pin or beacon envelope from a client
    /// ahead of this one). The parser says so (`MessageBodyKind.serviceEnvelope`);
    /// it is never a preview, so an older real message stays the preview or the
    /// handler falls back to its generic banner.
    case serviceEnvelope
    /// ADR-0061 section 6 — a chat message from a newer wire version than
    /// this build reads (`MessageBodyKind.unsupported`). It IS a message, so
    /// it previews — as the catalog sentence, never as core prose.
    case unsupported
    /// An incoming call offer, and whether the caller asked for video.
    case callOffer(video: Bool)
    /// A call ended (cancelled, declined, answered elsewhere, timed out).
    case callEnd
}

/// A decoded application row (post MLS-decrypt + envelope-parse).
public struct NseProcessedRow: Equatable {
    public let kind: NseProcessedKind
    /// Chat / edit text (empty for the other kinds).
    public let text: String
    /// Number of attachment refs (chat only).
    public let attachmentCount: Int
    /// ADR-0047 — the `kind` of the message's only attachment (`voice`,
    /// `video_note`, `gif`, or an unknown string), read from the envelope the
    /// NSE already decoded. `nil` when there is no single attachment. A LABEL
    /// only: the NSE never fetches or decrypts the media itself.
    public let singleAttachmentKind: String?
    /// Lowercase hex of the 16-byte call id, for the two call kinds only. It is
    /// what lets an "ended" row take down the alert an "offer" row raised, and
    /// what the app reads back to clear that alert when the call is answered
    /// here (``LakkoCallNotification/callIdUserInfoKey``).
    public let callIdHex: String?
    /// ADR-0073 R.8 — the ACCOUNT key (standard base64) of the row's
    /// MLS-authenticated sender, resolved by the core at receipt (C-68).
    /// `nil` when the sender does not resolve. Read to drop a row from a key
    /// an accepted reset retired.
    public let senderKeyB64: String?
    /// ADR-0073 R.7 case 1 — the re-send marker of a chat row, when its
    /// sealed envelope carried one: where the message was ORIGINALLY sent.
    public let resendOrigin: NseResendOrigin?

    public init(
        kind: NseProcessedKind,
        text: String,
        attachmentCount: Int,
        singleAttachmentKind: String? = nil,
        callIdHex: String? = nil,
        senderKeyB64: String? = nil,
        resendOrigin: NseResendOrigin? = nil
    ) {
        self.kind = kind
        self.text = text
        self.attachmentCount = attachmentCount
        self.singleAttachmentKind = attachmentCount == 1 ? singleAttachmentKind : nil
        self.callIdHex = callIdHex
        self.senderKeyB64 = senderKeyB64
        self.resendOrigin = kind == .chat ? resendOrigin : nil
    }

    /// The same row with its sender and re-send marker set (the call
    /// classifier builds a row before either is known).
    public func attributed(senderKeyB64: String?, resendOrigin: NseResendOrigin? = nil) -> NseProcessedRow {
        NseProcessedRow(
            kind: kind, text: text, attachmentCount: attachmentCount,
            singleAttachmentKind: singleAttachmentKind, callIdHex: callIdHex,
            senderKeyB64: senderKeyB64, resendOrigin: resendOrigin)
    }
}

/// ADR-0073 R.7 case 1 — where a re-sent message was originally sent: the
/// original group's id (standard base64) and the DS message id there.
public struct NseResendOrigin: Equatable {
    public let groupIdB64: String
    public let messageId: Int64

    public init(groupIdB64: String, messageId: Int64) {
        self.groupIdB64 = groupIdB64
        self.messageId = messageId
    }
}

/// What a decrypted call-signaling row says, as much of it as a background
/// process needs.
public enum NseCallSignal: Equatable {
    /// Someone is calling, and whether they asked for video.
    case offer(callIdHex: String, video: Bool)
    /// A call is over — cancelled by the caller, declined, answered on another
    /// device, or timed out. The extension does not learn which, and does not
    /// need to: every one of them means "stop ringing".
    case end(callIdHex: String)
    /// Call signaling of a kind that is not a user-visible event (an answer, an
    /// ICE candidate, a group-call control body).
    case other
}

/// Reads a decrypted call-signaling body.
///
/// ## Why this is a protocol and not a function here
///
/// The wire format is a protobuf `CallSignalingPayload` and this repository is
/// emphatic that a wire format has ONE decoder: the Rust core. The extension
/// reaches it through the slim `LakkoCoreExtension` FFI, exactly as it reaches
/// the push-routing, beacon, profile and group-name codecs. Writing a second
/// decoder in Swift — even a small one — is the drift class ADR-0044 exists to
/// document, and it would be a decoder for the one envelope whose misreading
/// makes a phone ring for a call that is already over.
///
/// So the parse is INJECTED, for the same reason the other decoders are: this
/// file compiles into the FFI-free unit-test target as well as into the
/// extension, and a test drives it with a fake.
///
/// `parse` returns `nil` for "cannot tell" — an unavailable parser, or bytes
/// the decoder refuses. The caller then does what this path did before calls
/// existed: suppress the row. That is the fail-safe direction. An alert for a
/// row nobody could read would be a phone ringing for something that may not be
/// a call at all.
public protocol NseCallSignalingParser {
    func parse(_ bytes: Data) -> NseCallSignal?
}

/// The call-style alert the extension raises when a call arrives without a VoIP
/// push — the best a notification extension can do, since only CallKit can ring
/// a phone and only the app can reach CallKit.
public struct NseCallAlert: Equatable {
    /// The caller: a mirrored name, else the unnamed wording.
    public let title: String
    /// `Voice call` / `Video call` (`docs/ux/calls.md` section 2.2).
    public let body: String
    /// Lowercase hex of the call id, stamped on the notification so the app can
    /// take this alert down when the call is answered or ends.
    public let callIdHex: String
    /// The group hex — groups a conversation's notifications.
    public let tag: String

    public init(title: String, body: String, callIdHex: String, tag: String) {
        self.title = title
        self.body = body
        self.callIdHex = callIdHex
        self.tag = tag
    }
}

/// What the extension should show for this wake.
public enum NsePushOutcome: Equatable {
    /// A decrypted message preview (ADR-0041).
    case preview(NsePushPreview)
    /// A call is ringing.
    case callOffer(NseCallAlert)
    /// A call that must not interrupt: the conversation is muted, or the
    /// caller has already given up. Recorded, passively — see
    /// ``NseCallAlert/passive``.
    case callSilent(NseCallAlert)
    /// A call this device may be showing an alert for is over.
    case callEnded(callIdHex: String)
    /// ADR-0073 R.8 / R.7 — every row this wake brought is one the APP shows
    /// nothing for: a row from a key an accepted reset retired, or a re-send
    /// of a message this device already holds. The extension must still
    /// deliver something (it cannot post nothing), so it delivers the
    /// quietest thing it can: passive and silent, in the conversation's
    /// stack (``tag``).
    case quiet(tag: String)
    /// Nothing to say — the extension shows its generic fallback.
    case none
}

/// The notification preview the NSE renders when the decrypt succeeds.
public struct NsePushPreview: Equatable {
    public let title: String
    public let body: String
    /// The conversation hex — collapses a burst into one grouped
    /// notification. The group's own hex unless the group continues another
    /// conversation (ADR-0073 R.5).
    public let tag: String
    /// ADR-0073 R.5 — whether the group this preview came from continues
    /// another conversation, so ``tag`` (the conversation) is what the
    /// notification is grouped under rather than the wake's own routing id.
    public let movedConversation: Bool

    public init(title: String, body: String, tag: String, movedConversation: Bool = false) {
        self.title = title
        self.body = body
        self.tag = tag
        self.movedConversation = movedConversation
    }
}

/// A TEMPORARY, in-memory MLS group restored from the last-persisted
/// snapshot. The production implementation (`NseRealThrowawayGroup`) wraps a
/// throwaway `Identity` + `Group`; ``close()`` frees them and MUST NEVER
/// persist / snapshot them back (read-only-throwaway discipline).
public protocol NseThrowawayGroup: AnyObject {
    /// Process ONE wire row through the throwaway group. Returns the decoded
    /// row, or `nil` when the frame does not decrypt / authenticate, is our
    /// own echo, or is a control frame (fail-closed skip — never a plaintext
    /// guess). MUST NOT persist any MLS state.
    func process(_ payload: Data) -> NseProcessedRow?

    /// ADR-0053 client contract point 9 — the `x-lakko-ds-proof` header for
    /// one read of this group's journal, signed with the access key the
    /// throwaway group derives at its restored epoch (`dsAccessSigner`), or
    /// `nil` when it cannot be derived (the read then goes unsigned and a
    /// claimed journal refuses it — the generic banner). Signing is not a
    /// write to group state: no claim, no registration (C-59).
    func dsProofHeader(method: String, path: String, query: String, body: Data) -> String?

    /// Discard the throwaway handles. MUST NOT persist / snapshot.
    func close()
}

/// READ-ONLY injected capabilities. Every method is a read or a pure compute
/// — there is deliberately NO write path (no snapshot persist, no cursor
/// advance). The protocol simply cannot persist, which is the structural
/// enforcement of the read-only-throwaway discipline.
public protocol NsePushDecryptDeps {
    /// ADR-0042 (amended 2026-09-30) — the ONE account this wake may be
    /// handled for, or `nil` when it may not be handled at all.
    ///
    /// Non-`nil` only when the app recorded an account binding at push setup
    /// and the wake carries a well-formed `s` (``PushAccountBinding``). A
    /// device has at most one active account and never shows another
    /// account's push, so every later read — route, seed, snapshot, cursor,
    /// name, mute — is for this account and no other. READ-ONLY.
    func boundAccount(wakeSubscriptionIdHex: String?) -> String?

    /// ADR-0042 Sprint 4 with ADR-0078 — reverse-map an opaque `push_gid`
    /// (lowercase hex, the wake's `g`) to the real `group_id` it routes for
    /// `accountIdB64`, via the PLAINTEXT App-Group reverse index
    /// (`PushRoutingStore.lookupRoute`), and only when the account's
    /// subscription id for that row equals the wake's `s`. A row another
    /// account wrote, or a subscription this account does not hold, is a
    /// miss, and a miss is refused. READ-ONLY.
    func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String?

    /// The 64-byte AccountIdentity (BIP39) seed for `accountIdB64`, or `nil`.
    func loadIdentitySeed(accountIdB64: String) -> Data?

    /// The last-persisted MLS provider snapshot for `accountIdB64`, or `nil`.
    /// Only ever READ — never written back.
    func loadSnapshot(accountIdB64: String) -> Data?

    /// Build a THROWAWAY `Identity` + `Group` from the snapshot. `nil` when
    /// the account is not a member of the group / the restore fails.
    func openThrowawayGroup(
        seed: Data,
        snapshot: Data,
        groupIdBytes: Data
    ) throws -> NseThrowawayGroup?

    /// READ-ONLY DS group-journal fetch of rows strictly after `afterId`
    /// (ascending id order), signed through `group` (ADR-0053 point 9). A
    /// refused proof throws, which the caller turns into the generic banner.
    func dsFetch(
        groupIdBytes: Data,
        afterId: Int64,
        group: NseThrowawayGroup
    ) async throws -> [NseFetchedRow]

    /// The app's last-persisted per-group cursor (READ-ONLY), or `nil`. Used
    /// as the fetch start so intervening commits replay and the epoch stays
    /// correct.
    func startCursor(accountIdB64: String, groupHex: String) -> Int64?

    /// `true` when the user has ARMED anti-censorship (ADR-0026), mirrored
    /// from the app's preference into the shared App Group suite (the NSE is
    /// a separate process and cannot read the app's private
    /// `UserDefaults.standard`). When armed, the NSE must make NO direct,
    /// fingerprintable connection to Lakko infra on a push wake — the decrypt
    /// short-circuits to the generic banner instead (ADR-0041 Finding-1).
    func isAntiCensorshipArmed() -> Bool

    /// `docs/ux/calls.md` section 2.1 — the conversation's display name, for a
    /// ringing lock screen, read from the App Group mirror the APP writes
    /// (``CallPeerNameMirror``). READ-ONLY, like every other method here: this
    /// process resolves no names and stores none (C-59).
    ///
    /// Defaulted so the decrypt core keeps compiling against a dependency
    /// object that predates calls; `nil` simply means the alert is unnamed.
    func callerName(accountIdB64: String, groupIdHex: String) -> String?

    /// `docs/ux/calls.md` section 2.1 — does this conversation carry the mute
    /// flag? Read from the App Group mirror the APP writes
    /// (``ConversationMuteMirror``); this process holds no thread flags and
    /// writes none (C-59).
    ///
    /// Defaulted `false`, and the direction is the decision: a missing mirror,
    /// an unprovisioned suite or a dependency object that predates mute all
    /// answer "not muted", so the call rings. Failing the other way would
    /// swallow calls because a file was not there.
    func isMuted(accountIdB64: String, groupIdHex: String) -> Bool

    /// ADR-0073 R.5 — the conversation `groupIdHex` belongs to, hex: the
    /// group itself unless it continues another conversation. What a
    /// notification is grouped under, so a conversation that moved to a new
    /// group keeps one stack. READ-ONLY, from the App Group lineage mirror
    /// (C-59).
    func conversationHex(accountIdB64: String, groupIdHex: String) -> String

    /// ADR-0073 R.8 — is `senderKeyB64` a key an accepted reset retired?
    /// READ-ONLY, from the App Group mirror the APP writes (the pins live in
    /// the app's own store, which this process does not open). Defaulted
    /// `false`: a missing mirror drops nothing.
    func isSupersededSender(accountIdB64: String, senderKeyB64: String?) -> Bool

    /// ADR-0073 R.7 case 1 — is this chat row a re-send whose original the
    /// app already holds: the named row exists in an OLDER segment of the
    /// SAME conversation and is by the SAME account as the sender? READ-ONLY:
    /// the message log the app wrote and the lineage mirror (C-59). Defaulted
    /// `false`: when the facts cannot be read the row is an ordinary message.
    func holdsResendOriginal(
        accountIdB64: String, receivedGroupIdHex: String,
        origin: NseResendOrigin, senderKeyB64: String?
    ) -> Bool
}

public extension NsePushDecryptDeps {
    func isSupersededSender(accountIdB64: String, senderKeyB64: String?) -> Bool { false }
    func holdsResendOriginal(
        accountIdB64: String, receivedGroupIdHex: String,
        origin: NseResendOrigin, senderKeyB64: String?
    ) -> Bool { false }
    func callerName(accountIdB64: String, groupIdHex: String) -> String? { nil }
    func isMuted(accountIdB64: String, groupIdHex: String) -> Bool { false }
    func conversationHex(accountIdB64: String, groupIdHex: String) -> String { groupIdHex }
}

/// Produce a real decrypted notification preview for a GroupMessage wake, or
/// `nil` when the background path cannot (safely) yield one — in which case
/// the NSE shows its generic "New message" fallback.
///
/// Never throws for a "cannot decrypt" reason; every such path returns `nil`.
/// (A programming error in a mock could still throw in tests — the NSE wraps
/// the whole call in a `Task` + budget guard regardless.)
public func decryptPushPreview(
    _ wake: NsePushWake?,
    _ deps: NsePushDecryptDeps
) async -> NsePushPreview? {
    if case let .preview(preview) = await decryptPushOutcome(wake, deps) { return preview }
    return nil
}

/// Everything a wake can turn into: a message preview, a ringing call, a call
/// that has just ended, or nothing.
///
/// The superset of ``decryptPushPreview(_:_:)``, which stays as the name for
/// the message half.
///
/// ## Why a call outranks a message
///
/// One fetch can bring back both a chat message and a call offer. The call
/// wins, whichever is newer: a call is an interruption that expires in seconds
/// (`docs/ux/calls.md` section 1) and the message is still there afterwards,
/// while a call announced 40 seconds late is worse than not announced at all.
///
/// A call that is already over never rings: within one batch an `end` for a
/// call supersedes its own `offer`, which is the common case for a caller who
/// gave up while this device was asleep.
/// `nowMs` is the clock the ring window is measured against. Defaulted to this
/// device's, and a parameter only so a test can place an offer on either side
/// of the window; the AGE it is compared with still comes from the DS row's own
/// server-assigned stamp, never from anything a caller wrote.
public func decryptPushOutcome(
    _ wake: NsePushWake?,
    _ deps: NsePushDecryptDeps,
    nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
) async -> NsePushOutcome {
    // 0. Anti-censorship gate (ADR-0041 Finding-1). If the user has ARMED
    //    anti-censorship (to avoid direct connections to Lakko infra in a
    //    hostile network), the NSE must NOT make a direct, fingerprintable DS
    //    connection on a push wake. Short-circuit BEFORE any store read or
    //    network fetch and return nothing, so the handler shows the generic
    //    "New message" banner — the armed user trades the rich preview for
    //    no-direct-connection, the correct anti-censorship posture. The flag
    //    is mirrored from the app's preference into the shared App Group
    //    suite (the NSE cannot read the app's private UserDefaults.standard).
    if deps.isAntiCensorshipArmed() { return .none }

    // 1. Only GroupMessage wakes carry a 64-hex group + a pointer. A Wakeup /
    //    MailboxMessage wake is not handled in this slice.
    guard let rawGroupHex = wake?.groupHex, let pointer = wake?.pointer else {
        return .none
    }
    let groupHex = rawGroupHex.lowercased()
    guard groupHex.count == groupHexLen, isLowerHex(groupHex) else { return .none }

    // 2. The wake is handled only for the bound account (ADR-0042, amended
    //    2026-09-30, with ADR-0078): no binding or no well-formed `s` → the
    //    generic line, with no store read at all. From here on the bound
    //    account is the ONLY account this wake is decrypted as.
    guard let boundAccountIdB64 = deps.boundAccount(
        wakeSubscriptionIdHex: wake?.subscriptionIdHex),
          !boundAccountIdB64.isEmpty else {
        return .none
    }

    // 3. Resolve the wake's `g` to the real group_id, for the bound account:
    //    the notify path sends `g = hex(push_gid)`, an opaque routing id, so
    //    it is reverse-mapped through the App-Group index for (bound account,
    //    push_gid), and only when the row's subscription id is the wake's
    //    `s`. On a hit the row's REAL group_id drives everything downstream
    //    (snapshot, group open, DS fetch, cursor). A miss — an unknown
    //    push_gid, another account's row, a subscription this account does not
    //    hold, an unprovisioned suite, a malformed row — is the generic banner
    //    (fail-closed).
    guard let routed = routedGroup(deps.lookupRoute(
        accountIdB64: boundAccountIdB64, pushGidHex: groupHex,
        subscriptionIdHex: wake?.subscriptionIdHex)) else {
        return .none
    }
    let account = NseResolvedAccount(accountIdB64: boundAccountIdB64, groupIdBytes: routed.bytes)
    let realGroupHex = routed.hex

    // 4. The AccountIdentity seed → the throwaway Identity.
    guard var seed = deps.loadIdentitySeed(accountIdB64: account.accountIdB64) else {
        return .none
    }

    // 5. The last-persisted MLS provider snapshot (the ratchet state the
    //    foreground app relies on — we only READ it).
    guard let snapshot = deps.loadSnapshot(accountIdB64: account.accountIdB64) else {
        // Zero our seed view before bailing (the identity was never built).
        seed.resetBytes(in: 0..<seed.count)
        return .none
    }

    // 6. Build a TEMPORARY, in-memory group from the snapshot. Everything from
    //    here is throwaway — never snapshotted back.
    let opened: NseThrowawayGroup?
    do {
        opened = try deps.openThrowawayGroup(
            seed: seed,
            snapshot: snapshot,
            groupIdBytes: account.groupIdBytes
        )
    } catch {
        opened = nil
    }
    // The throwaway Identity has copied the seed; zero our view of it
    // regardless of success (mirrors the seed-store caller contract).
    seed.resetBytes(in: 0..<seed.count)
    guard let group = opened else { return .none }

    defer {
        // Discard the throwaway MLS state. We NEVER snapshot it back — doing
        // so would desync the foreground app's ratchet.
        group.close()
    }

    // 7. Fetch DS rows after a READ-ONLY start cursor. Prefer the app's
    //    persisted per-group cursor as the fetch FLOOR: the app has durably
    //    processed everything up to it, so we replay exactly what it has not
    //    seen — and no more. Flooring at the cursor (instead of overriding it
    //    only when `persisted < pointer`) also caps a hostile push-gw
    //    `pointer` (e.g. p=1) from forcing a full-journal replay through the
    //    throwaway group (CPU/battery/bandwidth amplification — ADR-0041
    //    Finding-2). Without a persisted cursor, fall back to just the
    //    message that triggered the wake (`pointer - 1`, clamped at 0).
    let after: Int64
    if let persisted = deps.startCursor(accountIdB64: account.accountIdB64, groupHex: realGroupHex) {
        after = max(persisted, 0)
    } else {
        after = pointer > 0 ? pointer - 1 : 0
    }

    let rows: [NseFetchedRow]
    do {
        rows = try await deps.dsFetch(
            groupIdBytes: account.groupIdBytes, afterId: after, group: group)
    } catch {
        return .none
    }

    // 8. Process each row through the TEMP group and keep the NEWEST frame
    //    that yields a user-visible preview. A frame that will not decrypt /
    //    authenticate (wrong epoch, our own echo, a control frame, a
    //    role-refused commit) is skipped — never guessed. NOTE: `process`
    //    mutates the THROWAWAY provider ONLY; it is never persisted.
    //    A call-signaling row is classified separately and OUTRANKS a message
    //    (see this function's docs): the newest call event decides, and an
    //    `end` for a call cancels its own `offer` from the same batch.
    // ADR-0073 R.5 — notifications are grouped by CONVERSATION: a group
    // that continues one stacks with it.
    let conversationTag = deps.conversationHex(
        accountIdB64: account.accountIdB64, groupIdHex: realGroupHex)
    var latest: NsePushPreview?
    var latestCall: (row: NseProcessedRow, createdAt: String)?
    // Did the batch hold a row the app shows nothing for?
    var silenced = false
    for row in rows {
        guard let processed = group.process(row.payload) else { continue }
        // ADR-0073 R.8 — nothing from a superseded sender acts: its call does
        // not ring and its message is not previewed (the app drops the same
        // row before storage). Asked before the kind, so a kind added later
        // is covered.
        if nseDropsSilently(
            processed, accountIdB64: account.accountIdB64, receivedGroupIdHex: realGroupHex, deps: deps
        ) {
            silenced = true
            continue
        }
        switch processed.kind {
        case .callOffer, .callEnd:
            latestCall = (row: processed, createdAt: row.createdAt)
        default:
            guard let body = previewBody(of: processed) else { continue }
            latest = NsePushPreview(
                title: "Lakko", body: body, tag: conversationTag,
                movedConversation: conversationTag.caseInsensitiveCompare(realGroupHex) != .orderedSame)
        }
    }
    if let latestCall, let callIdHex = latestCall.row.callIdHex {
        switch latestCall.row.kind {
        case .callOffer(let video):
            let title = deps.callerName(accountIdB64: account.accountIdB64, groupIdHex: realGroupHex) ?? nseUnknownCallerTitle
            // `docs/ux/calls.md` section 2.1, through the one rule every
            // client runs. The age is the DS row's own stamp — this fetch is a
            // catch-up from the app's cursor, so the newest call row here can
            // easily be an offer from a call that is long over.
            let verdict = nseRingVerdict(
                muted: deps.isMuted(accountIdB64: account.accountIdB64, groupIdHex: realGroupHex),
                createdAt: latestCall.createdAt,
                nowMs: nowMs)
            switch verdict {
            case .ring:
                return .callOffer(NseCallAlert(
                    title: title,
                    body: nseCallKindLabel(video: video),
                    callIdHex: callIdHex,
                    tag: conversationTag
                ))
            case .silent:
                return .callSilent(NseCallAlert(
                    title: title,
                    body: nseCallKindLabel(video: video),
                    callIdHex: callIdHex,
                    tag: conversationTag
                ))
            case .tooLate:
                return .callSilent(NseCallAlert(
                    title: title,
                    body: nseMissedCallBody,
                    callIdHex: callIdHex,
                    tag: conversationTag
                ))
            }
        case .callEnd:
            return .callEnded(callIdHex: callIdHex)
        default:
            break
        }
    }
    if let latest { return .preview(latest) }
    // Only rows the app shows nothing for: the quietest delivery there is.
    // (A batch with nothing readable at all stays `.none`: the generic line,
    // because an unread message may be behind it.)
    if silenced { return .quiet(tag: conversationTag) }
    return .none
}

/// ADR-0073 R.8 / R.7 case 1 — the rows the background reader shows nothing
/// for, the same two silent drops the app's receive paths apply
/// (`ChatRepository.silentlyFiltered`):
///
/// | the row                                                              | dropped |
/// |----------------------------------------------------------------------|---------|
/// | any kind, from a key an accepted reset retired                       | yes     |
/// | a chat row carrying a re-send marker whose original the app holds    | yes     |
/// | a chat row carrying a marker that matches nothing the app holds      | no      |
/// | anything else                                                        | no      |
///
/// Both answers come from the dependency object, which only reads (C-59).
public func nseDropsSilently(
    _ row: NseProcessedRow, accountIdB64: String, receivedGroupIdHex: String,
    deps: NsePushDecryptDeps
) -> Bool {
    if deps.isSupersededSender(accountIdB64: accountIdB64, senderKeyB64: row.senderKeyB64) {
        return true
    }
    if row.kind == .chat, let origin = row.resendOrigin {
        return deps.holdsResendOriginal(
            accountIdB64: accountIdB64, receivedGroupIdHex: receivedGroupIdHex,
            origin: origin, senderKeyB64: row.senderKeyB64)
    }
    return false
}

/// ADR-0073 R.5 — the notification stack a wake belongs to, known BEFORE any
/// decrypt: the conversation of the group the wake's routing id names.
///
/// A wake carries `g`, an opaque per-group routing id that also changes when
/// the routing secret rotates (ADR-0043). Grouping by it splits one
/// conversation's notifications at every rotation and at the move to a
/// successor group. The stack is therefore the CONVERSATION's id (the lineage
/// root's group id, hex), which survives both: notifications delivered before
/// a conversation moved and those of its successor carry the same thread.
///
/// `nil` when the wake cannot be routed (no binding, an unknown routing id):
/// the caller keeps the wake's own `g`. Reads only (C-59); no seed, no
/// snapshot, no network.
public func nseConversationThread(_ wake: NsePushWake?, _ deps: NsePushDecryptDeps) -> String? {
    guard let rawGroupHex = wake?.groupHex else { return nil }
    let groupHex = rawGroupHex.lowercased()
    guard groupHex.count == groupHexLen, isLowerHex(groupHex) else { return nil }
    guard let accountIdB64 = deps.boundAccount(wakeSubscriptionIdHex: wake?.subscriptionIdHex),
          !accountIdB64.isEmpty,
          let routed = routedGroup(deps.lookupRoute(
            accountIdB64: accountIdB64, pushGidHex: groupHex,
            subscriptionIdHex: wake?.subscriptionIdHex)) else { return nil }
    return deps.conversationHex(accountIdB64: accountIdB64, groupIdHex: routed.hex).lowercased()
}

// MARK: - Ringing

/// What an arriving offer does in the extension. A mirror of
/// `LakkoCore.RingDecision`, for the one process that cannot ask it.
public enum NseRingVerdict: Equatable {
    /// Ring: a call-shaped alert with a ringtone.
    case ring
    /// Record it, interrupt nobody. The conversation is muted.
    case silent
    /// The caller has already given up. A missed call, shown passively.
    case tooLate
}

/// `docs/ux/calls.md` section 2.1, applied in the extension.
///
/// ## Why this is a second copy of a rule that has one home
///
/// It should be `LakkoCore.ringDecision(muted:offerAgeMs:)` and it is not,
/// for a build-shape reason rather than a design one: the notification-service
/// extension links `LakkoCoreExtension` — the slim decrypt-only FFI of
/// ADR-0041 Phase 2, which exists because the full framework does not fit the
/// extension's memory budget — and that surface does not carry the ring rule.
/// The app and the CallKit path both call the Rust function directly; only
/// this process cannot.
///
/// So the copy is kept honest from the outside instead: `NseRingRuleTests`
/// runs this function and `LakkoCore.ringDecision` side by side over the
/// window's boundary and both mute values, in the test target, which links the
/// full framework. A change to the Rust rule that this does not follow fails
/// that test rather than a phone. Exporting the rule on the slim surface would
/// retire the copy and the test with it.
///
/// `createdAt` is the DS row's server stamp, never a local clock: an age that
/// cannot be read is treated as past the window, because an age that cannot be
/// established is not one the window can be measured against.
public func nseRingVerdict(muted: Bool, createdAt: String, nowMs: Int64) -> NseRingVerdict {
    guard let ageMs = DsTimestamp.ageMs(createdAt: createdAt, nowMs: nowMs) else {
        return .tooLate
    }
    if ageMs >= nseCallRingWindowMs { return .tooLate }
    return muted ? .silent : .ring
}

/// The ring window in milliseconds, as this process knows it. See
/// ``nseRingVerdict(muted:createdAt:nowMs:)`` for why it is written here and
/// not read from the core, and for the test that keeps it equal.
public let nseCallRingWindowMs: UInt64 = 45_000

// MARK: - Notification words (ADR-0061)

/// Where the extension's notification words come from. The app renders them in
/// the interface language into the App Group suite
/// (`SharedStorage.notificationWordsKey`); ``NotificationService`` points this
/// at that store when it wakes. The default answers nothing, so a caller that
/// never set it (a unit test) reads English.
public var nseNotificationWordLookup: (String) -> String? = { _ in nil }

/// One notification word in the interface language, or `english` when the app
/// has not stored it: the fallback is per word, never all-or-nothing.
func nseWord(_ key: String, english: String) -> String {
    nseNotificationWordLookup(key) ?? english
}

/// ADR-0061 section 6 — the preview of a message written in a newer chat
/// wire version, in the interface language (English when the app has not
/// stored the word).
public func nseUnsupportedMessageLine() -> String {
    nseWord("chat.message.unsupported", english: "This message needs a newer version of Lakko.")
}

/// "Sent an attachment" / "Sent N attachments" in the interface language.
///
/// The extension has no plural rules, so the app stores one rendered line per
/// count under `<key>#<count>` (`SharedStorage.countedNotificationWordKey`,
/// up to `notificationCountedWordMax`); a count it did not render, or a store
/// the app has not written, reads English.
public func nseAttachmentsLine(count: Int) -> String {
    let key = "notification.message.attachments"
    if let line = nseNotificationWordLookup("\(key)#\(count)") { return line }
    return count == 1 ? "Sent an attachment" : "Sent \(count) attachments"
}

/// The body the server put on a wake, in the interface language when it is
/// one of the two generic placeholders `push-gw` sends (`New message`,
/// `Incoming call`); any other body is returned unchanged.
///
/// This is what a notification says when nothing could be decrypted: the
/// extension leaves the server's alert as it came, and the server knows no
/// language (invariant #5), so its English placeholder is swapped here.
public func nseGenericBody(_ serverBody: String) -> String {
    switch serverBody {
    case "New message": return nseWord("notification.message.new", english: serverBody)
    case "Incoming call": return nseWord("call.notification.incoming", english: serverBody)
    default: return serverBody
    }
}

/// What a passive call notification says when the call is already over.
public var nseMissedCallBody: String {
    nseWord("call.log.missed", english: "Missed call")
}

// MARK: - Call wording

/// What a ringing lock screen says when this device has no mirrored name for
/// the conversation.
///
/// The same words the app's own placeholder uses
/// (`IncomingCallReport.unknownCaller`), stated again here rather than shared:
/// the extension is a separate process that links neither the app module nor
/// the full FFI, and the alternative — compiling the app's file into a third
/// target — puts two copies of the same symbol in the unit-test target.
public var nseUnknownCallerTitle: String {
    nseWord("call.notification.incoming", english: "Incoming call")
}

/// `Voice call` / `Video call` (`docs/ux/calls.md` section 2.2). The
/// extension's copy of ``CallKindLabel``; see ``nseUnknownCallerTitle`` for why
/// it is a copy.
public func nseCallKindLabel(video: Bool) -> String {
    video
        ? nseWord("call.kind.video", english: "Video call")
        : nseWord("call.kind.voice", english: "Voice call")
}

/// What the extension shows for a call that has ENDED, given whether it found
/// its own alert still on screen.
///
/// The extension cannot decline to show anything: `contentHandler` must be
/// called, and skipping it DROPS the notification in a way iOS counts against
/// the app. So the question is only what to say, and the answer follows from
/// whether the offer alert was still sitting there:
///
///   * **It was** — nobody dealt with this call on this device, and the alert
///     has just been taken down. That is a missed call, and
///     `docs/ux/calls.md` section 6 says a missed call raises a notification.
///   * **It was not** — the call was answered or declined here (the app clears
///     the alert when it takes a call, ``CallKitBridge``), or it never showed.
///     There is nothing to tell the user, so the notification is `passive`:
///     iOS files it without lighting the screen or playing a sound. That is the
///     quietest an alert push can be made — an extension has no way to suppress
///     one outright.
public func nseEndedCallContent(
    hadDeliveredAlert: Bool,
    callerName: String?
) -> (title: String, body: String, passive: Bool) {
    if hadDeliveredAlert {
        return (title: callerName ?? nseUnknownCallerTitle, body: nseMissedCallBody, passive: false)
    }
    return (title: "", body: "", passive: true)
}

/// Classify a decrypted application body as call signaling, or `nil` when it is
/// not call signaling / cannot be read.
///
/// Two gates, in this order and for different reasons: the cheap byte peek
/// (``nseLooksLikeCallSignaling(_:)``) keeps an FFI call off the path for every
/// ordinary chat message, and the injected parser is the verdict. A `nil`
/// parser — the state of a build whose slim FFI does not carry the call parse
/// yet — makes every call row fall through to the service-envelope filter that
/// has always dropped them, so an incomplete build is quiet rather than wrong.
public func nseClassifyCallSignaling(
    _ bytes: Data,
    parser: NseCallSignalingParser?
) -> NseProcessedRow? {
    guard nseLooksLikeCallSignaling(bytes), let parser else { return nil }
    switch parser.parse(bytes) {
    case .offer(let callIdHex, let video):
        return NseProcessedRow(
            kind: .callOffer(video: video),
            text: "",
            attachmentCount: 0,
            callIdHex: callIdHex
        )
    case .end(let callIdHex):
        return NseProcessedRow(
            kind: .callEnd,
            text: "",
            attachmentCount: 0,
            callIdHex: callIdHex
        )
    case .other, nil:
        return nil
    }
}

/// Extract a user-visible preview string from a processed row, or `nil` for
/// anything that is not a renderable chat/edit body (so an older real message
/// stays the preview).
private func previewBody(of row: NseProcessedRow) -> String? {
    switch row.kind {
    // A normal chat message → its text (or an attachment hint).
    case .chat:
        if !row.text.isEmpty { return truncate(row.text) }
        if row.attachmentCount == 1 {
            return nseMediaLabel(kind: row.singleAttachmentKind) ?? nseAttachmentsLine(count: 1)
        }
        if row.attachmentCount > 1 { return nseAttachmentsLine(count: row.attachmentCount) }
        return nil
    // An edit rewrites a prior message → surface the new text.
    case .edit:
        return row.text.isEmpty ? nil : truncate(row.text)
    // delete / reaction / typing / read-receipt carry no standalone preview —
    // skip so an older real message stays the preview.
    case .delete, .reaction, .typing, .readReceipt:
        return nil
    // A service envelope this build cannot read (live walk 2026-09-29: a
    // profile v3 reached v2 clients). Never rendered, whatever text it carries.
    case .serviceEnvelope:
        return nil
    // A message from a newer version of Lakko: a real message, previewed as
    // the client's own sentence in the interface language.
    case .unsupported:
        return nseUnsupportedMessageLine()
    // A call is not a message preview — it is its own outcome, decided by the
    // caller before this function is reached.
    case .callOffer, .callEnd:
        return nil
    }
}

/// Validate a reverse-index row's group hex: lowercase it, require 64 hex
/// characters, and decode it. `nil` for a malformed row, which the caller
/// treats as a miss (never a crash, never a mis-route).
private func routedGroup(_ groupIdHex: String?) -> (hex: String, bytes: Data)? {
    guard let groupIdHex else { return nil }
    let hex = groupIdHex.lowercased()
    guard hex.count == groupHexLen, isLowerHex(hex), let bytes = hexToData(hex) else {
        return nil
    }
    return (hex: hex, bytes: bytes)
}

// MARK: - Service-envelope filter

/// The exact byte length of a v2 push-routing-secret distribution envelope
/// (ADR-0044): `domain(29) || version(1) || mint_epoch(8, BE) || secret(32)`.
private let pushRoutingEnvelopeLen = 70

/// First byte of the frozen 29-byte domain separator
/// `lakko/push-routing-secret/v1\0` — an ASCII `l`. Frozen forever (it is the
/// `incoming_envelope` dispatch prefix), so it is the one byte of this wire
/// format safe to peek at outside the codec.
private let pushRoutingEnvelopeFirstByte: UInt8 = 0x6C

/// First byte of the frozen 26-byte ADR-0045 committer-beacon domain separator
/// `lakko/committer-beacon/v1\0` — the same ASCII `l` every in-group envelope
/// domain opens with, and equally frozen (it is the `incoming_envelope`
/// dispatch prefix).
///
/// Kept as its OWN constant rather than shared with
/// ``pushRoutingEnvelopeFirstByte`` so retiring one wire format cannot silently
/// re-point the other's peek.
private let committerBeaconEnvelopeFirstByte: UInt8 = 0x6C

/// `true` when the bytes look like a prost-encoded `CallSignalingPayload`
/// (ADR-0009 call signaling).
///
/// Verbatim mirror of the foreground `ChatClient.looksLikeCallSignaling`: the
/// proto's first wire tag is one of `0x52` / `0x5A` / `0x62` / `0x6A`
/// (`CallSignalingPayload.body` oneof fields 10..13, the 1x1 bodies),
/// `0x72` / `0x7A` (`GroupCallStart` 14 / `GroupCallJoin` 15) or the two-byte
/// `0x82 0x01` (`GroupCallLeave` 16), while a chat envelope starts with `{`
/// (`0x7B`) — no overlap. Without the group tags a group-call body reached the
/// chat parser's lossy fallback and became a notification preview of garbage.
///
/// The group tags are ASCII `r` / `z`, so they are matched on their full
/// framing (``nseIsFramedGroupCallBody(_:tagLength:)``) rather than the first
/// byte — a first-byte rule would suppress every legacy plain-text message
/// starting with one of those letters.
///
/// This is the FULL authority for the call-signaling kind on both paths (there
/// is no prost decoder on the NSE's slim surface, and the foreground uses none
/// here either). It shares the foreground's known characteristic: a LEGACY
/// pre-1.19 plain-UTF-8 body whose first character happens to be `R` / `Z` /
/// `b` / `j` is classified as call signaling. Keeping the two paths byte-for-byte
/// identical is worth more than "improving" one of them — a divergence would
/// mean the notification and the timeline disagree about what arrived.
public func nseLooksLikeCallSignaling(_ bytes: Data) -> Bool {
    guard let first = bytes.first else { return false }
    switch first {
    case 0x52, 0x5A, 0x62, 0x6A:
        return true
    case 0x72, 0x7A:
        return nseIsFramedGroupCallBody(bytes, tagLength: 1)
    case 0x82:
        return bytes.count >= 2
            && bytes[bytes.startIndex + 1] == 0x01
            && nseIsFramedGroupCallBody(bytes, tagLength: 2)
    default:
        return false
    }
}

/// `tag || varint(len) || body`, where the body is EXACTLY the rest of the
/// payload and opens with the 16-byte `call_id` field (`0x0A 0x10`) every
/// group-call message carries first. Verbatim mirror of the foreground
/// `ChatClient.isFramedGroupCallBody`.
public func nseIsFramedGroupCallBody(_ bytes: Data, tagLength: Int) -> Bool {
    var index = bytes.startIndex + tagLength
    var length = 0
    var shift = 0
    while true {
        guard index < bytes.endIndex, shift <= 14 else { return false }
        let byte = bytes[index]
        index += 1
        length |= Int(byte & 0x7F) << shift
        if byte & 0x80 == 0 { break }
        shift += 7
    }
    guard length >= 18, bytes.endIndex - index == length else { return false }
    return bytes[index] == 0x0A && bytes[index + 1] == 0x10
}

/// Cheap PRE-FILTER for an ADR-0042 / ADR-0044 push-routing-secret envelope:
/// the fixed v2 length plus the first domain byte.
///
/// Mirror of the foreground `PushGroupRouting.tryDecodeSecretEnvelope` peek. It
/// is deliberately NOT authoritative — it exists only to keep an FFI call off
/// the hot path for every ordinary chat message. The verdict comes from the
/// Rust codec (`decodePushRoutingSecret`), whose domain + version + exact-length
/// gate is the single definition of this wire format; a Swift-side domain match
/// would silently drift on the next version bump, which is precisely the failure
/// class ADR-0044 exists to document. The two constants above are pinned by a
/// Rust golden test (`golden_pins_the_swift_prefilter_shape`) so a wire change
/// fails CI rather than silently widening this filter's blind spot.
public func nseLooksLikePushRoutingSecret(_ bytes: Data) -> Bool {
    bytes.count == pushRoutingEnvelopeLen && bytes.first == pushRoutingEnvelopeFirstByte
}

/// Cheap PRE-FILTER for an ADR-0045 committer-liveness beacon: the exact
/// envelope length plus the first domain byte — the same two-part shape as the
/// routing peek above.
///
/// ## Why the length arrives as a parameter
///
/// `envelopeLen` is the slim FFI's `committerBeaconEnvelopeLen()`, which derives
/// it from `lakko_contacts`' own domain + payload constants, so a wire revision
/// moves this peek automatically. It is INJECTED rather than read here for the
/// same reason `nseIsServiceEnvelope` injects its decoders: this file is
/// compiled into the FFI-free unit-test target, so it cannot call the FFI
/// itself. Hand-copying `27` into this file was the alternative and is exactly
/// what the export exists to avoid — an unpinned wire literal, the drift class
/// ADR-0044 documents, failing in the DANGEROUS direction if it went stale
/// (a too-narrow peek silently stops suppressing beacons, restoring the exact
/// defect this filter closes). The Rust golden
/// `golden_pins_the_swift_prefilter_shape` pins the exported value against the
/// composed wire, so a drift fails Rust CI rather than a notification.
///
/// ## Why an exact length is safe here
///
/// A beacon carries NO FIELDS — no epoch, no identity, no timestamp — so every
/// beacon on the wire is byte-identical and this length is a TOTAL test of the
/// size; it can never be too narrow for a legitimate beacon. The peek is still
/// only a hot-path guard, never a verdict: `decodeCommitterBeacon` remains the
/// complete authority, and this merely keeps an FFI call off the path for every
/// ordinary chat message that happens to begin with `l`.
public func nseLooksLikeCommitterBeacon(_ bytes: Data, envelopeLen: Int) -> Bool {
    bytes.count == envelopeLen && bytes.first == committerBeaconEnvelopeFirstByte
}

/// First byte of the ADR-0048 profile domain separator `lakko/profile/v1\0` —
/// the same ASCII `l` as the other in-group envelope domains, kept as its own
/// constant so retiring one wire format cannot re-point another's peek.
private let profileEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0048 profile envelope: a length inside the
/// well-formed range plus the first domain byte.
///
/// A profile, unlike a beacon, has variable length (a name and a username of
/// their own lengths), so the peek is a RANGE. Both bounds arrive from the slim
/// FFI (`profileEnvelopeMinLen()` / `profileEnvelopeMaxLen()`), which derives
/// them from `lakko_contacts`' own constants — never copied here. A stale upper
/// bound would fail in the dangerous direction (long names would stop being
/// suppressed), which is why it is injected rather than written as a literal.
/// The peek decides nothing; `decodeProfileEnvelope` is the verdict.
public func nseLooksLikeProfile(_ bytes: Data, envelopeLens: ClosedRange<Int>) -> Bool {
    envelopeLens.contains(bytes.count) && bytes.first == profileEnvelopeFirstByte
}

/// First byte of the ADR-0051 group-name domain separator
/// `lakko/group-name/v1\0` — the same ASCII `l` as every other in-group
/// envelope domain, kept as its own constant for the same reason as the
/// others: retiring one wire format must not re-point another's peek.
private let groupNameEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0051 group-name envelope: a length inside the
/// well-formed range plus the first domain byte.
///
/// Variable length like the profile (a name of its own length, or none — an
/// empty name is the "clear" envelope), so the peek is a RANGE whose bounds
/// arrive from the slim FFI (`groupNameEnvelopeMinLen()` /
/// `groupNameEnvelopeMaxLen()`), never copied here: a stale upper bound would
/// fail in the dangerous direction (a long name would stop being suppressed
/// and land in a notification as `lakko/group-name/v1` plus binary). The peek
/// decides nothing; `decodeGroupNameEnvelope` is the verdict.
public func nseLooksLikeGroupName(_ bytes: Data, envelopeLens: ClosedRange<Int>) -> Bool {
    envelopeLens.contains(bytes.count) && bytes.first == groupNameEnvelopeFirstByte
}

/// Cheap PRE-FILTER for an ADR-0073 group-closed envelope (UX 16.5): a length
/// inside the two well-formed lengths (32 without a successor, 64 with one)
/// plus the first domain byte. The bounds arrive from the slim FFI
/// (`groupClosedEnvelopeMinLen()` / `groupClosedEnvelopeMaxLen()`), never
/// copied here, for the group-name arm's reason: a stale bound fails in the
/// dangerous direction and a closed notice lands in a notification as
/// `lakko/group-closed/v1` plus binary. The peek decides nothing;
/// `decodeGroupClosedEnvelope` is the verdict.
public func nseLooksLikeGroupClosed(_ bytes: Data, envelopeLens: ClosedRange<Int>) -> Bool {
    envelopeLens.contains(bytes.count) && bytes.first == groupNameEnvelopeFirstByte
}

/// First byte of the ADR-0052 pin domain separator `lakko/pin/v1\0` — the
/// same ASCII `l` as every other in-group envelope domain, kept as its own
/// constant for the same reason as the others: retiring one wire format must
/// not re-point another's peek.
private let pinEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0052 shared-pin envelope: the exact length plus
/// the first domain byte, the beacon's shape. The envelope has no variable
/// field (a version, a stamp, an action and a message id), so the length is a
/// total test of the size; it is injected (the slim FFI's `pinEnvelopeLen()`)
/// rather than written as `31` because a stale literal fails in the dangerous
/// direction — a too-narrow peek silently stops suppressing pins and a
/// notification reads `lakko/pin/v1` plus binary. The peek decides nothing;
/// `decodePinEnvelope` is the verdict.
public func nseLooksLikePin(_ bytes: Data, envelopeLen: Int) -> Bool {
    bytes.count == envelopeLen && bytes.first == pinEnvelopeFirstByte
}

/// First byte of the ADR-0074 key-rotation domain separator
/// (`"lakko/key-rotation/v1\0"`): the same ASCII `l` as every other envelope
/// domain, its own constant for the same reason as the others.
private let keyRotationEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap, allocation-free pre-filter for the ADR-0074 key-rotation envelope:
/// the exact length (192 bytes, injected from the slim FFI's
/// `keyRotationEnvelopeLen()` rather than written here, because a stale
/// literal silently stops suppressing it) and the first domain byte. The peek
/// decides nothing; `isKeyRotationEnvelope` is the verdict.
public func nseLooksLikeKeyRotation(_ bytes: Data, envelopeLen: Int) -> Bool {
    envelopeLen > 0 && bytes.count == envelopeLen && bytes.first == keyRotationEnvelopeFirstByte
}

/// `true` when an already-DECRYPTED MLS application body is an in-group SERVICE
/// envelope that must NEVER be rendered as a notification.
///
/// Service envelopes ride the SAME group journal as chat. `parseBodyEnvelope`
/// has a deliberate legacy fallback that degrades non-JSON bytes to a `Chat`
/// body via `from_utf8_lossy` (pre-1.19 wire compatibility), so without this
/// filter a service envelope surfaces as a notification containing its domain
/// string plus raw binary. The foreground receive loop
/// (`ChatClient.toEvent`) has always filtered these; this is the background
/// half of the same gate, in the same ORDER (call signaling first, then push
/// routing) so the two paths cannot disagree.
///
/// Since `docs/ux/calls.md`, the caller asks ``nseClassifyCallSignaling(_:parser:)``
/// BEFORE this function, and a call it can read becomes a ring rather than a
/// drop. The call arm below is what remains for the rows that classifier cannot
/// use — a frame it could not read, or a build with no parser at all — and it
/// keeps doing what it always did with them: suppress, so an unreadable frame
/// is quiet rather than rendered as a smear of SDP.
///
/// Three kinds, each one arm below: call signaling, the ADR-0042 / ADR-0044
/// push-routing secret, and the ADR-0045 committer-liveness beacon. The beacon
/// is the MOST reachable of the three, not the least: a routing secret is
/// broadcast only on a membership change, while a beacon is emitted by every
/// owner and admin every 60 s, so in an active group the newest journal row —
/// the one this handler previews — is frequently a beacon.
///
/// `decodesAsPushRoutingSecret` / `decodesAsCommitterBeacon` /
/// `committerBeaconEnvelopeLen` are injected rather than read directly so this
/// function stays FFI-free and compiles into the unit-test target; the NSE
/// passes the slim FFI's `decodePushRoutingSecret`, `decodeCommitterBeacon` and
/// `committerBeaconEnvelopeLen()`. All three are REQUIRED, with no default: a
/// default decoder of "never this kind" would let a caller silently reinstate
/// the defect, and a default length would re-introduce the hand-copied wire
/// constant the export exists to remove.
///
/// Returning `true` makes the caller drop the row, and the extraction loop then
/// falls back naturally to the newest REAL chat message (or, with none, to the
/// handler's generic "New message" banner).
///
/// ADR-0048 adds a fourth kind, the profile envelope. It is suppressed and
/// NOTHING else: the NSE never stores the profile (C-59 — this process writes
/// no durable state) and has no roster to attribute it with anyway. The
/// foreground app stores it on its own receive path.
///
/// ADR-0051 adds a fifth, the group-name envelope, with the same treatment:
/// recognised and suppressed, never stored (the NSE has no role table to
/// gate it with, and no durable store to write). A recognised envelope makes
/// the caller fall back to the newest real message or the generic body.
///
/// ADR-0048 §2 trigger 4 adds a sixth, the profile REQUEST — a newcomer's
/// field-less "introduce yourselves". Recognised and suppressed, and NOT
/// answered: the answer is an MLS application message (a ratchet advance
/// C-59 forbids this process), so the foreground app answers it on its own
/// receive path. Like the beacon it is fixed-length, and the length arrives
/// from the slim FFI (`profileRequestEnvelopeLen()`) for the same reason.
///
/// ADR-0052 decision 3 adds a seventh, the shared-pin envelope (a pin or an
/// unpin of one message), with the group name's treatment: recognised and
/// suppressed, never stored (the NSE has no role table or people count to
/// gate it with, and no durable store to write). Fixed-length like the
/// beacon; the length arrives from the slim FFI (`pinEnvelopeLen()`).
///
/// ADR-0057 adds an eighth, the registrar-seed envelope a journal root sends
/// a member (205 bytes since v2: the clear header carries the registrar public
/// key; the seed is sealed to the member's account key). Recognised and
/// suppressed, never opened or stored: the NSE writes no durable state
/// (C-59). Fixed-length; the length arrives from the slim FFI
/// (`registrarSeedEnvelopeLen()`).
public func nseIsServiceEnvelope(
    _ bytes: Data,
    decodesAsPushRoutingSecret: (Data) -> Bool,
    decodesAsCommitterBeacon: (Data) -> Bool,
    committerBeaconEnvelopeLen: Int,
    decodesAsProfile: (Data) -> Bool,
    profileEnvelopeLens: ClosedRange<Int>,
    decodesAsGroupName: (Data) -> Bool,
    groupNameEnvelopeLens: ClosedRange<Int>,
    decodesAsProfileRequest: (Data) -> Bool,
    profileRequestEnvelopeLen: Int,
    decodesAsPin: (Data) -> Bool,
    pinEnvelopeLen: Int,
    decodesAsRegistrarSeed: (Data) -> Bool,
    registrarSeedEnvelopeLen: Int,
    decodesAsScopedDiscovery: (Data) -> Bool,
    scopedDiscoveryEnvelopeLens: ClosedRange<Int>,
    decodesAsSharedChannels: (Data) -> Bool,
    sharedChannelsEnvelopeLens: ClosedRange<Int>,
    // ADR-0073 phase 2 — the ninth service envelope, the group-closed notice.
    // Defaulted so the older call sites (and the tests that stub every
    // decoder) keep compiling; a stub that says "never" only ever lets a
    // notice through, which is what those sites did before the arm existed.
    decodesAsGroupClosed: (Data) -> Bool = { _ in false },
    groupClosedEnvelopeLens: ClosedRange<Int> = 0...0,
    // ADR-0074 section 3.3 — the tenth, the account's `key-rotation`
    // envelope. It is meaningful only in the account's own DeviceGroup, which
    // this process never reads; here it is recognised and suppressed like
    // every service envelope, and NOT decoded: it carries the account's next
    // seed, so the slim FFI answers a yes / no (`isKeyRotationEnvelope`) and
    // the bytes never become a Swift value. Defaulted for the reason above.
    isKeyRotation: (Data) -> Bool = { _ in false },
    keyRotationEnvelopeLen: Int = 0
) -> Bool {
    if nseLooksLikeCallSignaling(bytes) { return true }
    if nseLooksLikeKeyRotation(bytes, envelopeLen: keyRotationEnvelopeLen), isKeyRotation(bytes) {
        return true
    }
    if nseLooksLikePushRoutingSecret(bytes), decodesAsPushRoutingSecret(bytes) { return true }
    if nseLooksLikeCommitterBeacon(bytes, envelopeLen: committerBeaconEnvelopeLen),
       decodesAsCommitterBeacon(bytes) {
        return true
    }
    if nseLooksLikeProfile(bytes, envelopeLens: profileEnvelopeLens), decodesAsProfile(bytes) {
        return true
    }
    if nseLooksLikeGroupName(bytes, envelopeLens: groupNameEnvelopeLens), decodesAsGroupName(bytes) {
        return true
    }
    if nseLooksLikeGroupClosed(bytes, envelopeLens: groupClosedEnvelopeLens), decodesAsGroupClosed(bytes) {
        return true
    }
    if nseLooksLikeProfileRequest(bytes, envelopeLen: profileRequestEnvelopeLen),
       decodesAsProfileRequest(bytes) {
        return true
    }
    if nseLooksLikePin(bytes, envelopeLen: pinEnvelopeLen), decodesAsPin(bytes) {
        return true
    }
    if nseLooksLikeRegistrarSeed(bytes, envelopeLen: registrarSeedEnvelopeLen),
       decodesAsRegistrarSeed(bytes) {
        return true
    }
    if nseLooksLikeScopedDiscovery(bytes, envelopeLens: scopedDiscoveryEnvelopeLens),
       decodesAsScopedDiscovery(bytes) {
        return true
    }
    if nseLooksLikeSharedChannels(bytes, envelopeLens: sharedChannelsEnvelopeLens),
       decodesAsSharedChannels(bytes) {
        return true
    }
    return false
}

/// First byte of the ADR-0057 registrar-seed domain separator — the same ASCII
/// `l` as every other in-group envelope domain, its own constant for the same
/// reason as the others.
private let registrarSeedEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0057 registrar-seed envelope: the exact length
/// (injected from the slim FFI's `registrarSeedEnvelopeLen()`, 205, never a
/// hand-copied literal) plus the first domain byte. The peek decides nothing;
/// `decodeRegistrarSeedEnvelope` is the verdict.
public func nseLooksLikeRegistrarSeed(_ bytes: Data, envelopeLen: Int) -> Bool {
    bytes.count == envelopeLen && bytes.first == registrarSeedEnvelopeFirstByte
}

/// First byte of every ADR-0060 domain separator (`lakko/introduction-offer/…`,
/// `lakko/introductions/…`, `lakko/visibility/…`, `lakko/reach-state/…`,
/// `lakko/committer-delegation/…`) — the same ASCII `l` as every other in-group
/// envelope, kept as its own constant for the reason the others give.
private let scopedDiscoveryEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for the five ADR-0060 "who can find me" envelopes: the
/// first domain byte plus the length range the slim FFI exports
/// (`scopedDiscoveryEnvelopeMinLen()` / `MaxLen()`), never a hand-copied
/// number — a range too narrow fails in the dangerous direction and lets an
/// introduction bundle reach a notification body as its domain plus binary.
/// The peek decides nothing; `decodeScopedDiscoveryEnvelope` is the verdict,
/// and it returns NOTHING of the envelope (C-59: the NSE never learns a grant,
/// a setting or a delegation).
public func nseLooksLikeScopedDiscovery(_ bytes: Data, envelopeLens: ClosedRange<Int>) -> Bool {
    envelopeLens.contains(bytes.count) && bytes.first == scopedDiscoveryEnvelopeFirstByte
}

/// First byte of the ADR-0059 shared-channels domain separator
/// `lakko/shared-channels/v1\0` — the same ASCII `l` as every other in-group
/// envelope domain, kept as its own constant for the same reason as the
/// others: retiring one wire format must not re-point another's peek.
private let sharedChannelsEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0059 shared-channels list: a length inside the
/// well-formed range plus the first domain byte. Variable length (up to 32
/// channels of their own handle and name lengths), so the peek is a RANGE whose
/// bounds arrive from the slim FFI (`sharedChannelsEnvelopeMinLen()` /
/// `sharedChannelsEnvelopeMaxLen()`), never copied here: a stale upper bound
/// fails in the dangerous direction (a long list would stop being suppressed
/// and land in a notification as `lakko/shared-channels/v1` plus the channel
/// names). The peek decides nothing; the injected decoder is the verdict.
///
/// ADR-0059 section 6 adds this eighth kind with the pin's treatment:
/// recognised and suppressed, never stored (C-59). Which channels a person
/// follows is not the extension's to learn: the slim decode returns no fields.
public func nseLooksLikeSharedChannels(_ bytes: Data, envelopeLens: ClosedRange<Int>) -> Bool {
    envelopeLens.contains(bytes.count) && bytes.first == sharedChannelsEnvelopeFirstByte
}

/// First byte of the frozen 25-byte ADR-0048 profile-request domain separator
/// `lakko/profile-request/v1\0` — the same ASCII `l` as every other in-group
/// envelope domain, kept as its own constant for the same reason as the
/// others: retiring one wire format must not re-point another's peek.
private let profileRequestEnvelopeFirstByte: UInt8 = 0x6C

/// Cheap PRE-FILTER for an ADR-0048 profile request: the exact length plus the
/// first domain byte, the beacon's shape exactly. The request carries no
/// fields, so every request on the wire is byte-identical and the length is a
/// total test of the size; the length is injected (the slim FFI's
/// `profileRequestEnvelopeLen()`) rather than written as `26` because a stale
/// literal fails in the dangerous direction — a too-narrow peek silently stops
/// suppressing requests and a notification reads `lakko/profile-request/v1`.
/// The peek decides nothing; `decodeProfileRequest` is the verdict.
public func nseLooksLikeProfileRequest(_ bytes: Data, envelopeLen: Int) -> Bool {
    bytes.count == envelopeLen && bytes.first == profileRequestEnvelopeFirstByte
}

/// Decode a lowercase-hex string into bytes. `nil` on any malformed input
/// (odd length, non-hex char). Kept local so the pure core stays FFI-free /
/// deps-file-free (the test target compiles only this file).
private func hexToData(_ hex: String) -> Data? {
    guard hex.count % 2 == 0 else { return nil }
    var out = Data(capacity: hex.count / 2)
    var index = hex.startIndex
    while index < hex.endIndex {
        let next = hex.index(index, offsetBy: 2)
        guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
        out.append(byte)
        index = next
    }
    return out
}

/// ADR-0047 — the notification body for a media message whose kind the
/// already-decoded envelope names. An unknown kind keeps the generic
/// attachment line, exactly as the timeline renders it as a file chip.
func nseMediaLabel(kind: String?) -> String? {
    switch kind {
    case "voice": return nseWord("chat.media.voice_message", english: "Voice message")
    case "video_note": return nseWord("chat.media.video_message", english: "Video message")
    case "gif": return "GIF"
    default: return nil
    }
}

/// Truncate a preview body to a sane notification length.
private func truncate(_ text: String) -> String {
    if text.count <= previewMaxChars { return text }
    return String(text.prefix(previewMaxChars - 1)) + "…"
}

/// `true` when every char of `s` is a lowercase hex digit.
private func isLowerHex(_ s: String) -> Bool {
    for c in s.unicodeScalars {
        let isDigit = c >= "0" && c <= "9"
        let isLower = c >= "a" && c <= "f"
        if !isDigit && !isLower { return false }
    }
    return true
}
