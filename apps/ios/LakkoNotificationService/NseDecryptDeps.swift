// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCoreExtension

/// Production wiring for the iOS silent-push NSE background decrypt
/// (ADR-0041 Phase 3) — the iOS counterpart of Android's
/// `PushBackgroundDecrypt.kt` and the web `sw.js` `buildDecryptDeps`.
///
/// Everything here touches the shared on-device stores (Keychain / App Group
/// container) or the SLIM `LakkoCoreExtension` UniFFI framework (`Identity` /
/// `Group` / `parseBodyEnvelope`) — the decrypt-only surface the NSE links
/// INSTEAD of the full 333 MB `LakkoCore`, to stay under its ~30 MB RAM
/// budget. So it is NOT unit-tested; the pure, FFI-free decrypt core in
/// `NsePushDecrypt.swift` carries the tested logic and this file supplies the
/// real ``NsePushDecryptDeps``.
///
/// ## READ-ONLY THROWAWAY — the load-bearing invariant
///
/// The ``NsePushDecryptDeps`` this file builds has NO write path, and
/// ``NseRealThrowawayGroup`` frees the throwaway handles WITHOUT ever calling
/// `snapshotStorage()` / `IdentitySnapshotStore.putSnapshot` — the slim FFI
/// does not even EXPOSE a persist method (see `crates/lakko-ffi-extension`),
/// so there is no write path to reach. Persisting the advanced ratchet would
/// desync the foreground app, which restores from the SAME last-persisted
/// snapshot and does the real durable processing from its OWN cursor (the
/// G-34 nonce-reuse-on-crash class). There is intentionally no persist call
/// anywhere in this file.

/// Builds the real, read-only ``NsePushDecryptDeps`` from the shared stores +
/// the slim FFI. `boundAccount` reads the account binding the app wrote
/// (``PushAccountBindingStore``) and is the only source of the account a wake
/// is decrypted as; `lookupRoute` checks that account's route row holds the
/// wake's subscription id (the NSE has no live `ChatRepository`);
/// `openThrowawayGroup` reconstructs a FRESH throwaway
/// `Identity` + `Group` — never the app's handles (the NSE is a separate
/// process anyway); `dsFetch` is a slim `URLSession` GET (``NseDsFetch``, no
/// anti-censorship transport pulled in); `startCursor` reads the per-group
/// cursor from ``MessageLogStore`` (its FFI-clean half).
struct NseRealDecryptDeps: NsePushDecryptDeps {

    private let knownGroups = KnownGroupsStore()
    private let snapshotStore = IdentitySnapshotStore()
    private let messageLog = MessageLogStore()
    private let pushRoutes = PushRoutingStore()
    private let dsBaseURL: String
    private let session: URLSession

    init(dsBaseURL: String = NseConfig.dsBaseURL, session: URLSession = .shared) {
        self.dsBaseURL = dsBaseURL
        self.session = session
    }

    /// ADR-0042 (amended 2026-09-30, with ADR-0078) — the account the app
    /// bound push to, when the wake carries a well-formed `s`. The rule is
    /// ``PushAccountBinding/boundAccount(wakeSubscriptionIdHex:binding:)``,
    /// shared with the app's VoIP handler; this only reads the App Group row
    /// the app writes. READ-ONLY.
    func boundAccount(wakeSubscriptionIdHex: String?) -> String? {
        PushAccountBinding.boundAccount(
            wakeSubscriptionIdHex: wakeSubscriptionIdHex,
            binding: PushAccountBindingStore.live.current()
        )
    }

    /// ADR-0042 Sprint 4 — the App-Group reverse-index read that turns the
    /// wake's opaque `push_gid` hex back into the real `group_id`, for the
    /// bound account only. Reuses ``PushRoutingStore`` (compiled into the NSE
    /// target as a sibling source, like the other shared stores): it is
    /// FFI-clean — Foundation + CryptoKit only, no `LakkoCore` — and
    /// `lookupRoute` is a plain shared-suite `UserDefaults` read (the
    /// Keychain-touching secret paths exist on the store but are never called
    /// from the NSE), so the one key-shape definition is shared with the app
    /// instead of duplicated here. READ-ONLY.
    func lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?) -> String? {
        pushRoutes.lookupRoute(
            accountIdB64: accountIdB64, pushGidHex: pushGidHex,
            subscriptionIdHex: subscriptionIdHex)
    }

    /// ADR-0073 R.8 — a key an accepted reset retired, from the App Group
    /// mirror the APP writes (``ContinuityMirror``). READ-ONLY.
    func isSupersededSender(accountIdB64: String, senderKeyB64: String?) -> Bool {
        ContinuityMirror.isSuperseded(senderKeyB64, in: ContinuityMirror.read(accountIdB64: accountIdB64))
    }

    /// ADR-0073 R.7 case 1 — the rule the app's own receive paths run
    /// (``ResendMarker/heldResendPredicate(batch:port:)``), over what this
    /// process may READ: the message log the app wrote (the stored rows of
    /// the marker's group), the lineage mirror (which conversation a group
    /// belongs to) and the continuity mirror (segment order, this account's
    /// own key). Nothing is written, and no key is put through its proven
    /// aliases here (that map is the app's): a re-send whose sender rotated
    /// since simply does not match, and its notification is shown.
    func holdsResendOriginal(
        accountIdB64: String, receivedGroupIdHex: String,
        origin: NseResendOrigin, senderKeyB64: String?
    ) -> Bool {
        let mirror = ContinuityMirror.read(accountIdB64: accountIdB64)
        let received = ResendMarker.Received(
            resendOrigin: ResendMarker.Origin(groupIdB64: origin.groupIdB64, messageId: origin.messageId),
            senderKeyB64: senderKeyB64)
        let log = messageLog
        let held = ResendMarker.heldResendPredicate(
            batch: [received],
            port: ResendMarker.ReceivePort(
                receivedGroupIdHex: receivedGroupIdHex.lowercased(),
                conversationOf: {
                    ConversationLineageMirror.conversationHex(accountIdB64: accountIdB64, groupIdHex: $0)
                },
                segmentIndexOf: { mirror.segmentIndexOfGroupHex[$0.lowercased()] ?? 0 },
                ownPersonKeyB64: mirror.ownPersonKeyB64,
                canonical: { $0 },
                loadLog: {
                    guard let payload = log.getLog(accountIdB64: accountIdB64) else { return nil }
                    return ResendMarker.HeldLog(rows: { hex in
                        payload.groups[hex]?.messages.filter { $0.messageId > 0 }.map {
                            ResendMarker.HeldRow(
                                messageId: $0.messageId,
                                direction: $0.direction == "outgoing"
                                    ? .outgoing : ($0.direction == "incoming" ? .incoming : .system),
                                authorKeyB64: $0.authorKeyB64)
                        }
                    })
                }))
        return held(received)
    }

    /// The DS message ids of the group the app's message log holds, sent or
    /// received; what tells this device's own message coming back from one
    /// it cannot read. READ-ONLY: the log the app wrote (C-59).
    func heldMessageIds(accountIdB64: String, groupIdHex: String) -> Set<Int64> {
        guard let messages = messageLog.getLog(accountIdB64: accountIdB64)?
            .groups[groupIdHex.lowercased()]?.messages else { return [] }
        return Set(messages.lazy.map(\.messageId).filter { $0 > 0 })
    }

    /// This account's own person key, from the App Group mirror the APP
    /// writes (``ContinuityMirror``). READ-ONLY.
    func ownPersonKeyB64(accountIdB64: String) -> String? {
        ContinuityMirror.read(accountIdB64: accountIdB64).ownPersonKeyB64
    }

    func loadIdentitySeed(accountIdB64: String) -> Data? {
        // ADR-0050 — the app's MLS snapshot is taken under this device's
        // DEVICE identity (the conversation leaf), so that seed rebuilds the
        // identity that restores it. The account seed is the fallback for an
        // app that could not install a device identity and ran its chats on the
        // account identity. The slim FFI has no bound constructor; the binding
        // only shapes credentials this read-only process never mints.
        // A genuine Keychain fault is swallowed to `nil` — the NSE degrades
        // to the generic banner, never crashes.
        if let device = (try? DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64)) ?? nil {
            return device
        }
        return (try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64)) ?? nil
    }

    func loadSnapshot(accountIdB64: String) -> Data? {
        snapshotStore.getSnapshot(accountIdB64: accountIdB64)
    }

    func openThrowawayGroup(
        seed: Data,
        snapshot: Data,
        groupIdBytes: Data
    ) throws -> NseThrowawayGroup? {
        // A FRESH throwaway Identity — never the app's live handle (the NSE
        // is a separate process, so there is none anyway). Restoring lays
        // down provider storage in THIS temp Identity only.
        let identity = try Identity.fromSeed(seed: seed)
        try identity.restoreStorage(snapshot: snapshot)
        // `nil` → not a member of this group (or the group is not in the
        // restored provider) — the core discards + shows the generic banner.
        guard let group = try loadGroup(identity: identity, groupId: groupIdBytes) else {
            return nil
        }
        return NseRealThrowawayGroup(identity: identity, group: group)
    }

    func dsFetch(
        groupIdBytes: Data,
        afterId: Int64,
        group: NseThrowawayGroup
    ) async throws -> [NseFetchedRow] {
        try await NseDsFetch.fetchGroup(
            baseURL: dsBaseURL,
            session: session,
            groupId: groupIdBytes,
            after: afterId,
            proof: { path, query in
                group.dsProofHeader(method: "GET", path: path, query: query, body: Data())
            }
        )
    }

    func startCursor(accountIdB64: String, groupHex: String) -> Int64? {
        messageLog.getLog(accountIdB64: accountIdB64)?.groups[groupHex]?.cursor
    }

    /// ADR-0041 Finding-1 — read the armed flag the app mirrors into the
    /// shared App Group suite. Read-only: the NSE never writes it. When
    /// armed, ``decryptPushPreview`` short-circuits before any DS fetch, so
    /// the NSE makes no direct connection. Absent / an unprovisioned suite →
    /// `false` (the common disarmed case keeps the rich preview; a truly
    /// unprovisioned App Group makes the decrypt fail closed on the missing
    /// shared stores anyway).
    func isAntiCensorshipArmed() -> Bool {
        SharedStorage.isAntiCensorshipArmed()
    }

    /// `docs/ux/calls.md` section 2.1 — the conversation's name for a ringing
    /// lock screen, from the App Group mirror the APP writes
    /// (``CallPeerNameMirror``). READ-ONLY; this process resolves no names and
    /// writes nothing (C-59). Absent → the alert is unnamed.
    func callerName(accountIdB64: String, groupIdHex: String) -> String? {
        if let name = CallPeerNameMirror.name(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            return name
        }
        // ADR-0073 R.5 — a group that continues a conversation is titled as
        // the conversation when the app has not mirrored its own title yet.
        let conversation = conversationHex(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard conversation != groupIdHex else { return nil }
        return CallPeerNameMirror.name(accountIdB64: accountIdB64, groupIdHex: conversation)
    }

    /// `docs/ux/calls.md` section 2.1 — the App Group muted-conversation
    /// mirror the app writes. READ-ONLY, like every other method here: this
    /// process holds no thread flags and writes none (C-59). Absent → not
    /// muted → the call rings, which is the only direction a missing mirror
    /// may fail in.
    func isMuted(accountIdB64: String, groupIdHex: String) -> Bool {
        ConversationMuteMirror.isMuted(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// ADR-0073 R.5 — the conversation a group belongs to, from the App Group
    /// lineage mirror the app writes. READ-ONLY (C-59). Absent → the group
    /// itself.
    func conversationHex(accountIdB64: String, groupIdHex: String) -> String {
        ConversationLineageMirror.conversationHex(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// Decode a lowercase-hex string into bytes. `nil` on any malformed input
    /// (odd length, non-hex char).
    static func hexToData(_ hex: String) -> Data? {
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

    /// The reactor's name, from the App Group mirror the app writes
    /// (``PersonNameMirror``). READ-ONLY (C-59).
    func personName(accountIdB64: String, keyB64: String) -> String? {
        let name = PersonNameMirror.name(accountIdB64: accountIdB64, keyB64: keyB64)
        nseTrace("mirror lookup key=\(keyB64) \(name != nil ? "hit" : "miss") \(PersonNameMirror.debugShape(accountIdB64: accountIdB64))")
        return name
    }

    /// The Settings > Notifications > Reactions switch (App Group mirror).
    func reactionNotificationsOn() -> Bool {
        ReactionNotificationMirror.isOn
    }
}

/// A THROWAWAY MLS group backed by a temporary `Identity` + `Group` restored
/// from the last-persisted snapshot.
///
/// ## READ-ONLY THROWAWAY — load-bearing
///
/// ``process(_:)`` runs the audited `processIncomingVerifyingRoles` receive
/// path (advancing THIS temp provider's ratchet in memory) but NEVER persists
/// it, and ``close()`` drops the handles WITHOUT any `snapshotStorage()` /
/// `putSnapshot` call — the slim FFI exposes no such method. The Rust handles
/// are freed by UniFFI's Swift `deinit` when the references are released, so
/// the background decrypt leaves zero durable MLS state behind.
private final class NseRealThrowawayGroup: NseThrowawayGroup {

    private let identity: Identity
    private let group: Group

    init(identity: Identity, group: Group) {
        self.identity = identity
        self.group = group
    }

    var isGroupConversation: Bool { ((try? group.groupKind()) ?? nil) == .group }

    /// ADR-0053 point 9 — the throwaway group's access signer at its restored
    /// epoch. An exporter read over the throwaway provider, never persisted.
    func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? {
        guard let signer = try? group.dsAccessSigner(identity: identity) else { return nil }
        return signer.sign(method: method, path: path, query: query, body: body)
    }

    func process(_ payload: Data) throws -> NseProcessedRow? {
        // ADR-0022 — the same role-verifying receive path the live poll loop
        // uses. A bad frame / wrong epoch / own echo / role-refused commit
        // throws, and the throw reaches the caller's loop, which drops the
        // row (fail-closed, invariant #4) and tells this device's own message
        // apart from one it cannot read.
        // ADR-0092 section 3.6 — an anonymous poll ballot is a NON-MLS row on
        // the group journal: never fed to MLS processing (it would throw and
        // read as an unreadable message), never a notification.
        if isPollBallotRow(row: payload) { return nil }
        let processed = try group.processIncomingVerifyingRoles(identity: identity, bytes: payload)
        // Only Application messages carry a user-visible preview. A Commit /
        // Proposal / ExternalJoinProposal advances state (in the THROWAWAY
        // provider only) but has no body to show.
        guard case let .application(data, _, senderAccount) = processed else { return nil }
        // ADR-0073 R.8 — the PERSON the row came from: the account key the
        // core resolved the sender leaf to, at receipt (C-68). What the core
        // asks the superseded-key mirror about, for every kind of row.
        let senderKeyB64 = senderAccount?.base64EncodedString()
        // CALL SIGNALING — asked FIRST, because this is the one kind of body
        // that must produce something on the lock screen rather than be
        // suppressed (`docs/ux/calls.md` section 2.1: a closed app has to be
        // able to ring). A row this cannot read falls through to the
        // service-envelope filter below, which drops it exactly as before.
        if let call = nseClassifyCallSignaling(data, parser: Self.callParser) {
            return call.attributed(senderKeyB64: senderKeyB64)
        }
        // ADR-0048 — a profile envelope names its sender for the rest of this
        // batch (applied in memory by the caller's loop, never persisted,
        // C-59). Asked before the service-envelope filter, which drops it.
        if Self.decodesAsProfile(data) {
            return NseProcessedRow(
                kind: .profile, text: "", attachmentCount: 0, senderKeyB64: senderKeyB64,
                profileName: Self.profileDisplayName(data))
        }
        // SERVICE-ENVELOPE FILTER — the background half of the gate the
        // foreground `ChatClient.toEvent` has always run, in the same order.
        // Call-signaling proto bytes, ADR-0042/0044 push-routing-secret
        // envelopes, ADR-0045 committer-liveness beacons, ADR-0048 profile
        // envelopes and profile requests, ADR-0051 group names and ADR-0052
        // shared pins all ride the SAME
        // group journal as chat; without this, `parseBodyEnvelope`'s deliberate
        // legacy UTF-8 fallback degrades them to a `.chat` body and the NSE
        // renders a notification full of a domain string plus raw binary.
        // Returning nil drops the row, so the caller's loop falls back to the
        // newest REAL message (or the generic banner).
        if nseIsServiceEnvelope(
            data,
            decodesAsPushRoutingSecret: Self.decodesAsPushRoutingSecret,
            decodesAsCommitterBeacon: Self.decodesAsCommitterBeacon,
            committerBeaconEnvelopeLen: Self.committerBeaconEnvelopeLength,
            decodesAsProfile: Self.decodesAsProfile,
            profileEnvelopeLens: Self.profileEnvelopeLengths,
            decodesAsGroupName: Self.decodesAsGroupName,
            groupNameEnvelopeLens: Self.groupNameEnvelopeLengths,
            decodesAsProfileRequest: Self.decodesAsProfileRequest,
            profileRequestEnvelopeLen: Self.profileRequestEnvelopeLength,
            decodesAsPin: Self.decodesAsPin,
            pinEnvelopeLen: Self.pinEnvelopeLength,
            decodesAsRegistrarSeed: Self.decodesAsRegistrarSeed,
            registrarSeedEnvelopeLen: Self.registrarSeedEnvelopeLength,
            decodesAsScopedDiscovery: Self.decodesAsScopedDiscovery,
            scopedDiscoveryEnvelopeLens: Self.scopedDiscoveryEnvelopeLengths,
            decodesAsSharedChannels: Self.decodesAsSharedChannels,
            sharedChannelsEnvelopeLens: Self.sharedChannelsEnvelopeLengths,
            decodesAsGroupClosed: Self.decodesAsGroupClosed,
            groupClosedEnvelopeLens: Self.groupClosedEnvelopeLengths,
            // ADR-0074 section 3.3 — the key-rotation envelope: a yes / no
            // from the slim FFI, never a decode (it carries the account's
            // next seed and this process has no use for it).
            isKeyRotation: { isKeyRotationEnvelope(plaintext: $0) },
            keyRotationEnvelopeLen: Int(keyRotationEnvelopeLen()),
            // ADR-0080 — a device card: a yes / no from the slim FFI.
            isDeviceCard: { isDeviceCardEnvelope(plaintext: $0) },
            deviceCardEnvelopeLens: Int(deviceCardEnvelopeMinLen())...Int(deviceCardEnvelopeMaxLen()),
            // ADR-0079 (reworked 2026-10-17) — a link-request claim: a yes /
            // no from the slim FFI.
            isLinkClaim: { isLinkClaimEnvelope(plaintext: $0) },
            linkClaimEnvelopeLen: Int(linkClaimEnvelopeLen()),
            // ADR-0079 (order of 2026-10-17) — a link-founded note: a yes /
            // no from the slim FFI.
            isLinkFounded: { isLinkFoundedEnvelope(plaintext: $0) },
            linkFoundedEnvelopeLens: Int(linkFoundedEnvelopeMinLen())...Int(linkFoundedEnvelopeMaxLen()),
            isGroupSetting: { bytes in
                isGroupPhotoEnvelope(plaintext: bytes) || isGroupReactionsEnvelope(plaintext: bytes)
                    || isConversationHandleEnvelope(plaintext: bytes) || isKnockSeedEnvelope(plaintext: bytes)
                    || isAdmissionSeedEnvelope(plaintext: bytes)
                    // ADR-0087 phase B — a channel read key and a publisher's
                    // step-down ride PublisherGroup journals; never shown.
                    || isChannelReadKeyEnvelope(plaintext: bytes)
                    || isPublisherStepDownEnvelope(plaintext: bytes)
                    // A new publisher's earlier seeds and a co-publisher's
                    // rotate request ride PublisherGroup journals too.
                    || isChannelEpochSeedsEnvelope(plaintext: bytes)
                    || isChannelRotateRequestEnvelope(plaintext: bytes)
                    // ADR-0090 — a co-publisher's device request and the
                    // admission mark ride PublisherGroup journals too.
                    || isPublisherDeviceRequestEnvelope(plaintext: bytes)
                    || isChannelAdmissionMarkEnvelope(plaintext: bytes)
                    // ADR-0092 sections 3.6 and 3.9 — the open-items
                    // hand-over, the old-ballot rows and the own poll-choice
                    // register: never shown.
                    || isOpenItemsEnvelope(plaintext: bytes)
                    || isPollBallotsEnvelope(plaintext: bytes)
                    || isPollChoiceEnvelope(plaintext: bytes)
            },
            // `docs/ux/who-can-find-me.md` 6.4 — a contact request inside the
            // group: a yes / no from the slim FFI, never opened here.
            isGroupContactRequest: { isGroupContactRequestEnvelope(plaintext: $0) },
            groupContactRequestEnvelopeLens:
                Int(groupContactRequestEnvelopeMinLen())...Int(groupContactRequestEnvelopeMaxLen())
        ) {
            return nil
        }
        let body = parseBodyEnvelope(bytes: data)
        let kind: NseProcessedKind
        switch body.kind {
        case .chat: kind = .chat
        case .edit: kind = .edit
        case .delete: kind = .delete
        case .reaction: kind = .reaction
        case .typing: kind = .typing
        case .readReceipt: kind = .readReceipt
        // A service envelope the filter above did not recognise (a newer
        // version from a client ahead of this one): the pure core never
        // previews it.
        case .serviceEnvelope: kind = .serviceEnvelope
        // ADR-0061 section 6 — a newer chat wire version: the body text is
        // empty by contract; the preview is the extension's own sentence.
        case .unsupported: kind = .unsupported
        // ADR-0092 — content messages preview by one line; ops never notify.
        case .poll, .checklist, .location, .contactCard, .article: kind = .content
        case .pollVote, .pollClose, .checklistTick, .checklistAdd, .editChecklist, .editArticle:
            kind = .contentOp
        }
        let contentKind: NseContentKind?
        switch body.kind {
        case .poll: contentKind = .poll
        case .checklist: contentKind = .checklist
        case .location: contentKind = .location
        case .contactCard: contentKind = .contactCard
        case .article: contentKind = .article
        default: contentKind = nil
        }
        // Reactions-and-emoji spec section 12 — what a reaction row says, for
        // the "reacted to your message" notification.
        var reaction: NseReaction?
        if kind == .reaction, let target = body.target, let emoji = body.emoji, !emoji.isEmpty {
            reaction = NseReaction(
                targetMessageId: target, emoji: emoji, add: body.reactionAction != .remove,
                targetAuthorB64: body.reactionTargetAuthor?.base64EncodedString())
        }
        // ADR-0087 section 14 — a channel invitation card reads like the
        // card, never as its text (the `/g/` link with its secret).
        // A bare `/g/` link with no card: the link without its secret.
        let text = body.channelInvite.map {
            nseChannelInviteLine(opened: $0.opened, channelName: $0.channelName, publisher: $0.publisher)
        } ?? nseWithoutGroupLinkSecret(body.text)
        return NseProcessedRow(
            kind: kind,
            text: contentKind.map { nseContentPreviewLine(kind: $0, text: body.text) } ?? text,
            attachmentCount: body.attachments.count,
            singleAttachmentKind: body.attachments.count == 1 ? body.attachments[0].kind : nil,
            senderKeyB64: senderKeyB64,
            resendOrigin: Self.resendOrigin(of: body),
            reaction: reaction,
            attachmentCounts: nseAttachmentCounts(body.attachments.map { (kind: $0.kind, mime: $0.mime) }),
            // ADR-0047 section 6 — the first photo or video's micro
            // thumbnail, as the core read it from the envelope.
            inlineThumb: body.inlineThumb
        )
    }

    /// ADR-0073 R.7 case 1 — the re-send marker of a chat body.
    ///
    /// The marker is two fields of the chat envelope, read by the ONE parser
    /// of that wire format, in Rust; the slim extension surface carries the
    /// pair on `ParsedBody`, whole or not at all.
    private static func resendOrigin(of body: ParsedBody) -> NseResendOrigin? {
        guard let group = body.resendOriginGroupB64, let message = body.resendOriginMessageId else {
            return nil
        }
        return NseResendOrigin(groupIdB64: group, messageId: message)
    }

    /// The call-signaling parse: the slim FFI's `parseCallSignal`
    /// (`crates/lakko-ffi-extension/src/call_signaling.rs`), which is the ONE
    /// decoder of that wire format, in Rust, shared with the app.
    ///
    /// Deliberately not a hand-rolled protobuf reader in Swift: this is the one
    /// envelope whose misreading makes a phone ring for a call that is already
    /// over, and a second decoder is the drift class ADR-0044 exists to
    /// document. It is reached through the ``NseCallSignalingParser`` protocol
    /// for the same reason the sibling codecs are injected — the decrypt core
    /// compiles into the FFI-free unit-test target, where a fake drives it.
    ///
    /// `nil` would mean "this build cannot read call signaling", and the whole
    /// call path above then suppresses call rows exactly as the extension did
    /// before calls existed. That fail-safe stays reachable on purpose.
    private static var callParser: NseCallSignalingParser? { NseSlimFfiCallParser() }

    /// The AUTHORITATIVE half of the push-routing-secret filter: the slim FFI's
    /// `decodePushRoutingSecret` (ADR-0042 v2, per ADR-0044), whose domain +
    /// version + exact-length gate is the ONE definition of that wire format.
    /// A throw means "not this envelope" — the common case for every chat
    /// message — so the caller falls through to the normal chat parse.
    ///
    /// Deliberately routed through the Rust codec rather than a Swift-side
    /// domain-prefix match: a hardcoded prefix silently drifts on the next
    /// envelope version bump.
    ///
    /// READ-ONLY: a pure decode. It decodes the secret bytes but NOTHING here
    /// persists, derives a `push_gid`, or subscribes — that is the foreground
    /// app's job on its own durable path. The NSE only needs the yes/no.
    private static func decodesAsPushRoutingSecret(_ bytes: Data) -> Bool {
        (try? decodePushRoutingSecret(plaintext: bytes)) != nil
    }

    /// The AUTHORITATIVE — and here COMPLETE — half of the committer-beacon
    /// filter: the slim FFI's `decodeCommitterBeacon` (ADR-0045). It returns
    /// nothing on success, because the beacon is FIELD-LESS: a clean return IS
    /// the whole verdict, "this is a service envelope, render nothing".
    ///
    /// Unlike the routing case, the Swift-side peek in front of this is only a
    /// hot-path guard and never a partial verdict — every beacon on the wire is
    /// the same ``committerBeaconEnvelopeLength`` bytes, so this one decode is a
    /// total test of that value.
    ///
    /// READ-ONLY: a pure decode. The NSE never EMITS a beacon and never folds a
    /// committer verdict — a beacon means "able to sweep", and this path
    /// structurally cannot commit (C-59), so a beacon from it would be a lie
    /// that costs the group a takeover. Only the yes/no is used.
    private static func decodesAsCommitterBeacon(_ bytes: Data) -> Bool {
        (try? decodeCommitterBeacon(plaintext: bytes)) != nil
    }

    /// The exact composed length of an ADR-0045 beacon, read from the slim FFI's
    /// `committerBeaconEnvelopeLen()` — which derives it from `lakko_contacts`'
    /// own domain + payload constants — so the NSE's cheap pre-filter carries no
    /// hand-copied wire number. This is the FFI-side half of the injection
    /// ``nseLooksLikeCommitterBeacon(_:envelopeLen:)`` describes; a Rust golden
    /// pins the exported value against the composed wire, so a revision fails
    /// Rust CI instead of silently narrowing this filter.
    ///
    /// READ-ONLY: a pure constant. It opens no write path and cannot weaken the
    /// read-only-throwaway discipline (C-59).
    private static var committerBeaconEnvelopeLength: Int {
        Int(committerBeaconEnvelopeLen())
    }

    /// The AUTHORITATIVE half of the ADR-0048 profile filter: the slim FFI's
    /// `decodeProfileEnvelope`, which returns nothing on success — the slim
    /// surface deliberately does not hand the profile's fields to this process.
    ///
    /// READ-ONLY: a pure decode. The NSE never stores a profile (C-59) and never
    /// verifies a username; it only needs the yes/no that keeps a profile out
    /// of a notification body.
    private static func decodesAsProfile(_ bytes: Data) -> Bool {
        (try? decodeProfileEnvelope(plaintext: bytes)) != nil
    }

    /// ADR-0048 — the display name a profile envelope carries, for the
    /// batch-local name map, from the slim FFI's
    /// `profileEnvelopeDisplayName` (`crates/lakko-ffi-extension/src/profile.rs`).
    /// Used in memory for the rest of this wake only; nothing is stored (C-59).
    private static func profileDisplayName(_ bytes: Data) -> String? {
        guard let name = (try? profileEnvelopeDisplayName(plaintext: bytes)) ?? nil else { return nil }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    /// The well-formed profile envelope length range, read from the slim FFI's
    /// `profileEnvelopeMinLen()` / `profileEnvelopeMaxLen()` so no wire number
    /// is hand-copied into the NSE.
    private static var profileEnvelopeLengths: ClosedRange<Int> {
        let minLen = Int(profileEnvelopeMinLen())
        let maxLen = Int(profileEnvelopeMaxLen())
        return minLen...max(minLen, maxLen)
    }

    /// The AUTHORITATIVE half of the ADR-0051 group-name filter: the slim
    /// FFI's `decodeGroupNameEnvelope`. READ-ONLY: a pure decode. The NSE never
    /// stores a group name (C-59) and holds no role table to gate it with; it
    /// only needs the yes/no that keeps a rename out of a notification body.
    private static func decodesAsGroupName(_ bytes: Data) -> Bool {
        (try? decodeGroupNameEnvelope(plaintext: bytes)) != nil
    }

    /// The well-formed group-name envelope length range, read from the slim
    /// FFI's `groupNameEnvelopeMinLen()` / `groupNameEnvelopeMaxLen()` so no
    /// wire number is hand-copied into the NSE.
    private static var groupNameEnvelopeLengths: ClosedRange<Int> {
        let minLen = Int(groupNameEnvelopeMinLen())
        let maxLen = Int(groupNameEnvelopeMaxLen())
        return minLen...max(minLen, maxLen)
    }

    /// The AUTHORITATIVE half of the ADR-0073 group-closed filter (UX 16.5):
    /// the slim FFI's `decodeGroupClosedEnvelope`. READ-ONLY: a pure decode.
    /// The NSE never closes a group (C-59) and holds no role table to gate it
    /// with; it only needs the yes/no that keeps a closed notice out of a
    /// notification body.
    private static func decodesAsGroupClosed(_ bytes: Data) -> Bool {
        (try? decodeGroupClosedEnvelope(plaintext: bytes)) != nil
    }

    /// The two well-formed group-closed envelope lengths, as a range, read
    /// from the slim FFI's `groupClosedEnvelopeMinLen()` /
    /// `groupClosedEnvelopeMaxLen()`.
    private static var groupClosedEnvelopeLengths: ClosedRange<Int> {
        let minLen = Int(groupClosedEnvelopeMinLen())
        let maxLen = Int(groupClosedEnvelopeMaxLen())
        return minLen...max(minLen, maxLen)
    }

    /// The AUTHORITATIVE — and, as for the beacon, COMPLETE — half of the
    /// ADR-0048 profile-request filter: the slim FFI's `decodeProfileRequest`,
    /// which returns nothing on success because the request is field-less.
    ///
    /// READ-ONLY: a pure decode. The NSE never ANSWERS a request — the answer
    /// is an MLS application message, a ratchet advance this process may not
    /// make (C-59); the slim FFI does not even export `composeProfileRequest`.
    /// The foreground app answers on its own receive path.
    private static func decodesAsProfileRequest(_ bytes: Data) -> Bool {
        (try? decodeProfileRequest(plaintext: bytes)) != nil
    }

    /// The exact composed length of an ADR-0048 profile request, read from the
    /// slim FFI's `profileRequestEnvelopeLen()` so the cheap pre-filter carries
    /// no hand-copied wire number; a Rust golden pins the exported value
    /// against the composed wire, so a revision fails Rust CI rather than
    /// silently narrowing this filter.
    private static var profileRequestEnvelopeLength: Int {
        Int(profileRequestEnvelopeLen())
    }

    /// The AUTHORITATIVE half of the ADR-0052 shared-pin filter: the slim
    /// FFI's `decodePinEnvelope`. READ-ONLY: a pure decode. The NSE never
    /// stores a pin (C-59) and holds no role table or people count to gate it
    /// with; it only needs the yes/no that keeps a pin out of a notification
    /// body. The foreground app applies it on its own receive path.
    private static func decodesAsPin(_ bytes: Data) -> Bool {
        (try? decodePinEnvelope(plaintext: bytes)) != nil
    }

    /// The AUTHORITATIVE half of the ADR-0060 filter: the slim FFI's
    /// `decodeScopedDiscoveryEnvelope`, which answers "one of the five 'who can
    /// find me' envelopes" and returns nothing else. READ-ONLY: a pure decode;
    /// the NSE never keeps a grant, a setting or a delegation (C-59). The
    /// foreground app acts on them on its own receive path.
    private static func decodesAsScopedDiscovery(_ bytes: Data) -> Bool {
        (try? decodeScopedDiscoveryEnvelope(plaintext: bytes)) != nil
    }

    /// The length range of the five ADR-0060 envelopes, from the slim FFI.
    private static var scopedDiscoveryEnvelopeLengths: ClosedRange<Int> {
        Int(scopedDiscoveryEnvelopeMinLen())...Int(scopedDiscoveryEnvelopeMaxLen())
    }

    /// The exact composed length of an ADR-0052 pin envelope, read from the
    /// slim FFI's `pinEnvelopeLen()` so the cheap pre-filter carries no
    /// hand-copied wire number; a Rust golden pins the exported value against
    /// the composed wire, so a revision fails Rust CI rather than silently
    /// narrowing this filter.
    private static var pinEnvelopeLength: Int {
        Int(pinEnvelopeLen())
    }

    /// The AUTHORITATIVE half of the ADR-0057 registrar-seed filter: the slim
    /// FFI's `decodeRegistrarSeedEnvelope` (a pure decode; the seed stays
    /// sealed). The NSE never opens or stores it (C-59); the foreground app
    /// gates and keeps it on its own receive path.
    private static func decodesAsRegistrarSeed(_ bytes: Data) -> Bool {
        (try? decodeRegistrarSeedEnvelope(plaintext: bytes)) != nil
    }

    /// The exact length of an ADR-0057 registrar-seed envelope, from the slim
    /// FFI's `registrarSeedEnvelopeLen()` (205).
    private static var registrarSeedEnvelopeLength: Int {
        Int(registrarSeedEnvelopeLen())
    }

    /// The ADR-0059 shared-channels filter: the slim FFI's
    /// `decodeSharedChannelsEnvelope` (which returns no fields), plus the
    /// domain prefix itself. A list that fails its bounds is still a list
    /// the sender meant for the group, never a chat message, so a domain match
    /// is suppressed as well, the web service worker's rule. READ-ONLY: the
    /// NSE never stores a received list (C-59).
    private static func decodesAsSharedChannels(_ bytes: Data) -> Bool {
        if (try? decodeSharedChannelsEnvelope(plaintext: bytes)) != nil { return true }
        return bytes.starts(with: sharedChannelsDomain)
    }

    /// `lakko/shared-channels/v1` plus its trailing NUL, built without a raw
    /// NUL in this source.
    private static let sharedChannelsDomain: Data = {
        var d = Data("lakko/shared-channels/v1".utf8)
        d.append(0x00)
        return d
    }()

    /// The well-formed length range of a shared-channels list, from the slim
    /// FFI so no wire number is hand-copied here.
    private static var sharedChannelsEnvelopeLengths: ClosedRange<Int> {
        Int(sharedChannelsEnvelopeMinLen())...Int(sharedChannelsEnvelopeMaxLen())
    }

    /// LOAD-BEARING: this does NOT persist — no `snapshotStorage()` /
    /// `putSnapshot`. Dropping the references lets UniFFI's `deinit` free the
    /// Rust handles; nothing durable is written.
    func close() {
        // Intentionally empty — ARC + UniFFI `deinit` reclaim the handles.
        // The method exists for the ``NseThrowawayGroup`` contract + to make
        // the "discarded, never persisted" step explicit at the call site.
    }
}

/// The slim FFI's call-signaling classifier, behind the
/// ``NseCallSignalingParser`` seam the decrypt core is written against.
///
/// Maps the three Rust arms onto the three the extension acts on. The one
/// mapping worth stating: a **group-call announce is an offer**, deliberately,
/// because folding it into "some other call frame" would mean a group call
/// never rings — the Rust side pins that choice with a test of its own. The
/// alert wording is the same either way, and it is true either way: someone is
/// calling, with or without video.
///
/// A malformed payload, or one whose call id is not the fixed 16 bytes, is
/// refused rather than guessed at: `nil` makes the caller fall through to the
/// filter that has always dropped these rows, so an unreadable frame is quiet
/// rather than a phone ringing for something nobody could read.
///
/// READ-ONLY: a pure decode. This process never answers, declines or ends a
/// call — all three are MLS application messages, which advance the sender
/// ratchet, the write C-59 forbids here. The app answers; the extension rings.
private struct NseSlimFfiCallParser: NseCallSignalingParser {

    func parse(_ bytes: Data) -> NseCallSignal? {
        guard let signal = try? parseCallSignal(plaintext: bytes) else { return nil }
        switch signal {
        case .offer(let callId, let video, _):
            guard let hex = Self.callIdHex(callId) else { return nil }
            return .offer(callIdHex: hex, video: video)
        case .end(let callId):
            guard let hex = Self.callIdHex(callId) else { return nil }
            return .end(callIdHex: hex)
        case .other:
            return .other
        }
    }

    /// Lowercase hex of a 16-byte call id — the same spelling the app stamps on
    /// and reads back from a notification
    /// (``LakkoCallNotification/callIdUserInfoKey``), so the two processes name
    /// one call identically. A wrong-width id is refused, not padded.
    private static func callIdHex(_ callId: Data) -> String? {
        guard callId.count == 16 else { return nil }
        let alphabet = Array("0123456789abcdef")
        var out = String()
        out.reserveCapacity(32)
        for byte in callId {
            out.append(alphabet[Int(byte >> 4)])
            out.append(alphabet[Int(byte & 0x0F)])
        }
        return out
    }
}

/// Slim, self-contained DS group-journal fetch for the NSE.
///
/// A plain `URLSession` GET of `GET /ds/group/{group_hex}/messages?after=&wait=0`
/// — DELIBERATELY NOT the app's ``DsClient``, which routes through
/// `AntiCensorshipTransport` and would drag the transport / anti-censorship
/// stack (tokio, REALITY, MASQUE, …) into the memory-starved NSE process. The
/// NSE only needs a read-only catch-up fetch; this is the whole surface.
enum NseDsFetch {

    /// Fetch DS journal rows strictly after `after` for `groupId`. `wait=0` —
    /// a catch-up fetch, never a long-poll (the NSE budget is short).
    static func fetchGroup(
        baseURL: String,
        session: URLSession,
        groupId: Data,
        after: Int64,
        proof: (_ path: String, _ query: String) -> String?
    ) async throws -> [NseFetchedRow] {
        let hex = hexString(groupId)
        // ADR-0053 — the proof covers the SERVICE path (never the base URL's
        // nginx prefix) and the query exactly as `components.url` sends it.
        let path = "/ds/group/\(hex)/messages"
        guard var components = URLComponents(string: baseURL + path) else {
            throw NseDsFetchError.badURL
        }
        components.queryItems = [
            URLQueryItem(name: "after", value: String(after)),
            URLQueryItem(name: "wait", value: "0"),
        ]
        guard let url = components.url else { throw NseDsFetchError.badURL }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        if let header = proof(path, components.percentEncodedQuery ?? "") {
            request.setValue(header, forHTTPHeaderField: dsProofHeaderName())
        }
        // Keep well inside the NSE wall-clock budget (the handler also runs a
        // shorter overall deadline).
        request.timeoutInterval = 15

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            throw NseDsFetchError.http
        }
        let decoded = try JSONDecoder().decode(FetchWireResponse.self, from: data)
        return decoded.messages.compactMap { message in
            guard let payload = Data(base64Encoded: message.payloadB64) else { return nil }
            // `created_at` is carried because a call's age is measured against
            // the SERVER's stamp and nothing else — this is a catch-up fetch
            // from the app's cursor, so the newest call row can easily be an
            // offer from a call that is long over. A response without the
            // field yields an empty string, which reads as an unestablishable
            // age and so as past the ring window.
            return NseFetchedRow(
                id: message.id,
                payload: payload,
                createdAt: message.createdAt ?? "")
        }
    }

    /// Lowercase-hex encode a byte buffer (matches ``DsClient/pseudonymHex(_:)``).
    private static func hexString(_ bytes: Data) -> String {
        let alphabet = Array("0123456789abcdef")
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for b in bytes {
            out.append(alphabet[Int(b >> 4)])
            out.append(alphabet[Int(b & 0x0F)])
        }
        return out
    }

    private struct FetchWireResponse: Decodable {
        let messages: [FetchWireMessage]

        enum CodingKeys: String, CodingKey {
            case messages
        }
    }

    private struct FetchWireMessage: Decodable {
        let id: Int64
        let payloadB64: String
        /// Optional so a response that omits it decodes rather than throwing
        /// away the whole batch; an absent stamp becomes an unreadable age,
        /// which the ring rule treats as past the window.
        let createdAt: String?

        enum CodingKeys: String, CodingKey {
            case id
            case payloadB64 = "payload_b64"
            case createdAt = "created_at"
        }
    }

    enum NseDsFetchError: Error {
        case badURL
        case http
    }
}

/// NSE-side configuration. The DS base URL follows the app's ``DsClient``
/// default: the NSE bundle carries the same `LakkoApiBaseUrl` Info.plist key
/// (filled from the same `LAKKO_API_BASE_URL` build setting), so it resolves
/// to `<base>/ds`, or `http://localhost:8082` with no base URL. The NSE
/// bundle's `LakkoDsUrl` key, when non-empty, overrides both.
enum NseConfig {
    static var dsBaseURL: String {
        if let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoDsUrl") as? String {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { return trimmed }
        }
        return ApiEndpoints.baseURL(for: .ds)
    }
}

#if DEBUG
/// DEBUG builds only: the extension's trace file, one line per step of a
/// wake, appended to `Library/Caches/nse-trace.log` in the App Group
/// container so a device run can be read back with `devicectl`. Capped at
/// 256 KiB (the older half is dropped). Carries no message text.
enum NseTraceFile {
    private static let queue = DispatchQueue(label: "me.lakko.nse-trace")

    /// Route ``nseTrace(_:)`` here. Called once from the extension's entry.
    static func install() {
        guard nseTraceSink == nil else { return }
        nseTraceSink = { line in append(line) }
    }

    static func append(_ line: String) {
        guard let dir = SharedStorage.containerURL()?
            .appendingPathComponent("Library/Caches", isDirectory: true) else { return }
        queue.sync {
            let url = dir.appendingPathComponent("nse-trace.log")
            let stamp = ISO8601DateFormatter().string(from: Date())
            guard let data = "\(stamp) \(line)\n".data(using: .utf8) else { return }
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            if let existing = try? Data(contentsOf: url), existing.count + data.count > 256 * 1024 {
                try? existing.suffix(128 * 1024).write(to: url)
            }
            if let handle = try? FileHandle(forWritingTo: url) {
                defer { try? handle.close() }
                _ = try? handle.seekToEnd()
                try? handle.write(contentsOf: data)
            } else {
                try? data.write(to: url)
            }
        }
    }
}
#endif

