// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import CryptoKit
import Foundation
import LakkoCore

/// In-memory + on-disk state holder for the user's MLS chats.
///
/// Mirror of:
///
///   * `apps/web/src/chat/ChatContext.tsx` (1.17f4a)
///   * `apps/android/.../chat/ChatRepository.kt` (1.15f4a)
///
/// **Owns the canonical ``Identity``** for the current session —
/// built once via ``bootstrap(secrets:)`` from the OPAQUE export
/// key, kept alive until ``clear()``. Every
/// chat operation threads this same handle through ``ChatClient``
/// so MLS state mutations accumulate on a single OpenMLS provider.
/// Throwaway `Identity.fromSeed` per op (the simpler shape that
/// ``ContactsRepository`` uses for its stateless sign / decrypt
/// surface) would lose every ratchet advance.
///
/// **Holds a per-group `Group` handle map** keyed by hex group id.
/// The Group instances carry MLS ratchet state; dropping them
/// mid-session would force a re-bootstrap from the snapshot. The
/// map itself is `@MainActor`-isolated, so no two callers ever
/// mutate it at literally the same instant. That is NOT the same as
/// serialization: `@MainActor` isolation is released at every
/// `await`, so two `async` callers interleave freely at their
/// suspension points. The four group-membership create-commit paths,
/// and the APPLY half of the poll (G-38), therefore run inside an
/// explicit per-group serializing chain — see
/// ``withCommitLock(groupIdHex:_:)``.
///
/// **Snapshot persistence** (1.14f5) runs after every
/// state-mutating call via ``IdentitySnapshotStore``. Failures
/// are logged-and-swallowed — the in-memory state is still
/// correct; the next mutation will retry the persist naturally.
///
/// ## Lifecycle
///
///   * ``bootstrap(secrets:)`` is called by
///     ``AuthRepository/signup(username:password:)`` /
///     `login` / `recover` once the session's ``IdentitySecrets``
///     are known. It builds the canonical Identity from the export
///     key and restores the on-disk snapshot if one exists.
///     Idempotent for the same account id; replaces the Identity
///     if called for a different account.
///   * ``clear()`` is called by ``AuthRepository/logout()`` —
///     drops the in-memory state but keeps the on-disk snapshot
///     envelope intact, so the next login on this device picks up
///     where we left off.
///   * Forget device and Delete account call ``clear()`` too, from
///     ``AppServices/wipeAccountLocalData(accountIdB64:)``; the on-disk
///     stores are then removed by ``AccountLocalDataWipe``, which holds the
///     one list of them.
@MainActor
public final class ChatRepository: ObservableObject {

    // MARK: - Published state

    @Published public internal(set) var state: ChatRepositoryState = ChatRepositoryState()

    // MARK: - Wiring

    let chatClient: ChatClient
    let dsClient: DsClient
    let discovery: DiscoveryClient
    private let snapshotStore: IdentitySnapshotStore
    /// ADR-0050 — which leaf identity the on-disk snapshot belongs to.
    private let snapshotLeafKeyStore: SnapshotLeafKeyStore
    let knownGroupsStore: KnownGroupsStore
    /// Where ``RestoredKnownGroupsStore`` lives: the conversations a restored
    /// backup named that this device has not joined. Settable so a test can
    /// point it at a transient suite.
    var restoredGroupsDefaults: UserDefaults = .standard
    /// Phase 2a3f1 — persistent plaintext-message log.
    /// Survives app launches; mutated on every append /
    /// poll / send.
    let messageLogStore: MessageLogStore
    /// ADR-0040 (G-35) — durable pending-commit outbox. The four
    /// group-membership commit paths write a `PendingCommitEntry`
    /// (commit + Welcome / rendezvous bytes) here BEFORE they
    /// publish, and the first `pollGroup` of a session sweeps it to
    /// re-publish any commit that crashed mid-window. See
    /// ``persistCommitEntryThenSnapshotOrThrow(_:)`` +
    /// ``sweepPendingCommits(record:)``.
    let commitOutbox: CommitOutboxStore

    /// ADR-0042 — the opaque per-group push routing orchestrator. Owns the
    /// mint (owner-only) / receive / derive / subscribe-under-`push_gid`
    /// logic + the device-key-encrypted secret store + the App Group
    /// plaintext `push_gid → (account, group_id)` reverse index.
    let pushRouting: PushGroupRouting

    /// ADR-0045 S4b — per-`(account, group)` record of which candidates have
    /// beaconed and when, keyed on the sender's 32-byte signature KEY (never a
    /// leaf) and stamped with the DS row's server-assigned time (never a local
    /// clock). Read at verdict time by ``actingCommitterNow(record:ownRank:)``.
    private let beaconStore: BeaconObservationStore

    /// ADR-0045 S4a — when each pending membership request was first observed,
    /// so the UX quiet window can mean the same thing on every device.
    private let pendingRequestStore: PendingRequestStore
    /// ADR-0045 S7 residual 1 — the `Not applied` rows. Deliberately NOT part of
    /// ``pendingRequestStore``: that store is pruned against the core's live
    /// pending set on every fold, and these rows must OUTLIVE the sweep that
    /// empties it — a row is created at the exact moment its observation is
    /// pruned.
    private let unappliedRequestStore: UnappliedRequestStore

    /// ADR-0045 UX §6.1.9 — the owner this device last observed, per group, so
    /// an ownership CHANGE can be told apart from a FIRST observation. Nothing
    /// here is load-bearing: losing it costs one notice, not a role.
    private let lastObservedOwnerStore: LastObservedOwnerStore

    /// ADR-0045 UX §6.2.3 — when this device published a self-targeted leave
    /// request, per group. Deliberately NOT part of ``pendingRequestStore``:
    /// that store is pruned against the core's live pending set on every fold,
    /// and the sweep that removes us is exactly what empties that set, so the
    /// poll batch carrying the eviction would destroy the only evidence that we
    /// asked for it. Losing it costs the NARROWING, never the state — the
    /// fallback string is true either way.
    private let leaveRequestStore: LeaveRequestStore

    /// ADR-0087 section 9 — the owed ends, the notices not yet closed and the
    /// held Welcomes (``ChatRepository+MembershipEnd``).
    let membershipEndStore: MembershipEndStore
    /// When this device created or joined each group (``GroupArrival``).
    let groupArrivalStore: GroupArrivalStore
    /// The owed ends of this account, the store's record in memory: consulted
    /// on every poll and for every row.
    var owedEnds: [String: OwedMembershipEnd] = [:]
    /// UX 9.1 — what the banner at the top of Chats shows.
    public let membershipEndNotices = MembershipEndNoticeCenter()
    /// The account's synced state. Wired by the composition root; `nil` in a
    /// test that does not exercise it, and then step 2 waits.
    weak var conversationEndSync: ConversationEndSync?
    /// The subscription that feeds ``applyHeardEnds()``.
    var conversationEndsWatch: AnyCancellable?
    /// Whether the end may run now. A VoIP wake only records it: the wipe
    /// waits for the app to be in front. Tests inject their own.
    var membershipEndMayRun: @MainActor () -> Bool = { CommitterEnvelopeForeground.isOnScreen() }
    /// An end was recorded for a group. The composition root points it at the
    /// group call host: a call in that group ends now. `left`: the person
    /// asked for it.
    var onMembershipEndRecorded: (@MainActor (_ groupIdHex: String, _ left: Bool) -> Void)?
    /// A channel ended on another device of this account. Wired by the
    /// signed-in shell, which owns the channel list.
    var onChannelEndedElsewhere: (@MainActor (_ channelIdHex: String) -> Void)?
    /// A held Welcome landed. Wired by ``ContactsRepository``, for what it
    /// does after any Welcome.
    var onHeldWelcomeJoined: (@MainActor (_ groupIdHex: String) -> Void)?

    /// `docs/ux/who-can-find-me.md` 6.4 — a contact request one member sent
    /// inside a group (the envelope as it came, and the poster's account key
    /// resolved at receipt, base64). Set by the contacts repository, which
    /// opens it or drops it.
    var onGroupContactRequest: (@MainActor (_ groupIdHex: String, _ envelope: Data, _ senderKeyB64: String) -> Void)?
    /// Groups whose end is being carried out now.
    var endsRunning: Set<String> = []
    /// One pass over the held Welcomes at a time.
    var heldWelcomesRunning = false
    /// A pass was asked for while one ran.
    var heldWelcomesRerun = false

    /// ADR-0045 S4b — when this device last emitted its own liveness beacon, per
    /// group. This device's OWN local clock, which is legitimate here and
    /// nowhere else in the beacon design: it governs only this device's send
    /// cadence and feeds no judgement about anybody else.
    private var lastBeaconAtMs: [String: UInt64] = [:]

    /// The canonical ``Identity`` for the current session. `nil`
    /// until ``bootstrap(secrets:)`` populates it.
    ///
    /// ADR-0050 — this is the DEVICE identity: a per-install random seed bound
    /// to the account key by a signed binding carried in its leaf credential.
    /// Every conversation group and every KeyPackage uses it, so a person's
    /// devices are distinct MLS leaves that all resolve to one account.
    var canonicalIdentity: Identity?

    /// ADR-0050 — the phrase-seeded ACCOUNT identity, kept beside the device
    /// identity for what is about the PERSON rather than a leaf: group invite
    /// capabilities (issuer key) and their revocation.
    var accountIdentity: Identity?

    /// ADR-0074 section 3.1 (Stage A) — the account identity this device's
    /// conversation leaves are BOUND to when that is not the current one: the
    /// account rotated its keys and the leaves keep their bindings under the
    /// old key until Stage B. `nil` when the binding names the current key
    /// (an account that never rotated here).
    private var predecessorAccountIdentity: Identity?

    /// The account identity the CONVERSATION layer knows this person by: the
    /// key the device binding names, hence the key the role tables, the
    /// registrar-seed and root-handover envelopes and the invite issuers
    /// carry. During Stage A that is the old key, everywhere; what derives
    /// from the seed OUTSIDE MLS (journal roots, the mailbox, the reach keys,
    /// channel owner keys) uses ``accountIdentity``.
    var conversationAccountIdentity: Identity? {
        predecessorAccountIdentity ?? accountIdentity
    }

    /// Account id (base64) of the session that bootstrapped this
    /// repository.
    var currentAccountIdB64: String? {
        // ADR-0082 section 5 — a delete for everyone removes the message's
        // translations and transcripts; the fold that sees it does not know
        // the account, so the bound one is kept where the purge reads it.
        didSet { MessageTranslationPurge.boundAccountIdB64 = currentAccountIdB64 }
    }

    /// Raw account id + bearer token of the live session, for the
    /// KeyPackage upload. Refreshed on EVERY ``bootstrap(secrets:)`` call,
    /// including the same-account early return, because a re-login in the
    /// same process mints a new token without changing the account.
    var currentAccountId: Data?
    private var currentSessionToken: String?

    /// Advanced whenever the account this repository serves leaves or is
    /// replaced (``wipeInMemoryState()``: ``clear()`` and an account switch
    /// in ``bootstrap(secrets:)``). An operation that suspends captures it
    /// first and checks it before it writes: the identity and the group it
    /// read before the `await` belong to the binding that started it, and
    /// ``groups`` may by then be another account's. Every region queued on
    /// the per-group commit chain is checked in one place
    /// (``withCommitLock(groupIdHex:_:)``).
    let binding = BindingEpoch()

    /// The binding a send was sealed under, for the ratchet persist that
    /// runs between the seal and the publish (``ChatClient/ratchetPersister``).
    @TaskLocal private static var sendBinding: BindingEpoch.Captured?

    /// Publishes this device's KeyPackage from ``canonicalIdentity`` —
    /// persist-before-upload, single-flight. See ``republishKeyPackage()``.
    private lazy var keyPackageRepublisher = KeyPackageRepublisher(
        ports: KeyPackageRepublisher.Ports(
            mint: { [weak self] in
                guard let self,
                      let identity = self.canonicalIdentity,
                      let accountId = self.currentAccountId,
                      let token = self.currentSessionToken,
                      !token.isEmpty else {
                    return nil
                }
                return KeyPackageUpload(
                    accountId: accountId,
                    sessionToken: token,
                    keyPackage: try identity.createKeyPackage(),
                    deviceId: identity.signaturePublicKey()
                )
            },
            persist: { [weak self] in
                guard let self else {
                    throw ChatError.argument(
                        "ChatRepository deallocated — KeyPackage upload aborted"
                    )
                }
                try self.persistSnapshotDurableOrThrow()
            },
            upload: { upload in
                do {
                    try await DiscoveryClient(sessionToken: upload.sessionToken)
                        .uploadKeyPackage(
                            accountId: upload.accountId,
                            deviceId: upload.deviceId,
                            keyPackageBytes: upload.keyPackage
                        )
                } catch {
                    // ADR-0074 section 2.2 — discovery's `403 device-revoked`
                    // is NOT proof of removal: auth is asked (a refresh), and
                    // its answer is what the session layer acts on. A
                    // `device-mismatch` is a session that does not name this
                    // device yet; the bind re-requests the publish.
                    if KeyPackagePublishRefusal.of(error) == .deviceRevoked {
                        _ = await SessionTokenProvider.shared.refresh()
                    }
                    throw error
                }
            }
        )
    )

    /// Map of hex group id → live ``Group`` handle + per-thread
    /// timeline / cursor. Read + mutated only on `@MainActor`, and every
    /// UniFFI `Group` call is synchronous, so an individual access is never
    /// torn. Membership commits additionally serialize on
    /// ``withCommitLock(groupIdHex:_:)`` because their hazard spans an
    /// `await`, which `@MainActor` does not cover.
    ///
    /// A record can be REPLACED (not mutated) or evicted by another task while
    /// a caller waits, so any code that waits on an `await` must re-read this
    /// map afterwards rather than hold the `GroupRecord` it captured before the
    /// wait.
    var groups: [String: GroupRecord] = [:]

    /// ADR-0073 R.5 — the conversation lineage: which group continues which
    /// conversation. A projection of the DeviceGroup synced state's lineage
    /// registers, handed in by the composition root
    /// (``applyLineage(_:)``); empty until then, so every group is its own
    /// conversation.
    var conversationLineage = ConversationLineage.none
    /// ADR-0073 R.5 — writes the register `successor -> root conversation`
    /// (write-once) and sends it to the account's other devices. The
    /// composition root points it at
    /// ``DeviceGroupSyncRepository/setLineage(successorGroupId:rootConversationId:)``.
    /// ADR-0073 R.6 step 1 — successors this device created and has not yet
    /// made held conversations, by successor group id (standard base64). In
    /// memory; after a restart the group is loaded from the MLS snapshot the
    /// creation persisted.
    var preparedSuccessors: [String: Group] = [:]
    /// ADR-0079 — one-to-ones founded for a won invite-link request and not
    /// yet held, by group id (standard base64). In memory; after a restart
    /// the group is loaded from the MLS snapshot the founding persisted.
    var preparedLinkGroups: [String: Group] = [:]
    /// Old groups with a continuity pass in flight.
    var continuityInFlight: Set<String> = []
    /// When every held group was last offered a continuity pass, UNIX ms.
    var lastContinuitySweepAtMs: Int64 = 0
    /// One flush of the held messages at a time.
    var heldFlushRunning = false
    /// ADR-0073 R.7 case 1 — where the pending row of each held message is
    /// drawn: queue entry id -> (group, the row's local id).
    var heldRows: [String: (groupIdHex: String, localId: Int64)] = [:]
    /// Pairing state bundle v3 — the display-only copies of what another
    /// device of this account wrote to a pending contact, by key, as last
    /// read from ``PendingDisplayCopies``; `nil` until read (and after every
    /// change). See `ChatRepository+PendingInvited.swift`.
    var displayCopyCache: [String: [HeldSendQueue.Entry]]?
    /// When the roster identities of a group were last re-checked because a
    /// claim was kept pending, UNIX ms.
    var claimIdentityCheckedAtMs: [String: Int64] = [:]
    /// ADR-0073 R.7 case 1 — how the held-message queue is sealed at rest:
    /// under the Keychain wrap key the message log uses (a held message is
    /// message plaintext). Injectable for a test host with no Keychain.
    var heldSendSealer = HeldSendQueue.Sealer(
        seal: { try MessageLogStore.sealUnderWrapKey($0) },
        open: { try MessageLogStore.openUnderWrapKey($0) })
    /// Removes the delivered notifications the extension marked quiet
    /// (``QuietNotifications``). Injectable for tests.
    var removeQuietNotifications: () -> Void = { QuietNotifications.removeDelivered() }
    /// Removes the ``ResetEvents`` subscription.
    var resetEventsUnsubscribe: (() -> Void)?
    /// The subscription that feeds ``applyLineage(_:)``; owned here so the
    /// composition root needs no cancellable bag of its own.
    var lineageWatch: AnyCancellable?
    var lineageWriter: @MainActor (_ successorGroupId: Data, _ rootConversationId: Data) async -> Bool = {
        _, _ in false
    }

    /// ADR-0040 (G-35) — per-group chain tail: the **serializing** lock the
    /// four group-membership create-commit paths run inside, joined by the
    /// poll's apply half in G-38 and then by EVERY application send —
    /// ``sendMessage(groupIdHex:plaintext:attachments:)``,
    /// ``publishApplicationMessage(groupIdHex:plaintextBytes:)``,
    /// ``broadcastPushRoutingControl(accountIdB64:groupIdHex:bytes:)`` and the three
    /// ADR-0023 message ops via ``publishMessageOpLocked(groupIdHex:envelopeBytes:applyLocally:)``
    /// — because a send straddling an epoch change is lost permanently, not
    /// merely reordered (see ``sealAndPublishLocked``). Swift counterpart of
    /// the Android `ChatRepository` per-group mutex — the same lock its
    /// `runSplitPoll` takes — keyed per group so two different conversations
    /// never block each other.
    ///
    /// The value is the tail `Task` the next caller chains behind, type-erased
    /// to `Void` so one queue can hold sections returning different things.
    /// Same construction as ``PushGroupRouting`` `routingChain` (ADR-0044
    /// C-62), for the same reason: a read-decide-write that spans an `await`
    /// needs a lock, not actor isolation.
    ///
    /// **Serializing, not coalescing.** A successor WAITS and then runs in
    /// full against freshly-read state; it is never folded into the section
    /// already running. Folding was ADR-0043 finding F1 on the push path, and
    /// here it would be worse than useless — two membership operations answer
    /// two DIFFERENT intents (add Alice, remove Bob), so a fold would silently
    /// drop one.
    ///
    /// The chain lives in ``GroupMlsLock`` (same construction, moved so the
    /// app-level call host can observe its drains — see
    /// ``observeGroupMlsIdle(_:)``).
    private let commitLock = GroupMlsLock()

    /// ADR-0040 (G-35) — hex group ids already swept this session, so the
    /// pending-commit outbox sweep runs at most once per group per session (on
    /// the first ``pollGroup(groupIdHex:waitSecs:)`` for that group). Reset by
    /// ``wipeInMemoryState()`` on a logout / account switch.
    private var sweptGroupsThisSession: Set<String> = []

    /// ADR-0050 2.5a — the epoch each group's recorded intents were last
    /// evaluated at, so a re-request is attempted only after an OBSERVED
    /// merged commit, never on a poll cadence. Reset per session: the first
    /// poll of a session evaluates, because a commit may have landed while the
    /// app was not running.
    private var supersededTrigger = SupersededTrigger()
    /// ADR-0050 2.5a — groups whose superseded evaluation is running, so two
    /// triggers never re-request the same intent twice.
    private var supersededRunning: Set<String> = []

    /// ADR-0060 section 6 — `group | person` of the committer envelopes on
    /// their way out, so a poll's re-send check never doubles a send in flight.
    private var committerEnvelopesInFlight: Set<String> = []
    /// The reach state committer envelopes are composed from. Tests inject
    /// their own.
    var committerEnvelopeReach: @MainActor () -> ReachCenter = { .shared }
    /// What a committer envelope's send time is read from. Tests inject their
    /// own.
    var committerEnvelopeClock: @MainActor () -> UInt64 = { ReachCenter.nowMs() }
    /// Whether a poll's tail may re-send. A VoIP wake polls in the background,
    /// where no re-send runs. Tests inject their own.
    var committerEnvelopeForeground: @MainActor () -> Bool = { CommitterEnvelopeForeground.isActive() }

    /// ADR-0053 — per group journal, the newest access signer this device
    /// derived and the one before it (contract points 2 and 5).
    let dsSigners = DsSignerMemory()
    /// ADR-0053 point 2 — groups already claimed this session (claim-on-open
    /// runs on the first poll of each group per session).
    var claimedGroupsThisSession: Set<String> = []
    /// ADR-0057, live walk 2026-09-29 — the owner's claim of each group
    /// journal, once per session, sent without waiting for a `401` (an
    /// unclaimed journal never answers one). Awaited before any epoch
    /// registration of the group.
    private let claimOnOpen = JournalClaimOnOpen()
    /// Live walk 2026-09-29 — per group, the retry backoff after the DS
    /// refused a sweep's epoch registration (`400` / `401` / `403`).
    private var sweepRegistrationBackoff: [String: SweepRegistrationBackoff] = [:]
    /// ADR-0057 — this device's registrar seeds + the owner's appointment
    /// ledger, per `(account, group)`.
    let registrarSeeds: RegistrarSeedStore
    /// ADR-0057 — member account key -> account X25519 key (from the identity
    /// bundle), for sealing registrar seeds. Per session.
    private var memberX25519Cache: [Data: Data] = [:]
    /// ADR-0057 — account key -> account id this device learned first-hand
    /// (the recipient of a chat it founded), for the identity-bundle fetch.
    var memberAccountIdHints: [Data: Data] = [:]
    /// ADR-0057 — groups whose registrar reconcile is running.
    private var registrarReconcileInflight: Set<String> = []
    /// ADR-0057 — groups whose reconcile was asked for again while one was
    /// running; the running one goes round once more (a root takeover must
    /// not be swallowed by a reconcile that started under the old ledger).
    private var registrarReconcileRerun: Set<String> = []
    /// ADR-0057 S2 finding 1 — groups whose root handover is running now.
    private var rootHandoverInFlight: Set<String> = []
    /// ADR-0057 S2 finding 1 — a schedule that arrived while the handover ran.
    private var rootHandoverRerun: Set<String> = []
    /// ADR-0057 S2 finding 1 — when an unsettled handover may run again.
    private var rootHandoverNotBefore: [String: Date] = [:]
    /// ADR-0057 second pass, finding 4 — when this session last sent an owed
    /// handover envelope per group (re-sent at most every
    /// ``RootHandover/resendInterval`` while the probe answers 204).
    private var rootHandoverLastSentAt: [String: Date] = [:]
    /// ADR-0057 second pass, finding 4 — groups whose takeover (with a kept
    /// old root seed) is running now, and when a failed one may run again.
    private var rootTakeoverInFlight: Set<String> = []
    private var rootTakeoverNotBefore: [String: Date] = [:]
    /// ADR-0057 second pass, finding 4 — when an owner without the "root
    /// held" marker may probe again.
    private var rootProbeNotBefore: [String: Date] = [:]
    /// ADR-0057 second pass, finding 2 — the registrar set this session last
    /// sent per group (its fingerprint): sent once per group per session and
    /// on every roster or role change.
    private var registrarSetsSent: [String: String] = [:]
    /// ADR-0057 second pass, finding 1 — the repair-from-above scheduler for
    /// conversation groups (the only groups with a head poller here).
    private lazy var journalRepair: JournalRepair = JournalRepair(ports: .init(
        authority: { [weak self] groupIdHex in
            guard let self, let record = self.groups[groupIdHex] else { return nil }
            return self.journalAuthority(record: record, groupIdHex: groupIdHex)
        },
        pollToHead: { [weak self] groupIdHex in
            try await self?.pollToHead(groupIdHex: groupIdHex)
        },
        currentEpochKey: { [weak self] groupIdHex in
            guard let self, let record = self.groups[groupIdHex],
                  let signer = self.currentDsSigner(record: record, groupIdHex: groupIdHex) else {
                return nil
            }
            return (epoch: signer.epoch(), key: signer.verifyingKey())
        },
        journalId: { [weak self] groupIdHex in self?.groups[groupIdHex]?.groupId },
        ds: dsClient))
    /// ADR-0053 point 7 — the per-group floor on knock reads.
    private let knockThrottle = KnockPollThrottle()
    /// ADR-0053 — the same floor for the attribution read, which reads the
    /// whole knock queue for the members sheet. A sibling keyed the same way
    /// (by group), so the two reads do not starve each other.
    private let attributionThrottle = KnockPollThrottle()

    /// Take one attribution read for `groupIdHex` if the floor allows it.
    func takeJoinAttributionRead(groupIdHex: String) -> Bool {
        attributionThrottle.take(groupIdHex)
    }

    /// ADR-0048 — where a received in-group profile goes: the shared profile
    /// directory in production. A seam so a test can capture what the receive
    /// path hands over.
    ///
    /// The third argument is the account the batch was received for (this
    /// repository's account when the sink fired): the directory files the
    /// profile only while that account is the active one.
    var receivedProfileSink: @MainActor (ProfileClaim, MemberIdentity?, String?) -> Void = {
        claim, senderKey, accountIdB64 in
        ProfileDirectory.shared.acceptReceived(
            claim, signatureKeyB64: senderKey, accountIdB64: accountIdB64)
    }

    /// ADR-0051 §3 — the per-account, per-group `{ name, updatedAtMs }` store
    /// behind every group title. Public so the list / chat view models read
    /// the title through it (``groupName(groupIdHex:)``).
    public let groupNameStore: GroupNameStore

    /// ADR-0052 decision 3 — the per-account, per-conversation shared pin map
    /// `messageId -> { pinned, updatedAtMs }`, wiped with the conversation.
    public let sharedPinStore: SharedPinStore

    /// ADR-0052 decision 3 — "Pin for me": this device's own bookmarks,
    /// keyed on the conversation and the message. Nothing about them is sent.
    public let personalPinStore: PersonalPinStore

    /// ADR-0024 — the per-account, per-group durable read watermarks
    /// (`senderSignatureKeyB64 -> upTo`). Fed here, at receipt, for EVERY group
    /// whether or not its chat is open, so a receipt applied by the session
    /// poll or while another chat is open is not lost, and the Read tick
    /// survives a relaunch (the DS poll never replays an applied row).
    public let readWatermarkStore: ReadWatermarkStore

    /// The per-account group-call records behind ``groupCalls``, so a relaunched
    /// device still shows the Join banner for a call that is running.
    let groupCallStore: GroupCallStore

    /// ADR-0024 — the in-memory mirror of ``readWatermarkStore`` for the
    /// current account, keyed by group hex. Populated on every advancing
    /// receipt (the rest is read through ``readWatermarks(groupIdHex:)``); the
    /// open chat subscribes to its own group's entry. Cleared with the session.
    @Published public private(set) var readWatermarksByGroup: [String: PresenceOps.ReadWatermarkMap] = [:]

    /// ADR-0051 — bumped on every accepted name (received or set here), so the
    /// chat list and the open chat re-title in place. The title itself is read
    /// from ``groupNameStore`` on demand; this only says "read it again".
    @Published public internal(set) var groupNameRevision: Int = 0

    /// ADR-0087 — the per-account, per-group photo, allowed reactions, @name
    /// and knock seeds (``GroupSettingsStore``).
    public let groupSettingsStore = GroupSettingsStore()

    /// ADR-0087 — bumped on every accepted group photo or reactions setting,
    /// so the list, the open chat and the info screen re-read them.
    /// Reactions-and-emoji spec section 12 — own messages with unseen
    /// reactions from others, per conversation, and its revision.
    public let unseenReactionsStore = UnseenReactionsStore()
    @Published public internal(set) var unseenReactionsRevision: Int = 0

    @Published public internal(set) var groupSettingsRevision: Int = 0 {
        didSet { refreshGroupPhotos() }
    }

    /// ADR-0052 decision 3 — bumped on every accepted pin action (received or
    /// taken here, shared or personal), so the open chat rebuilds its pinned
    /// bar in place. The pins themselves are read from the two stores on
    /// demand; this only says "read them again".
    @Published public private(set) var pinRevision: Int = 0

    /// A pin store changed outside the two ordinary setters (ADR-0073 R.5:
    /// a shared pin of an older segment cleared on this device alone).
    func notePinsChanged() { pinRevision &+= 1 }

    /// ADR-0052 decision 3 — the per-group serializing chain the pin receive
    /// runs under, for the reason ``groupNameLock`` gives (C-62 sub-invariant
    /// 5: the per-message stamp comparison is only meaningful against CURRENT
    /// stored state). Its own chain, never ``commitLock``: the sink is called
    /// from inside the poll's commit-locked apply region.
    private let pinLock = GroupMlsLock()

    /// ADR-0052 decision 3 — the growth re-send: a device that may set shared
    /// pins re-sends the current pinned set (original stamps) when the roster
    /// grows, throttled and jittered like the group name.
    private lazy var pinPublisher = PinPublisher(
        ports: PinPublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelopes: { [weak self] groupIdHex in
                self?.pinEnvelopesForResend(groupIdHex: groupIdHex) ?? []
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// ADR-0059 section 6 — the channels members share inside a group and
    /// this device's own "Share my channels" switches. Receives from both
    /// receive paths, sends through the locked application path, re-sends on
    /// growth. The shareable list is fed by the signed-in shell, which owns the
    /// channel list.
    public private(set) lazy var sharedChannels = SharedChannelsHub(
        ports: SharedChannelsHub.Ports(
            accountIdB64: { [weak self] in self?.currentAccountIdB64 },
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            canSend: { [weak self] groupIdHex in
                guard let self, self.groups[groupIdHex] != nil else { return false }
                return !membershipEnded(self.membershipStateNow(groupIdHex: groupIdHex))
            },
            nowMs: { Self.nowMs() }
        )
    )

    /// This account's person key (base64), read off any live group's roster;
    /// `nil` before a group exists. The shared-channels aggregation skips it.
    public func ownPersonKeyB64() -> String? {
        for record in groups.values {
            if let own = AuthorIdentity.own(group: record.group) { return own }
        }
        return nil
    }

    /// ADR-0051 §2 — the per-group serializing chain the name receive runs
    /// under (C-62 sub-invariant 5: the gate must read CURRENT stored state, and
    /// two envelopes from one poll batch must not both decide against the same
    /// pre-batch snapshot). Its own chain, not ``commitLock``: the receive sink
    /// is called synchronously from inside the poll's commit-locked apply
    /// region, so taking the commit chain from it would nest the lock.
    let groupNameLock = GroupMlsLock()

    /// ADR-0073 R.12 — a one-to-one moved to a successor group. The
    /// composition root points this at the call host, which ends a call live
    /// in the old group: its keys belong to devices that were removed.
    var onOneToOneMoved: ((_ groupIdHex: String) -> Void)?

    /// ADR-0051 §3 — the growth re-send: an owner / admin holding a name
    /// re-sends it (original stamp) when the roster grows, throttled and
    /// jittered like the profile republish.
    lazy var groupNamePublisher = GroupNamePublisher(
        ports: GroupNamePublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelope: { [weak self] groupIdHex in
                self?.groupNameEnvelopeForResend(groupIdHex: groupIdHex)
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// ADR-0087 section 18.1 — the growth re-send of the group photo, the
    /// group-name publisher's throttle and jitter, the stored stamp verbatim.
    lazy var groupPhotoPublisher = GroupNamePublisher(
        ports: GroupNamePublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelope: { [weak self] groupIdHex in
                self?.groupPhotoEnvelopeForResend(groupIdHex: groupIdHex)
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// ADR-0087 section 11 — the growth re-send of the group's `@name`, the
    /// same shape; any member may relay it.
    lazy var groupHandlePublisher = GroupNamePublisher(
        ports: GroupNamePublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelope: { [weak self] groupIdHex in
                self?.groupHandleEnvelopeForResend(groupIdHex: groupIdHex)
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// Reactions-and-emoji spec section 17.1 — the growth re-send of the
    /// group's allowed reactions, the same shape.
    lazy var groupReactionsPublisher = GroupNamePublisher(
        ports: GroupNamePublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelope: { [weak self] groupIdHex in
                self?.groupReactionsEnvelopeForResend(groupIdHex: groupIdHex)
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// ADR-0048 §2 — publishes this device's own profile into its groups on
    /// join, on change and on growth, through the same locked application send
    /// path the other service envelopes take.
    private lazy var profilePublisher = ProfilePublisher(
        ports: ProfilePublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                guard let self else { return }
                try await self.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: bytes)
            },
            envelope: { [weak self] groupIdHex in
                // ADR-0060 section 3 — the grants of THIS conversation.
                guard let self, let record = self.groups[groupIdHex] else {
                    return ProfileDirectory.shared.ownProfileEnvelope()
                }
                return ProfileDirectory.shared.ownProfileEnvelope(
                    groupId: record.groupId,
                    oneToOne: self.conversationKind(groupIdHex: groupIdHex) == .chat)
            },
            canSend: { [weak self] groupIdHex in
                guard let self, self.groups[groupIdHex] != nil else { return false }
                return !membershipEnded(self.membershipStateNow(groupIdHex: groupIdHex))
            },
            nowMs: { Self.nowMs() },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    /// ADR-0048 §2 trigger 4 — the per-account, per-group "last asked" stamp
    /// behind ``profileRequester``. Non-secret, app-private.
    private let profileRequestStore = ProfileRequestStore()

    /// ADR-0048 §2 trigger 4 — asks a group to introduce itself, right after a
    /// join and on opening a chat that still shows a placeholder for someone,
    /// through the same locked application send path the profile itself takes.
    /// The decision is the core's `shouldRequestProfiles`; this only supplies
    /// its inputs (the unnamed count off the live roster + the persisted stamp).
    private lazy var profileRequester = ProfileRequester(
        ports: ProfileRequester.Ports(
            send: { [weak self] groupIdHex in
                guard let self else { return }
                try await self.publishApplicationMessage(
                    groupIdHex: groupIdHex, plaintextBytes: composeProfileRequest())
            },
            unnamedMembers: { [weak self] groupIdHex in
                self?.unnamedMemberCount(groupIdHex: groupIdHex)
            },
            canSend: { [weak self] groupIdHex in
                guard let self, self.groups[groupIdHex] != nil else { return false }
                return !membershipEnded(self.membershipStateNow(groupIdHex: groupIdHex))
            },
            lastRequestedAtMs: { [weak self] groupIdHex in
                guard let self, let accountIdB64 = self.currentAccountIdB64 else { return nil }
                return self.profileRequestStore.lastRequestedAtMs(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            },
            recordRequestedAt: { [weak self] groupIdHex, atMs in
                guard let self, let accountIdB64 = self.currentAccountIdB64 else { return }
                self.profileRequestStore.record(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, atMs: atMs)
            },
            nowMs: { UInt64(max(0, Self.nowMs())) },
            rule: { joinedJustNow, unnamed, last, now in
                shouldRequestProfiles(
                    joinedJustNow: joinedJustNow, unnamedMembers: unnamed,
                    lastRequestedAtMs: last, nowMs: now)
            }
        )
    )

    public init(
        chatClient: ChatClient = ChatClient(),
        dsClient: DsClient = DsClient(),
        discovery: DiscoveryClient = DiscoveryClient(),
        snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore(),
        snapshotLeafKeyStore: SnapshotLeafKeyStore = SnapshotLeafKeyStore(),
        knownGroupsStore: KnownGroupsStore = KnownGroupsStore(),
        messageLogStore: MessageLogStore = MessageLogStore(),
        commitOutbox: CommitOutboxStore = CommitOutboxStore(),
        pushRouting: PushGroupRouting = PushGroupRouting(),
        beaconStore: BeaconObservationStore = BeaconObservationStore(),
        pendingRequestStore: PendingRequestStore = PendingRequestStore(),
        unappliedRequestStore: UnappliedRequestStore = UnappliedRequestStore(),
        lastObservedOwnerStore: LastObservedOwnerStore = LastObservedOwnerStore(),
        leaveRequestStore: LeaveRequestStore = LeaveRequestStore(),
        membershipEndStore: MembershipEndStore = MembershipEndStore(),
        groupNameStore: GroupNameStore = GroupNameStore(),
        sharedPinStore: SharedPinStore = SharedPinStore(),
        personalPinStore: PersonalPinStore = PersonalPinStore(),
        readWatermarkStore: ReadWatermarkStore = ReadWatermarkStore(),
        groupCallStore: GroupCallStore = GroupCallStore(),
        registrarSeeds: RegistrarSeedStore = RegistrarSeedStore(),
        groupArrivalStore: GroupArrivalStore = GroupArrivalStore()
    ) {
        self.groupArrivalStore = groupArrivalStore
        self.groupCallStore = groupCallStore
        self.registrarSeeds = registrarSeeds
        self.groupNameStore = groupNameStore
        self.sharedPinStore = sharedPinStore
        self.personalPinStore = personalPinStore
        self.readWatermarkStore = readWatermarkStore
        self.chatClient = chatClient
        self.dsClient = dsClient
        self.discovery = discovery
        self.snapshotStore = snapshotStore
        self.snapshotLeafKeyStore = snapshotLeafKeyStore
        self.knownGroupsStore = knownGroupsStore
        self.messageLogStore = messageLogStore
        self.commitOutbox = commitOutbox
        self.pushRouting = pushRouting
        self.beaconStore = beaconStore
        self.pendingRequestStore = pendingRequestStore
        self.unappliedRequestStore = unappliedRequestStore
        self.lastObservedOwnerStore = lastObservedOwnerStore
        self.leaveRequestStore = leaveRequestStore
        self.membershipEndStore = membershipEndStore
        // SECURITY (nonce-reuse-on-crash) — wire the fail-closed MLS
        // ratchet persister into the ChatClient so EVERY application send
        // (chat / edit / delete / reaction / typing / read-receipt /
        // call-signaling — they all ratchet via `createApplicationMessage`)
        // durably persists the post-advance (N+1) ratchet BEFORE the DS
        // publish, and aborts the send if it cannot. See
        // ``persistSnapshotDurableOrThrow()``.
        self.chatClient.ratchetPersister = { [weak self] in
            guard let self else {
                throw ChatError.argument(
                    "ChatRepository deallocated — send aborted before publish"
                )
            }
            // A send started by this repository carries the binding it was
            // sealed under. The seal runs off the main actor, so the account
            // can have left before this persist: then the identity read below
            // is nobody's (the persist would do nothing) or the next
            // account's, and the ciphertext must not go out.
            try Self.sendBinding?.require()
            try await self.persistSnapshotDurableOrThrow()
        }
        // ADR-0053 — every publish ChatClient makes to a group journal is
        // signed with this repository's signer, previous-epoch fallback and
        // claim-on-open retry included.
        self.chatClient.dsAuthProvider = { [weak self] groupIdHex in
            self?.dsAuth(groupIdHex: groupIdHex)
        }
        // A routing row is worked on per `(account, group)` and that work
        // outlives a sign-out. A subscription carries this device's tokens
        // sealed for the signed-in account's group (ADR-0078), so the gateway
        // is reached only for the account that is bound here.
        self.pushRouting.accountIsBound = { [weak self] accountIdB64 in
            self?.currentAccountIdB64 == accountIdB64
        }
        // ADR-0078 — every subscription of the signed-in account is sent again
        // through this routing when the device's tokens change.
        PushSubscriber.shared.routing = self.pushRouting
        // ADR-0074 part 3 — a pairing state bundle may bring the account's
        // own predecessors to a session that is already running.
        stateBundleObserver = NotificationCenter.default.addObserver(
            forName: .lakkoStateBundleApplied, object: nil, queue: .main
        ) { [weak self] note in
            guard let accountIdB64 = note.userInfo?[StateBundle.appliedAccountKey] as? String else { return }
            let pendingAdded = note.userInfo?[StateBundle.pendingAddedKey] as? Bool == true
            Task { @MainActor in
                // The own-key resolver reads the predecessor slot.
                ReachCenter.shared.reloadKeyResolver()
                self?.rebindAfterStateBundle(accountIdB64: accountIdB64)
                // Bundle v3 — display copies of a pending contact's messages.
                if pendingAdded, self?.currentAccountIdB64 == accountIdB64 {
                    self?.invalidateDisplayCopies()
                }
            }
        }
        observeStateBundleNames()
        historyBundleObserver = NotificationCenter.default.addObserver(
            forName: .lakkoHistoryBundleFiled, object: nil, queue: .main
        ) { [weak self] note in
            guard let accountIdB64 = note.userInfo?[HistoryBundleCodec.accountUserInfoKey] as? String,
                  let slots = note.userInfo?[HistoryBundleCodec.slotsUserInfoKey] as? [String: PersistedGroup]
            else { return }
            MainActor.assumeIsolated {
                self?.adoptHistoryBundle(accountIdB64: accountIdB64, slots: slots)
            }
        }
    }

    private var historyBundleObserver: NSObjectProtocol?
    private var stateBundleObserver: NSObjectProtocol?
    private var stateBundleNamesObserver: NSObjectProtocol?

    /// Pairing state bundle v3 — group names stored by the bundle re-title
    /// the chat list and the open chat in place (profiles bump the profile
    /// directory on their own); display copies of a pending contact's
    /// messages are read again.
    private func observeStateBundleNames() {
        stateBundleNamesObserver = NotificationCenter.default.addObserver(
            forName: .lakkoStateBundleNamesApplied, object: nil, queue: .main
        ) { [weak self] note in
            guard let accountIdB64 = note.userInfo?[StateBundle.appliedAccountKey] as? String else { return }
            MainActor.assumeIsolated {
                guard let self, self.currentAccountIdB64 == accountIdB64 else { return }
                self.groupNameRevision &+= 1
            }
        }
    }

    // MARK: - Lifecycle

    /// Build the canonical Identity for the session from the
    /// OPAQUE export key + restore any on-disk MLS snapshot.
    ///
    /// Idempotent for the same account id (early-returns).
    /// Switching accounts (different `secrets.accountId`) wipes the
    /// in-memory state first.
    ///
    /// Failures from snapshot restore are logged-and-swallowed:
    /// a corrupted / stale envelope shouldn't block the user from
    /// continuing — they just start with an empty conversation
    /// list on this device, and peers re-bootstrap via fresh
    /// KeyPackages.
    public func bootstrap(secrets: IdentitySecrets) {
        let newAccountIdB64 = base64Encode(secrets.accountId)
        currentAccountId = secrets.accountId
        currentSessionToken = secrets.sessionToken
        if currentAccountIdB64 == newAccountIdB64 { return }
        if currentAccountIdB64 != nil {
            wipeInMemoryState()
        }
        let account: Identity
        do {
            // ADR-0017: the AccountIdentity is seeded from the
            // phrase-derived seed, NOT the OPAQUE export key — so a
            // routine recover / password change preserves the identity.
            account = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        } catch {
            // Identity construction is deterministic + can't fail
            // unless the seed bytes are malformed (UniFFI
            // throws on wrong length). Surface as a no-op: the
            // state stays empty + the next login retries.
            return
        }
        // ADR-0050 — the chats run on the DEVICE identity.
        let device = deviceIdentityInstaller(newAccountIdB64, account)
        let identity = device ?? account
        // ADR-0074 Stage A — a binding kept under the account's own old key.
        let predecessor = Self.boundPredecessor(
            accountIdB64: newAccountIdB64, device: device, account: account)

        let restored: Bool
        do {
            let ownLeafB64 = base64Encode(identity.signaturePublicKey())
            let recorded = snapshotLeafKeyStore.load(accountIdB64: newAccountIdB64)
            // A recorded leaf key that differs is always another identity's
            // snapshot. An ABSENT record is one written before ADR-0050, i.e.
            // under the account identity: foreign to a device identity, and
            // exactly right for the account-identity fallback.
            let foreign = recorded.map { $0 != ownLeafB64 } ?? (device != nil)
            if foreign, snapshotStore.getSnapshot(accountIdB64: newAccountIdB64) != nil {
                // ADR-0050 — a snapshot taken under another leaf identity (every
                // pre-ADR-0050 snapshot was taken under the ACCOUNT identity)
                // must not be restored: `restoreStorage` replaces the whole
                // provider with groups signed by a key this identity does not
                // hold. Drop it (no production data to migrate), so neither this
                // process nor the notification extension restores it again.
                NSLog("ChatRepository: snapshot belongs to another leaf identity; starting empty")
                snapshotStore.deleteSnapshot(accountIdB64: newAccountIdB64)
                knownGroupsStore.clear(accountIdB64: newAccountIdB64)
            }
            snapshotLeafKeyStore.save(accountIdB64: newAccountIdB64, leafKeyB64: ownLeafB64)
            if let blob = snapshotStore.getSnapshot(accountIdB64: newAccountIdB64) {
                try identity.restoreStorage(snapshot: blob)
                restored = true
            } else {
                restored = false
            }
        } catch {
            restored = false
        }

        canonicalIdentity = identity
        accountIdentity = account
        predecessorAccountIdentity = predecessor
        currentAccountIdB64 = newAccountIdB64
        // Ongoing group calls this device observed before the relaunch; records
        // past the max age are dropped on load.
        groupCalls = groupCallStore.load(accountIdB64: newAccountIdB64, nowMs: Self.nowMs())
        // ADR-0087 section 9 — read before anything consults it.
        owedEnds = Dictionary(
            uniqueKeysWithValues: membershipEndStore.owed(accountIdB64: newAccountIdB64)
                .map { ($0.groupIdHex, $0) })

        // Eagerly rebuild `Group` handles for every previously-
        // known group id (1.14f5). `restoreStorage` laid down
        // OpenMLS provider storage; without this loop the in-
        // memory map would still be empty + peers' commits to
        // those groups would miss `pollGroup`'s lookup until the
        // user re-opened each thread.
        if restored {
            for groupIdHex in knownGroupsStore.load(accountIdB64: newAccountIdB64) {
                guard let groupId = Self.hexToBytes(groupIdHex) else { continue }
                do {
                    guard let group = try loadGroup(
                        identity: identity,
                        groupId: groupId
                    ) else {
                        continue
                    }
                    groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
                    // ADR-0042 legacy-sweep — provision opaque push routing
                    // for every restored group. Owner + no secret yet → mint
                    // + persist + distribute the envelope + subscribe; a
                    // member holding a secret → subscribe under `push_gid`;
                    // anyone else → defer (no push under `group_id`).
                    // Idempotent + race-safe (shared in-flight guard with the
                    // chat view's onAppear). Best-effort.
                    ensurePushRouting(groupIdHex: groupIdHex)
                } catch {
                    // best-effort
                    continue
                }
            }
        }

        // The groups this device holds are no longer "conversations a
        // restored backup named that it has yet to join". Covers a restore
        // on a device whose local MLS state already held them.
        RestoredKnownGroupsStore.prune(
            accountIdB64: newAccountIdB64,
            heldGroupIdsHex: Array(groups.keys),
            defaults: restoredGroupsDefaults)

        // Phase 2a3f1 — restore the plaintext message log on
        // top of the rehydrated Group handles. The two stores
        // are independent (MLS state ⊥ message timeline), so a
        // corrupt / missing log doesn't block the rest of the
        // bootstrap.
        if let log = messageLogStore.getLog(accountIdB64: newAccountIdB64) {
            for (groupIdHex, persisted) in log.groups {
                guard let record = groups[groupIdHex] else { continue }
                let messages = persisted.messages.compactMap { $0.toChatMessage() }
                record.seed(
                    messages: messages,
                    cursor: persisted.cursor,
                    lastViewedMessageId: persisted.lastViewedMessageId,
                    feedSeen: Set(persisted.feedSeen ?? []),
                    syncedThroughMessageId: persisted.syncedThroughMessageId)
            }
            // `docs/ux/second-device.md` section 2 — the rest of the log (a
            // history bundle lands before its Welcomes) waits for its join.
            waitingLogSlots.fill(from: log.groups, held: Set(groups.keys))
        }

        state = ChatRepositoryState(
            threads: buildThreadList(),
            bootstrapped: true
        )
        // ADR-0073 — the dead-session publish guard, the reset-accepted
        // trigger, the held messages a restart left on disk, and one
        // continuity pass.
        installContinuity()
        // ADR-0087 section 9 — the ends a restart left owed, and the stored
        // groups this account is no longer in.
        sweepMembershipEnds()
    }

    /// Clear the in-memory state for the current session. Keeps
    /// the on-disk MLS snapshot intact — ``AccountLocalDataWipe`` is
    /// the full-wipe path.
    public func clear() {
        wipeInMemoryState()
        currentAccountIdB64 = nil
        // The bearer token was the closed session's too.
        currentAccountId = nil
        currentSessionToken = nil
        canonicalIdentity = nil
        accountIdentity = nil
        predecessorAccountIdentity = nil
        state = ChatRepositoryState()
    }

    private func wipeInMemoryState() {
        binding.advance()
        accountIdentity = nil
        predecessorAccountIdentity = nil
        clearHeldSends()
        groups.removeAll()
        waitingLogSlots.clear()
        groupCalls.removeAll()
        unreadMessageIds.removeAll()
        canonicalIdentity = nil
        // ADR-0024 — the mirror belongs to the account that loaded it.
        readWatermarksByGroup.removeAll()
        // ADR-0040 (G-35) — a new session must re-sweep the outbox for every
        // group on its first poll.
        sweptGroupsThisSession.removeAll()
        // ADR-0053 — the signers belong to the identity of the closing session.
        dsSigners.reset()
        memberX25519Cache.removeAll()
        memberAccountIdHints.removeAll()
        registrarReconcileInflight.removeAll()
        registrarReconcileRerun.removeAll()
        rootHandoverInFlight.removeAll()
        rootHandoverRerun.removeAll()
        rootHandoverNotBefore.removeAll()
        rootHandoverLastSentAt.removeAll()
        rootTakeoverInFlight.removeAll()
        rootTakeoverNotBefore.removeAll()
        rootProbeNotBefore.removeAll()
        registrarSetsSent.removeAll()
        journalRepair.reset()
        claimedGroupsThisSession.removeAll()
        claimOnOpen.reset()
        sweepRegistrationBackoff.removeAll()
        knockThrottle.reset()
        attributionThrottle.reset()
        supersededTrigger = SupersededTrigger()
        supersededRunning.removeAll()
        committerEnvelopesInFlight.removeAll()
        owedEnds.removeAll()
        endsRunning.removeAll()
        heldWelcomesRunning = false
        heldWelcomesRerun = false
        membershipEndNotices.replace([])
        // ADR-0045 — a new session re-establishes its own beacon cadence; a
        // carried-over stamp would suppress the first beacon of that session,
        // which the core deliberately makes immediate.
        lastBeaconAtMs.removeAll()
        // ADR-0048 — a scheduled growth republish belongs to the session that
        // saw the growth.
        profilePublisher.reset()
        // ADR-0051 — same for a scheduled group-name re-send.
        groupNamePublisher.reset()
        // ADR-0087 — and for the photo and reactions re-sends.
        groupPhotoPublisher.reset()
        groupReactionsPublisher.reset()
        groupHandlePublisher.reset()
        // ADR-0052 — and for a scheduled shared-pin re-send.
        pinPublisher.reset()
        // ADR-0059 — and the shared-channels row + its growth re-sends.
        sharedChannels.reset()
        // The commit chain (``GroupMlsLock``) is deliberately NOT cleared. Dropping a tail does not
        // stop the section behind it — it only makes the next caller start
        // BESIDE that section, which is the race the chain exists to close.
        // Queued sections re-read `canonicalIdentity` / `groups` (now empty)
        // and fail their own guards, and the tail-guarded release in
        // ``withCommitLock(groupIdHex:_:)`` drains the map on its own.
    }

    /// ADR-0050 — how ``bootstrap(secrets:)`` obtains the device identity.
    /// Production is ``installDeviceIdentity(accountIdB64:account:)``; a test
    /// that seeds a snapshot under the account identity sets a closure that
    /// returns `nil`, which keeps the chats on the account identity.
    var deviceIdentityInstaller: @MainActor (_ accountIdB64: String, _ account: Identity) -> Identity? = {
        ChatRepository.installDeviceIdentity(accountIdB64: $0, account: $1)
    }

    /// ADR-0050 — build this device's bound DEVICE identity: the per-install
    /// seed from ``DeviceIdentityStore`` plus the persisted binding, or a fresh
    /// binding minted by the account key when none is stored (or the stored
    /// one no longer verifies, e.g. after a fresh-identity mint). `nil` when
    /// the device seed cannot be read or written, in which case the caller
    /// falls back to the account identity for the chats (the pre-ADR-0050
    /// behaviour).
    static func installDeviceIdentity(accountIdB64: String, account: Identity) -> Identity? {
        installDeviceIdentity(
            accountIdB64: accountIdB64, account: account,
            predecessorSeeds: KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64),
            holdsConversations: !KnownGroupsStore().load(accountIdB64: accountIdB64).isEmpty)
    }

    /// The rule of ``installDeviceIdentity(accountIdB64:account:)`` over the
    /// account's own predecessor seeds (oldest first) and whether this device
    /// is in any conversation.
    ///
    /// ADR-0074 section 3.1 (Stage A) — after a rotation of the account keys
    /// a device does NOT re-bind: its leaves stay under the old key, and a
    /// binding minted under the new key would make this device a different
    /// account to every gate, and misjudge its own standing. A binding under
    /// one of the account's OWN predecessor keys is kept. (A reset keeps no
    /// predecessor, so it still re-mints.)
    ///
    /// ADR-0074 part 3 — a device paired AFTER a rotation gets the
    /// predecessor seeds from the pairing state bundle and mints its binding
    /// under the conversation key (the oldest predecessor), the key the
    /// account's other devices are bound to. A binding minted under the
    /// current key before the bundle arrived is minted again while this
    /// device holds no conversation (``OwnAccountKeys/bindingVerdict``).
    static func installDeviceIdentity(
        accountIdB64: String, account: Identity, predecessorSeeds: [Data], holdsConversations: Bool
    ) -> Identity? {
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            var stored: Identity?
            if let binding = DeviceIdentityStore.getDeviceBinding(accountIdB64: accountIdB64) {
                stored = try? Identity.fromSeedWithBinding(seed: seed, binding: binding)
                if stored == nil { NSLog("ChatRepository: stored device binding rejected; re-minting") }
            }
            let predecessors = predecessorSeeds.compactMap { try? Identity.fromSeed(seed: $0) }
            switch OwnAccountKeys.bindingVerdict(
                boundKey: stored?.accountPublicKey(),
                currentKey: account.signaturePublicKey(),
                predecessorKeys: predecessors.map { $0.signaturePublicKey() },
                holdsConversations: holdsConversations) {
            case .keep:
                if let stored { return stored }
            case .keepUnderCurrentKey:
                NSLog("ADR-0074: the device binding names the current account key and this device holds conversations; kept")
                if let stored { return stored }
            case .remint:
                NSLog("ADR-0074: the device binding is minted again under the conversation key")
            case .mint:
                if stored != nil {
                    NSLog("ChatRepository: stored device binding names another account key; re-minting")
                }
            }
            let conversation = predecessors.first ?? account
            let bound = try Identity.fromSeedBound(
                seed: seed,
                accountSeed: conversation.ed25519Seed(),
                issuedAtUnixSecs: UInt64(max(0, Date().timeIntervalSince1970))
            )
            if let binding = bound.deviceBinding() {
                try DeviceIdentityStore.putDeviceBinding(accountIdB64: accountIdB64, binding: binding)
            }
            return bound
        } catch {
            NSLog("ChatRepository: device identity install failed; chats fall back to the account identity: \(error)")
            return nil
        }
    }

    /// ADR-0074 part 3 — a pairing state bundle just stored the account's
    /// own predecessors on a device whose chat layer is already running (it
    /// signed in with the phrase and paired afterwards). While it holds no
    /// conversation its binding is minted again under the conversation key
    /// and its KeyPackage published again, so no conversation admits it
    /// under the current key. Returns whether the binding changed.
    @discardableResult
    func rebindAfterStateBundle(accountIdB64: String) -> Bool {
        guard currentAccountIdB64 == accountIdB64, let account = accountIdentity,
              let before = canonicalIdentity, groups.isEmpty else { return false }
        guard let device = deviceIdentityInstaller(accountIdB64, account),
              let boundKey = device.accountPublicKey(), boundKey != before.accountPublicKey() else {
            return false
        }
        canonicalIdentity = device
        predecessorAccountIdentity = Self.boundPredecessor(
            accountIdB64: accountIdB64, device: device, account: account)
        republishKeyPackage()
        return true
    }

    /// ADR-0074 Stage A — the account's own predecessor identity the device
    /// binding names, or `nil` when the binding names the current key (or
    /// there is no device identity).
    static func boundPredecessor(accountIdB64: String, device: Identity?, account: Identity) -> Identity? {
        guard let boundKey = device?.accountPublicKey(), boundKey != account.signaturePublicKey() else {
            return nil
        }
        return KeychainOwnPredecessorStore().load(accountIdB64: accountIdB64)
            .compactMap { try? Identity.fromSeed(seed: $0) }
            .first { $0.signaturePublicKey() == boundKey }
    }

    /// ADR-0074 section 3.2 step 6 / section 3.3 — the account's keys were
    /// rotated and this device holds the new ones. Nothing of the
    /// conversations changes: the device identity, its binding and every
    /// group stay. What moves is the ACCOUNT identity used outside MLS (the
    /// journal roots this account holds, its mailbox), and the old identity
    /// becomes the one the conversation layer answers to.
    public func adoptRotatedAccount(secrets: IdentitySecrets, predecessorSeed: Data) {
        guard currentAccountIdB64 == base64Encode(secrets.accountId),
              let rotated = try? Identity.fromSeed(seed: secrets.accountIdentitySeed) else { return }
        currentSessionToken = secrets.sessionToken
        if predecessorAccountIdentity == nil,
           let old = try? Identity.fromSeed(seed: predecessorSeed),
           let boundKey = canonicalIdentity?.accountPublicKey(), boundKey == old.signaturePublicKey() {
            predecessorAccountIdentity = old
        }
        accountIdentity = rotated
        // ADR-0074 section 3.5a rule 3 — from now the profile re-send is
        // owed, durably, on this device (rotating or kept).
        markProfileResendOwed()
        // The new mailbox (its pseudonym follows the new key).
        claimOwnMailboxes()
    }

    /// ADR-0074 section 3.5, "DS" — for every group journal this account
    /// roots (it is the ADR-0022 owner), hand the root from the OLD seed's
    /// root to the new seed's: the old root seed is kept as the pending
    /// takeover seed and the ADR-0057 takeover runs, exactly as after an
    /// ownership handover (the `root` transfer signed by the old root, then
    /// the registrar set replaced and the registrar seeds re-sent). Durable:
    /// a takeover that does not land is retried by the receive loop. Returns
    /// how many journals were scheduled.
    ///
    /// Call after ``adoptRotatedAccount(secrets:predecessorSeed:)``: the new
    /// root is derived from the account identity it installed.
    @discardableResult
    public func transferOwnedJournalRoots(predecessorSeed: Data) throws -> Int {
        guard let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        var oldAccountSeed = try Identity.fromSeed(seed: predecessorSeed).ed25519Seed()
        defer { RootHandover.wipe(&oldAccountSeed) }
        var scheduled = 0
        for (groupIdHex, record) in groups where isJournalOwner(record: record) {
            if registrarSeeds.pendingRootSeed(accountIdB64: accountIdB64, groupIdHex: groupIdHex) == nil {
                let rootSeed = try dsJournalRootSeed(
                    accountIdentitySeed: oldAccountSeed, journalId: record.groupId)
                registrarSeeds.putPendingRootSeed(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, seed: rootSeed)
            }
            scheduleRootTakeover(groupIdHex: groupIdHex)
            scheduled += 1
        }
        // ADR-0087 section 5 rotation rule — the root moved, so every
        // knock seed moved with it: bump each owned group's knock version.
        bumpKnockVersionsAfterRotation(predecessorSeed: predecessorSeed)
        NSLog("ADR-0074: %d owned journal root transfer(s) scheduled", scheduled)
        return scheduled
    }

    /// ADR-0074 section 3.5 — re-publish this device's KeyPackage after a
    /// rotation: every row of the account was deleted by the directory, the
    /// device id stays, and the binding is NOT re-minted (it stays under the
    /// old key). Throws when the publish did not upload, so the rotation
    /// keeps the step owed.
    public func republishKeyPackageAfterKeyRotation() async throws {
        let before = keyPackageRepublisher.outcomes.count
        await republishKeyPackage().value
        guard keyPackageRepublisher.outcomes.count > before,
              keyPackageRepublisher.outcomes.last == .uploaded else {
            throw KeyResetPublishFailed(
                step: KeyRotationPlan.Step.keyPackage.rawValue,
                underlying: DiscoveryClientError.argument(
                    "keypackage republish did not upload: \(String(describing: keyPackageRepublisher.outcomes.last))"))
        }
    }

    // MARK: - Group operations

    /// Start a 1×1 MLS group with `recipientAccountId`.
    ///
    /// Steps:
    ///   1. Fetch the recipient's KeyPackage from the discovery
    ///      directory.
    ///   2. `Group.create(identity:kind:)` — fresh group with us as
    ///      the founding member.
    ///   3. `group.addMemberFromWire(identity:keyPackage:)` —
    ///      produces a `(commit, welcome)` pair. Per MLS the
    ///      commit would be broadcast, but our group is
    ///      one-of-one before this call, so we drop it.
    ///   4. Seal the welcome as a `.welcome` mailbox row (ADR-0078) to
    ///      the recipient's VERIFIED mailbox (a grant's or a link's
    ///      binding) and post it via
    ///      ``DsClient/publishMailbox(pseudonym:payload:writeAuth:)``.
    ///   5. Persist the MLS snapshot.
    ///
    /// Returns the new group's hex id so the caller can hand it
    /// straight to the Chat UI route.
    ///
    /// ADR-0062 — `kind` is written into the group's GroupContext at
    /// creation and never changes: `.oneToOne` for a contact accept / invite
    /// accept, `.group` for the new-group flow even while it holds two
    /// people. There is no default, so no caller can forget it.
    public func startGroupWith(
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        kind: GroupKind,
        recipientPlan: MailboxWritePlan? = nil
    ) async throws -> String {
        try await foundGroup(
            recipientAccountId: recipientAccountId, recipientSignaturePubkey: recipientSignaturePubkey,
            kind: kind, recipientPlan: recipientPlan, answeringRequest: nil
        ).groupIdHex
    }

    /// Found the one-to-one that answers the contact request `requestMessageId`.
    /// Its Welcome to the requester goes out under the request's reply grant,
    /// then a contact grant, and nothing else (``deliverAnswerWelcome(welcome:toRequester:)``).
    ///
    /// Once the group is founded and its journal claimed the answer is owed,
    /// durably, before any Welcome goes out: a Welcome that fails (the
    /// requester's mailbox refused it, a connection failed) keeps the group,
    /// is retried (``resolveAllOwedAnswers()``), and the answer is
    /// ``AnswerFounding/welcomeOwed(groupIdHex:)``. Only a failure before that
    /// (no way to answer, the claim refused) throws, with nothing standing.
    ///
    /// A request with a founding on record never founds a second group: the
    /// accept carries on from it.
    public func foundOneToOneAnswering(
        requestMessageId: Int64, recipientAccountId: Data, recipientSignaturePubkey: Data
    ) async throws -> AnswerFounding {
        if let accountIdB64 = currentAccountIdB64,
           let held = OwedAnswers.foundedGroupHex(
               requestMessageId: requestMessageId,
               rows: commitOutbox.loadUnacked(accountIdB64: accountIdB64)) {
            await resolveOwedAnswers(groupIdHex: held)
            // Whatever landed was acked by the resolver.
            return .welcomeOwed(groupIdHex: held)
        }
        return try await foundGroup(
            recipientAccountId: recipientAccountId, recipientSignaturePubkey: recipientSignaturePubkey,
            kind: .oneToOne, recipientPlan: nil, answeringRequest: requestMessageId)
    }

    private func foundGroup(
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        kind: GroupKind,
        recipientPlan: MailboxWritePlan?,
        answeringRequest: Int64?
    ) async throws -> AnswerFounding {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — call bootstrap() first"
            )
        }
        // ADR-0050 — a person is added as ALL of their devices, each its own
        // leaf; the recipient's device binding inside each leaf is what the
        // core verifies. ADR-0057 — remembered so the registrar appointment
        // can fetch the recipient's identity bundle.
        memberAccountIdHints[recipientSignaturePubkey] = recipientAccountId
        // `identity` is this binding's. Checked after every wait below: a
        // group founded under it must never be filed into the next account's
        // list, nor claimed and announced once its account has signed out.
        let bound = binding.capture()
        // ADR-0078 — the Welcome goes to the recipient's VERIFIED mailbox (the
        // link's or a held grant's binding). With none known nothing is
        // created: there is no address to derive any more.
        let recipientTarget = recipientPlan
            ?? ReachCenter.shared.writePlan(toAccountKey: recipientSignaturePubkey)
        let reachable = answeringRequest == nil
            ? recipientTarget != nil
            : !ReachCenter.shared.answerPlans(toRequester: recipientSignaturePubkey).isEmpty
        guard reachable else {
            NSLog("ADR-0078: no verified mailbox for the recipient; the chat was not created")
            throw ChatError.wrapped("no mailbox known for the recipient", underlying: MailboxUnknown())
        }
        // ADR-0073 R.2 / ADR-0074 3.5 — the identity gate for the account,
        // then only the devices bound to the key the caller expects this
        // person to have or a proven predecessor of it
        // (``expectedPersonDevices``); none left is the same answer as an
        // empty directory, and nothing was created.
        let recipientDevices = try await expectedPersonDevices(
            accountId: recipientAccountId, expectedKey: recipientSignaturePubkey)
        guard !recipientDevices.isEmpty else {
            throw ChatError.argument(
                "recipient has no KeyPackage uploaded yet"
            )
        }
        try bound.require()
        // ...and this account's OTHER devices join from the first epoch too.
        let devices = recipientDevices + (await ownOtherDevices(identity: identity))
        try bound.require()

        let group: Group
        let addResult: AddMember
        let joinSigner: DsProofSigner
        do {
            group = try Group.create(identity: identity, kind: kind)
            // ONE commit for every device (ADR-0050 2.6): we are the only
            // member, so the commit fans out to nobody and is dropped; the ONE
            // Welcome carries every added leaf's secrets and goes,
            // byte-identical, to each leaf's own mailbox. One commit per device
            // would strand every device but the last at a Welcome epoch we then
            // advanced past with nobody to tell.
            addResult = try group.addMembersFromWire(
                identity: identity,
                keyPackages: devices.map { $0.keyPackage }
            )
            joinSigner = try group.dsAccessSigner(identity: identity)
        } catch {
            throw ChatError.wrapped(
                "addMembersFromWire failed",
                underlying: error
            )
        }
        // ADR-0057 obligation 1 — the founder owns the group, so it claims the
        // journal with its ROOT key at the epoch the Welcome lands its members
        // at, BEFORE any Welcome goes out: a member must never hold an epoch
        // the server refuses. A failure aborts the creation (nothing was
        // published).
        do {
            try await JournalOwnership.claim(
                root: try journalRootSigner(groupId: try group.groupId()),
                journalId: try group.groupId(),
                epoch: joinSigner.epoch(),
                epochKey: joinSigner.verifyingKey(),
                ds: dsClient)
        } catch {
            throw ChatError.wrapped("DS journal claim failed", underlying: error)
        }
        try bound.require()
        if let accountIdB64 = currentAccountIdB64 {
            registrarSeeds.markRootHeld(
                accountIdB64: accountIdB64, groupIdHex: DsClient.pseudonymHex(try group.groupId()))
        }

        // ADR-0060 section 6 — one inbound mailbox per person: the ONE
        // Welcome goes to each added ACCOUNT's mailbox (the recipient's, and
        // this account's own when it has other devices), one write per
        // account, signed with the grant that reaches it.
        let welcome = addResult.welcome
        var targets: [(accountKey: Data, plan: MailboxWritePlan?)] = [
            (recipientSignaturePubkey, recipientTarget)
        ]
        // ADR-0079 (2026-10-17) — this account's own mailbox is written FIRST
        // (inserted at the front below) and the recipient's only after it:
        // whenever the recipient holds the Welcome, every own device's
        // mailbox holds it too.
        // ADR-0074 section 3.5a rule 1 — each account key is resolved before
        // its mailbox is derived: a contact stored under a key they have
        // since rotated away by proof, and this account's own devices (whose
        // bindings name the old key in Stage A), are written to at the
        // mailbox of the CURRENT key.
        if devices.count > recipientDevices.count,
           let own = ReachCenter.shared.ownAccountKey,
           !ReachCenter.shared.keyResolver.samePerson(own, recipientSignaturePubkey) {
            targets.insert((own, ReachCenter.shared.writePlan(toAccountKey: own)), at: 0)
        }

        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        // The founder's state is on disk BEFORE any Welcome goes out, fail
        // closed (the commit paths' discipline): the MLS snapshot holding the
        // new group, then its row in the known groups. A sign-out between a
        // Welcome and a best-effort persist used to lose the group on the
        // founder while its members joined it. A refused or failed persist
        // throws here, and nothing was published.
        let foundingAccount = try persistFoundingOrThrow(groupIdHex: groupIdHex)
        // The founding of an accept is on record before any Welcome goes out,
        // so whatever fails below, or a process death, leaves it owed and
        // another Accept carries on from it. A refused write of the record
        // means nothing was sent.
        var owedAnswer: PendingCommitEntry?
        if let answeringRequest {
            do {
                owedAnswer = try recordOwedAnswer(
                    group: group, groupId: groupId, welcome: welcome,
                    requesterKey: recipientSignaturePubkey, requestMessageId: answeringRequest,
                    ownWelcomeOwed: targets.count > 1)
            } catch {
                knownGroupsStore.remove(accountIdB64: foundingAccount, groupIdHex: groupIdHex)
                throw error
            }
        }
        var delivered = 0
        var welcomeOwed = false
        do {
            for target in targets {
                if answeringRequest != nil {
                    do {
                        if target.accountKey == recipientSignaturePubkey {
                            try await deliverAnswerWelcome(
                                welcome: welcome, toRequester: recipientSignaturePubkey)
                        } else {
                            try await deliverWelcome(
                                welcome: welcome, toAccountKey: target.accountKey, plan: target.plan)
                            owedAnswer = noteOwnWelcomeLanded(owedAnswer)
                        }
                    } catch {
                        NSLog("contact accept: the Welcome stays owed: %@", ContactAccept.reason(of: error))
                        welcomeOwed = true
                        break
                    }
                } else {
                    try await deliverWelcome(
                        welcome: welcome, toAccountKey: target.accountKey, plan: target.plan)
                }
                delivered += 1
                try bound.require()
            }
        } catch {
            // No Welcome went out: nobody else holds the group, so its row
            // goes again (the provider keeps the MLS state, as it did before
            // the row was written up front). Once one went out the group has
            // a member, and the founder keeps it for this account's next
            // start, signed out in between or not.
            if FoundingRollback.forgetsRow(welcomesDelivered: delivered) {
                knownGroupsStore.remove(accountIdB64: foundingAccount, groupIdHex: groupIdHex)
            }
            throw error
        }

        if !welcomeOwed, let owedAnswer {
            commitOutbox.deleteEntryBestEffort(accountIdB64: foundingAccount, id: owedAnswer.id)
        }
        groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
        // ADR-0073 R.5 — a group that continues a conversation is known to
        // the notification extension as that conversation.
        mirrorLineage()
        dsSigners.observe(journal: groupIdHex, signer: joinSigner)
        claimedGroupsThisSession.insert(groupIdHex)
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0042 — subscribe under `push_gid` (defers until this device
        // holds the group's `push_routing_secret`; never subscribes under
        // the DS `group_id`). The owner mints the secret on chat-open
        // (`ensurePushRouting`); until then this is a no-op deferral.
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        // ADR-0057 obligation 3 — appoint the recipient a registrar.
        reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
        // ADR-0048 §2 — a chat this device just founded has never heard its
        // name; the recipient joins at the epoch this is sealed under.
        publishOwnProfileSoon(groupIdHex: groupIdHex)
        // ADR-0048 §2 trigger 4 — and this device has never heard theirs.
        requestProfilesAfterJoin(groupIdHex: groupIdHex)
        return welcomeOwed ? .welcomeOwed(groupIdHex: groupIdHex) : .delivered(groupIdHex: groupIdHex)
    }

    /// Found a group with nobody else in it (`docs/ux/new-group.md`): this
    /// account's own devices only, so people are added afterwards as in any
    /// group. The founder claims the journal before anything else, as
    /// ``foundGroup(recipientAccountId:recipientSignaturePubkey:kind:recipientPlan:answeringRequest:)``
    /// does. A Welcome to this account's own mailbox that fails does not undo
    /// the group: the re-admission check brings those devices in.
    public func foundGroupWithNobodyElse(
        /// ADR-0087 sections 3 and 10 — run with the new group's id BEFORE
        /// anything about the group leaves this device (the @name claim): a
        /// throw here creates nothing.
        beforePublish: ((Data) async throws -> Void)? = nil
    ) async throws -> String {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        let bound = binding.capture()
        let ownDevices = await ownOtherDevices(identity: identity)
        try bound.require()
        let group: Group
        let welcome: Data?
        let joinSigner: DsProofSigner
        do {
            group = try Group.create(identity: identity, kind: .group)
            welcome = ownDevices.isEmpty
                ? nil
                : try group.addMembersFromWire(
                    identity: identity, keyPackages: ownDevices.map { $0.keyPackage }).welcome
            joinSigner = try group.dsAccessSigner(identity: identity)
        } catch {
            throw ChatError.wrapped("the group was not created", underlying: error)
        }
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        if let beforePublish {
            try await beforePublish(groupId)
            try bound.require()
        }
        do {
            try await JournalOwnership.claim(
                root: try journalRootSigner(groupId: groupId),
                journalId: groupId,
                epoch: joinSigner.epoch(),
                epochKey: joinSigner.verifyingKey(),
                ds: dsClient)
        } catch {
            throw ChatError.wrapped("DS journal claim failed", underlying: error)
        }
        try bound.require()
        if let accountIdB64 = currentAccountIdB64 {
            registrarSeeds.markRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
        _ = try persistFoundingOrThrow(groupIdHex: groupIdHex)
        if let welcome, let own = ReachCenter.shared.ownAccountKey {
            do {
                try await deliverWelcome(
                    welcome: welcome, toAccountKey: own, plan: ReachCenter.shared.writePlan(toAccountKey: own))
            } catch {
                NSLog("New group: the Welcome to this account's other devices did not land")
            }
            try bound.require()
        }
        groups[groupIdHex] = GroupRecord(group: group, groupId: groupId)
        mirrorLineage()
        dsSigners.observe(journal: groupIdHex, signer: joinSigner)
        claimedGroupsThisSession.insert(groupIdHex)
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
        return groupIdHex
    }

    /// The founding of an accept, on record in the outbox every owed Welcome
    /// lives in. A failed write throws: the accept then failed.
    private func recordOwedAnswer(
        group: Group, groupId: Data, welcome: Data, requesterKey: Data, requestMessageId: Int64,
        ownWelcomeOwed: Bool
    ) throws -> PendingCommitEntry {
        guard let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument("no account — cannot record the owed Welcome")
        }
        var entry = PendingCommitEntry(
            id: newOutboxEntryId(),
            accountId: accountIdB64,
            groupId: base64Encode(groupId),
            targetEpoch: String((try? group.epoch()) ?? 0),
            commitB64: "",
            secondArtifactB64: base64Encode(welcome),
            secondArtifactKind: .welcome,
            sweepVerdictPending: true,
            sweepWelcomesOwed: true,
            answerRequesterKeyB64: base64Encode(requesterKey),
            answerRequestMessageId: requestMessageId)
        entry.answerOwnWelcomeOwed = ownWelcomeOwed
        try commitOutbox.putEntryDurable(accountIdB64: accountIdB64, entry: entry)
        return entry
    }

    /// This account's own mailbox holds the Welcome: a retry writes only the
    /// requester's. Best effort: at worst the row is written twice.
    private func noteOwnWelcomeLanded(_ entry: PendingCommitEntry?) -> PendingCommitEntry? {
        guard var entry, let accountIdB64 = currentAccountIdB64 else { return entry }
        entry.answerOwnWelcomeOwed = false
        try? commitOutbox.putEntryDurable(accountIdB64: accountIdB64, entry: entry)
        return entry
    }

    /// What ``resolveOwedAnswers(groupIdHex:)`` calls once a Welcome landed:
    /// the accept is finished and its ack goes out. Set by
    /// ``ContactsRepository``.
    var owedAnswerAck: (@MainActor (_ requestMessageId: Int64, _ groupIdHex: String) async -> Void)?
    /// The groups whose owed answers are being written, so two triggers never
    /// write one Welcome twice at the same time.
    private var owedAnswersRunning: Set<String> = []

    /// Retry every accept whose Welcome is owed: at launch and at the tail of
    /// each inbox poll. Nothing owed reads one directory.
    func resolveAllOwedAnswers() async {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let owing = Set(commitOutbox.loadUnacked(accountIdB64: accountIdB64)
            .filter { $0.answerRequesterKeyB64 != nil }
            .compactMap { base64Decode($0.groupId) })
        for groupId in owing {
            await resolveOwedAnswers(groupIdHex: DsClient.pseudonymHex(groupId))
        }
    }

    /// Retry the accepts of `groupIdHex` whose Welcome is owed: at the tail of
    /// a poll, at launch, and when a key for someone arrived. A Welcome that
    /// landed drops its entry and then the ack goes out.
    func resolveOwedAnswers(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex],
              owedAnswersRunning.insert(groupIdHex).inserted else { return }
        defer { owedAnswersRunning.remove(groupIdHex) }
        let rows = commitOutbox.loadUnacked(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId)
        ).filter { $0.answerRequesterKeyB64 != nil }
        guard !rows.isEmpty else { return }
        let bound = binding.capture()
        await OwedAnswers.resolve(rows: rows, ports: OwedAnswers.Ports(
            deliver: { [self] row in
                guard let welcome = row.secondArtifactB64.flatMap(base64Decode),
                      let key = row.answerRequesterKeyB64.flatMap(base64Decode) else {
                    throw MailboxUnknown()
                }
                try bound.require()
                // Own mailbox first (ADR-0079): whenever the requester holds
                // the Welcome, every own device's mailbox holds it too.
                if row.answerOwnWelcomeOwed == true, let own = ReachCenter.shared.ownAccountKey {
                    try await deliverWelcome(
                        welcome: welcome, toAccountKey: own,
                        plan: ReachCenter.shared.writePlan(toAccountKey: own))
                    try bound.require()
                    _ = noteOwnWelcomeLanded(row)
                }
                try await deliverAnswerWelcome(welcome: welcome, toRequester: key)
            },
            landed: { [self] row in
                commitOutbox.deleteEntryBestEffort(accountIdB64: accountIdB64, id: row.id)
            },
            ack: { [self] messageId in
                guard bound.isCurrent else { return }
                await owedAnswerAck?(messageId, groupIdHex)
            }))
    }

    /// The Welcome of an accept, to the person who sent the request: under the
    /// request's reply grant, then a contact grant, moving on at a `401`,
    /// through ``ReachCenter/answerPlans(toRequester:)`` and nothing else. No
    /// such grant: nothing is written, and it throws.
    ///
    /// ADR-0074 section 3.5a rule 6 — a refusal of all of them re-checks the
    /// person's address once.
    func deliverAnswerWelcome(welcome: Data, toRequester accountKey: Data) async throws {
        do {
            _ = try await RefusedMailboxWrite.write(
                resolve: {
                    let plans = ReachCenter.shared.answerPlans(toRequester: accountKey)
                    guard !plans.isEmpty else {
                        NSLog("ADR-0078: no grant answers a request; its Welcome was not written")
                        throw MailboxUnknown()
                    }
                    return plans
                },
                recheck: { [self] in
                    await PersonAddressRecheck.recheck(
                        accountKey: accountKey, accountIdHint: memberAccountIdHints[accountKey])
                },
                publish: { [self] plans in
                    try await RefusedMailboxWrite.tryingPlans(plans) { plan in
                        let row = try plan.target.seal(kind: .welcome, body: welcome)
                        return try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                            try await dsClient.publishMailbox(
                                pseudonym: plan.target.address, payload: row, writeAuth: auth)
                        }
                    }
                })
        } catch {
            throw ChatError.wrapped("DS publishMailbox (Welcome) failed", underlying: error)
        }
    }

    /// Write one Welcome to one account's mailbox, as a sealed `.welcome`
    /// row (ADR-0078) to the verified mailbox `plan` names (a link's, handed
    /// in by the caller) or, when the caller has none, the one a held grant
    /// names. No mailbox known: nothing is written, and it throws.
    ///
    /// ADR-0074 section 3.5a rule 6 — a refused Welcome re-checks the
    /// person's address once. After the re-check a grant held for the person
    /// goes first: one the caller handed in (a link's) opened the mailbox it
    /// was issued for only.
    func deliverWelcome(
        welcome: Data, toAccountKey accountKey: Data, plan: MailboxWritePlan?
    ) async throws {
        do {
            var rechecked = false
            _ = try await RefusedMailboxWrite.write(
                resolve: { [self] in
                    let held = ReachCenter.shared.writePlan(toAccountKey: accountKey)
                    guard let chosen = rechecked ? (held ?? plan) : (plan ?? held) else {
                        NSLog("ADR-0078: no verified mailbox for a Welcome; it was not written")
                        throw MailboxUnknown()
                    }
                    return chosen
                },
                recheck: { [self] in
                    rechecked = true
                    await PersonAddressRecheck.recheck(
                        accountKey: accountKey,
                        accountIdHint: memberAccountIdHints[accountKey])
                },
                publish: { [self] plan in
                    let row = try plan.target.seal(kind: .welcome, body: welcome)
                    return try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                        try await dsClient.publishMailbox(
                            pseudonym: plan.target.address, payload: row, writeAuth: auth)
                    }
                })
        } catch {
            throw ChatError.wrapped(
                "DS publishMailbox (Welcome) failed",
                underlying: error
            )
        }
    }

    /// Join an MLS group from a `Welcome` blob.
    ///
    /// Returns the joined group's hex id.
    ///
    /// Every attempt, success or failure, ends by requesting a KeyPackage
    /// republish: one published package is a last-resort package (ADR-0050
    /// section 2.6) and a fresh one narrows how long it is used. Its private
    /// keys survive a join, so a Welcome refused here can be tried again.
    ///
    /// ADR-0087 section 9, C-88 — a Welcome replaces an ended group under the
    /// same id, and is refused with `MembershipGroupStillActive`, changing
    /// nothing, for a group this device still holds as active. That refusal
    /// is not a failure of the Welcome: the removal it answers may simply
    /// still be in the journal, so the caller holds it
    /// (``holdWelcomeForActiveGroup(groupIdHex:welcome:)``) instead of losing
    /// it. When the replaced group's person was out, nothing of it carries;
    /// when only this device's leaf was retired, the history does.
    public func joinGroupFromWelcome(welcomeBytes: Data) async throws -> String {
        defer { republishKeyPackage() }
        guard let identity = canonicalIdentity else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — call bootstrap() first"
            )
        }
        let group: Group
        do {
            group = try Group.joinFromWelcome(
                welcome: welcomeBytes,
                identity: identity
            )
        } catch {
            throw ChatError.wrapped(
                "joinFromWelcome failed",
                underlying: error
            )
        }
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let record = GroupRecord(group: group, groupId: groupId)
        // ADR-0087 section 9 — what this Welcome replaced, if anything.
        closeReplacedGroup(groupIdHex: groupIdHex, joined: record)
        // What the log holds for this group goes in with it: the persist below
        // writes the whole log, and a record registered empty would write this
        // group's history bundle away.
        if let waiting = waitingLogSlots.take(groupIdHex) { record.adoptHistory(waiting) }
        groups[groupIdHex] = record
        persistKnownGroupBestEffort(groupIdHex: groupIdHex)
        // A later join clears a held end (rule 3: a first join writes nothing).
        publishJoinedSoon(groupIdHex: groupIdHex)
        // ADR-0060 section 6 — a `/g/` request of ours was answered: its
        // single-use `reply` key goes.
        ReachCenter.shared.retireReplies(purpose: ReplyPurpose.groupJoin(groupId))
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0057 — the committer registered the epoch this Welcome lands at
        // before publishing it; a joiner never claims or heals. (The owner's
        // other device joining here claims lazily, only after a `401`.)
        claimedGroupsThisSession.insert(groupIdHex)
        // ADR-0042 — subscribe under `push_gid` (defers until the joiner
        // receives the group's `push_routing_secret` over the distribution
        // envelope on its next poll; never subscribes under `group_id`).
        subscribeGroupUnderPushGid(groupIdHex: groupIdHex)
        // ADR-0048 §2 — joining: tell the group who this is.
        publishOwnProfileSoon(groupIdHex: groupIdHex)
        // ADR-0048 §2 trigger 4 — and ask who everyone else is: every profile
        // already in the journal was sealed in an epoch this device cannot
        // open, and only a member whose device notices the growth re-sends.
        requestProfilesAfterJoin(groupIdHex: groupIdHex)
        return groupIdHex
    }

    /// ADR-0048 §2 — publish the own profile into `groupIdHex` without making
    /// the caller wait on the network. Fire-and-forget: the send takes the
    /// per-group commit chain on its own.
    func publishOwnProfileSoon(groupIdHex: String) {
        // Asked for by the account that just founded or joined the group.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard bound.isCurrent else { return }
            await self?.profilePublisher.publishNow(groupIdHex: groupIdHex)
        }
    }

    /// ADR-0048 §2 trigger 4 — ask `groupIdHex` to introduce itself right
    /// after joining it. Fire-and-forget like the profile publish beside it,
    /// and sequenced after it on the same per-group send chain, so the group
    /// reads "here is who I am" before "who are you".
    func requestProfilesAfterJoin(groupIdHex: String) {
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard bound.isCurrent else { return }
            await self?.profileRequester.noteJoined(groupIdHex: groupIdHex)
        }
    }

    /// ADR-0048 §2 trigger 4 — the chat for `groupIdHex` was opened: ask the
    /// group to introduce itself if someone in it still has no name here and
    /// this device has not asked within the core's interval. The view model
    /// calls this from the screen's appear path; the rule decides, so calling
    /// it on every appear costs nothing when nothing is due.
    public func requestProfilesOnOpen(groupIdHex: String) async {
        await profileRequester.noteOpened(groupIdHex: groupIdHex)
    }

    /// ADR-0048 §2 trigger 4 — answer another member's request by
    /// republishing this device's own profile through the growth path: same
    /// throttle, same jitter, so N members answering one request do not land
    /// N envelopes in the same second. A device with no own profile publishes
    /// nothing, exactly as it does on growth.
    ///
    /// ADR-0060 section 4, "A device that holds nothing asks" — the same
    /// envelope from the other person of a one-to-one also means they may
    /// hold nothing of this account's: that conversation is forgotten in the
    /// sent records, at most once an hour, so the offer and the bundle go out
    /// again. The profile answer above is unchanged and is what carries the
    /// contact grant.
    private func answerProfileRequest(groupIdHex: String, senderKey: MemberIdentity) {
        profilePublisher.noteGrowth(groupIdHex: groupIdHex)
        if ReachCenter.shared.noteIntroductionsAsked(
            groupIdHex: groupIdHex,
            isOneToOneWithAsker: isOneToOneWithOther(groupIdHex: groupIdHex, senderKey: senderKey),
            nowMs: UInt64(max(0, Self.nowMs()))) {
            requestIntroductions()
        }
    }

    /// ADR-0048 §2 trigger 4 — how many PEOPLE in `groupIdHex`, this account
    /// excluded, still resolve to the neutral placeholder on this device; `nil`
    /// when there is no handle or the roster cannot be read. Counted per
    /// person (ADR-0050 collapses a person's device leaves), against the one
    /// resolver every surface names people through, so "unnamed" here is
    /// exactly what the user sees as `Member`.
    func unnamedMemberCount(groupIdHex: String) -> Int? {
        guard let record = groups[groupIdHex],
              let people = members(groupIdHex: groupIdHex)
        else { return nil }
        let own = AuthorIdentity.own(group: record.group)
        return people.filter { person in
            base64Encode(person.signaturePublicKey) != own
                && ProfileDirectory.shared.countsAsUnnamed(signatureKey: person.signaturePublicKey)
        }.count
    }

    // MARK: - Calls

    /// The conversation's title, as the chat list and the chat header write it
    /// (ADR-0051 §5): the shared group name when one is held, else the other
    /// person's resolved name in a 1x1, else the members' names. Never a hex
    /// id.
    ///
    /// Lives here, rather than only on the view models that used to own a copy
    /// each, because a call outlives every chat screen: ``CallHost`` needs the
    /// name with no view model in existence (`docs/ux/calls.md` section 4).
    public func conversationTitle(groupIdHex: String) -> String {
        if let shared = groupName(groupIdHex: groupIdHex) { return shared }
        // ADR-0062 — a one-to-one is titled by the person, never by the
        // group-title fallback ("Group"). Their name is on this device; when
        // it is not (a defect), the resolver's own answer stands.
        if isOneToOne(groupIdHex: groupIdHex) {
            if let peer = oneToOnePeerSignatureKey(groupIdHex: groupIdHex) {
                return ProfileDirectory.shared.resolve(signatureKey: peer).name
            }
            return ResolvedDisplayName.placeholderMember
        }
        let members = self.members(groupIdHex: groupIdHex) ?? []
        let own = ownLeafIndex(groupIdHex: groupIdHex)
        return ConversationTitle.groupTitle(
            otherMemberNames: members
                .filter { $0.leafIndex != own }
                .map { member in
                    let resolved = ProfileDirectory.shared.resolve(
                        signatureKey: member.signaturePublicKey)
                    return resolved.isPlaceholder ? nil : resolved.name
                })
    }

    /// The people of this device's conversations, as person keys (base64),
    /// most recently active conversation first; this device's own person is
    /// left out. The order a backup keeps received names in after contacts.
    func peopleByRecentUse() -> [String] {
        let byActivity = groups.values.sorted {
            ($0.messages.last?.createdAt ?? "") > ($1.messages.last?.createdAt ?? "")
        }
        var out: [String] = []
        var seen = Set<String>()
        for record in byActivity {
            let own = try? record.group.ownLeafIndex()
            for member in (try? record.group.members()) ?? [] where member.leafIndex != own {
                let key = PersonKey.of(member).base64EncodedString()
                if seen.insert(key).inserted { out.append(key) }
            }
        }
        return out
    }

    /// The title the notification extension may show for `groupIdHex`: the
    /// conversation's title, or `nil` (the row is removed) when it would be a
    /// placeholder: a one-to-one whose person this device has no name for, or
    /// a group with no name and no named member. The extension then shows its
    /// own unnamed wording; it never shows "Group" or "Member" for a person.
    func mirroredConversationTitle(groupIdHex: String) -> String? {
        if let shared = groupName(groupIdHex: groupIdHex) { return shared }
        if isOneToOne(groupIdHex: groupIdHex) {
            guard let peer = oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return nil }
            let resolved = ProfileDirectory.shared.resolve(signatureKey: peer)
            return resolved.isPlaceholder ? nil : resolved.name
        }
        let title = conversationTitle(groupIdHex: groupIdHex)
        return title == Localization.text("chat.group.fallback_name") ? nil : title
    }

    /// `docs/ux/calls.md` section 2.1 — refresh the background name mirror for
    /// every conversation this device knows.
    ///
    /// A ringing phone has to show a name before anything is open, and the
    /// process that raises that alert — the notification-service extension —
    /// cannot resolve one: it has no profile directory and no roster, by design
    /// (C-59). So the app writes the names it has already resolved into the App
    /// Group suite and the extension only reads them. See
    /// ``CallPeerNameMirror`` for what that costs and what it deliberately is
    /// not.
    ///
    /// Cheap and idempotent: a name that has not changed is a read and no
    /// write. Called when the app becomes active and after the chat bootstrap,
    /// which is often enough — a name that is one session stale costs the lock
    /// screen an old name, and the system call screen is corrected by a call
    /// update as soon as the app decrypts the offer.
    public func mirrorCallPeerNames() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        for groupIdHex in groups.keys {
            CallPeerNameMirror.mirror(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                name: mirroredConversationTitle(groupIdHex: groupIdHex)
            )
        }
        // Reactions-and-emoji spec section 12 — the people in these
        // conversations, by account key, for the reaction notification.
        let names = Self.personNameMap(
            rosters: groups.values.map { (try? $0.group.members()) ?? [] },
            resolve: { key in
                let resolved = ProfileDirectory.shared.resolve(signatureKey: key)
                return resolved.source == .placeholder ? nil : resolved.name
            })
        PersonNameMirror.replace(accountIdB64: accountIdB64, names: names)
    }

    /// The names a notification may use, keyed by every key the extension
    /// can be handed for a member: the PERSON key (the account key the core
    /// resolves the sender to, else the leaf key) and each of the member's
    /// leaf keys, all mapped to the one name the app shows for that person.
    /// Every member of every loaded group, not only contacts: a group
    /// member's profile arrived in the group and is stored under that same
    /// person key. A member with no known name is left out; the extension
    /// then says the chat's own word for an unnamed member.
    nonisolated static func personNameMap(
        rosters: [[GroupMember]], resolve: (Data) -> String?
    ) -> [String: String] {
        var names: [String: String] = [:]
        for roster in rosters {
            for member in roster {
                let person = PersonKey.of(member)
                guard let name = resolve(person) else { continue }
                names[person.base64EncodedString()] = name
                names[member.signaturePublicKey.base64EncodedString()] = name
            }
        }
        return names
    }

    /// `docs/ux/calls.md` section 6 — write a finished call's row into the
    /// conversation.
    ///
    /// Local to this device and never sent: both ends were in the same call
    /// and each writes what it saw, so there is nothing to agree on. The row
    /// is an ordinary system row in the timeline, which is what makes it
    /// survive a relaunch — the encrypted message log persists it with every
    /// other message, by the same route and with no new store.
    ///
    /// Its id is **negative**, below every id already in this group. DS ids are
    /// positive and monotone, so a local row can never collide with one, can
    /// never be the target of an edit / delete / reaction arriving from the
    /// wire, and cannot move the journal cursor.
    public func appendCallRow(groupIdHex: String, row: CallTranscriptRow, atMs: Int64? = nil) {
        guard let record = groups[groupIdHex] else { return }
        let lowest = record.messages.map(\.id).min() ?? 0
        record.appendLocalRow(
            text: row.marker,
            messageId: min(0, lowest) - 1,
            createdAt: Self.isoTimestamp(ms: atMs ?? Self.nowMs()))
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// `docs/ux/navigation.md` section 6.2 — file a finished call BOTH ways at
    /// once: the row the conversation shows, and the entry the flat Calls list
    /// reads.
    ///
    /// The two writes live together because the spec's requirement is an
    /// ordering one — the Calls list must be an index written when the row is,
    /// never a scan of every transcript — and splitting them across two call
    /// sites is how an index stops being written for one of the two call kinds
    /// without anything failing.
    public func recordFinishedCall(_ call: FinishedCall) {
        // The one-time fill from past calls runs BEFORE this call's row
        // exists. It keys a call on the row's time, which is when the call
        // ended, while this entry keys it on when it started; run after a
        // live record (the first opening of the Calls screen used to be what
        // ran it), it filed every call already recorded a second time. Seen
        // in the simulator on 2026-10-23: each call listed twice, a minute
        // apart. Run first, it only ever sees calls from before the index.
        backfillCallHistory()
        appendCallRow(groupIdHex: call.groupIdHex, row: call.row)
        guard let accountIdB64 = currentAccountIdB64 else { return }
        let (outcome, durationMs) = CallHistoryEntry.outcome(of: call.row)
        CallHistoryStore.shared.bind(accountIdB64: accountIdB64)
        CallHistoryStore.shared.record(CallHistoryEntry(
            // The conversation and the start are not unique on their own (two
            // calls can start in one millisecond), so the direction and the
            // outcome join them: the pair of rows a 1x1 leaves on two devices
            // is one row each here, and a re-record of the same call replaces
            // rather than duplicates.
            id: "\(call.groupIdHex).\(call.startedAtMs).\(call.row.direction.rawValue)",
            conversationId: call.groupIdHex,
            kind: conversationKind(groupIdHex: call.groupIdHex) == .group ? .group : .chat,
            direction: call.row.direction,
            outcome: outcome,
            video: call.video,
            startedAtMs: call.startedAtMs,
            durationMs: durationMs))
    }

    /// Fill the cross-conversation call index from calls that happened before
    /// it existed. Once per account.
    ///
    /// Found live rather than by a test: the index only ever received calls
    /// placed after it shipped, so on a device that had been calling for
    /// months the Calls screen opened empty while those same calls were
    /// visibly sitting in the conversation list. A screen that contradicts the
    /// one next to it reads as broken, whatever the honest reason.
    ///
    /// It walks the transcripts this device already holds in memory, which is
    /// the per-open cost section 6.2 refuses — but at most ONCE per account,
    /// ever, and what it produces is the index the screen then reads. The row
    /// it reads is a machine marker (``CallTranscriptRow/parse(_:)``), never
    /// the sentence a reader sees, so nothing is recovered out of words.
    ///
    /// One thing is genuinely lost and is not guessed at: the marker never
    /// carried whether the call had video, so a backfilled row calls back by
    /// voice. Inventing a camera the record does not mention would be worse
    /// than the row being modest about what it knows.
    public func backfillCallHistory() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        CallHistoryStore.shared.bind(accountIdB64: accountIdB64)
        CallHistoryStore.shared.backfillOnce {
            var found: [CallHistoryEntry] = []
            for (hex, record) in groups {
                let kind: CallHistoryKind =
                    conversationKind(groupIdHex: hex) == .group ? .group : .chat
                for message in record.messages {
                    guard let row = CallTranscriptRow.parse(message.text) else { continue }
                    // A row whose time cannot be read has no place in a list
                    // whose whole order is time, so it stays where it is.
                    guard let at = Self.parseDsTimestampMs(message.createdAt) else { continue }
                    let (outcome, durationMs) = CallHistoryEntry.outcome(of: row)
                    found.append(CallHistoryEntry(
                        id: "\(hex).\(at).\(row.direction.rawValue)",
                        conversationId: hex,
                        kind: kind,
                        direction: row.direction,
                        outcome: outcome,
                        video: false,
                        startedAtMs: at,
                        durationMs: durationMs))
                }
            }
            return found.sorted { $0.startedAtMs < $1.startedAtMs }
        }
    }

    /// ADR-0062 — the kind the creator fixed in the group's GroupContext, the
    /// same on every member's device: `.oneToOne` or `.group`. `nil` when
    /// there is no handle, the read fails, or the group carries no kind at
    /// all (never a conversation). The kind never changes, so no commit can
    /// make a read of it stale.
    public func groupKind(groupIdHex: String) -> GroupKind? {
        guard let record = groups[groupIdHex] else { return nil }
        do {
            return try record.group.groupKind()
        } catch {
            NSLog("groupKind failed for \(groupIdHex): \(error)")
            return nil
        }
    }

    /// Which list `groupIdHex` belongs to, read from the group itself
    /// (``groupKind(groupIdHex:)``, ADR-0062); `nil` counts as a group
    /// (``ConversationKind/of(groupKind:)``). Asked by the conversation list,
    /// the Calls index, the chat screen and the pickers — one read, so they
    /// cannot disagree.
    public func conversationKind(groupIdHex: String) -> ConversationKind {
        ConversationKind.of(groupKind: groupKind(groupIdHex: groupIdHex))
    }

    /// `true` for a one-to-one by the kind its creator fixed (ADR-0062). A
    /// two-person GROUP is not one.
    public func isOneToOne(groupIdHex: String) -> Bool {
        conversationKind(groupIdHex: groupIdHex) == .chat
    }

    /// An ISO-8601 stamp in the shape ``parseDsTimestampMs`` reads, for the
    /// rows this device writes itself.
    nonisolated static func isoTimestamp(ms: Int64) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    // MARK: - ADR-0051 group name

    /// ADR-0051 §5 — the shared group name this device holds for
    /// `groupIdHex`, or `nil` when none was ever set / it was cleared. The
    /// title resolvers read this first, then fall back to the member-derived
    /// title.
    public func groupName(groupIdHex: String) -> String? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)?.displayName
    }

    /// ADR-0051 §3 — set (or, with an empty `name`, clear) the group's shared
    /// name: compose the envelope with a fresh stamp, send it into the group
    /// through the locked application path, and only THEN write the store and
    /// re-title — so a failed send changes nothing locally (UX spec §4:
    /// "the local title does not change until the send succeeds"). Throws
    /// what the send threw, or the codec's refusal of a name that fails a
    /// bound; the caller shows the same error copy as a failed message.
    ///
    /// The role check is the receivers' (every peer runs the gate against its
    /// live role table); locally the UI only offers the control to the owner
    /// and admins, read from the live group.
    public func setGroupName(groupIdHex: String, name: String) async throws {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let stamp = UInt64(max(0, Self.nowMs()))
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let bytes = try composeGroupNameEnvelope(
            groupName: FfiGroupName(name: trimmed, updatedAtMs: stamp))
        let bound = binding.capture()
        try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
        // Under the name chain so a concurrent receive cannot interleave its
        // read-decide-write with this write.
        try await groupNameLock.run(groupIdHex: groupIdHex) { [self] in
            groupNameStore.record(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                value: StoredGroupName(name: trimmed, updatedAtMs: stamp))
            // The store row above is keyed on the account that sent the name.
            // The re-send schedule and the revision are whoever is signed in
            // now.
            guard bound.isCurrent else { return }
            groupNamePublisher.notePublished(groupIdHex: groupIdHex)
            groupNameRevision &+= 1
            notePublicRecordChanged(groupIdHex: groupIdHex)
        }
    }

    /// ADR-0051 §2 — a group-name envelope arrived on the group journal: run
    /// the role gate against the LIVE group, then the strictly-newer order,
    /// then (on accept) store it and re-title. Fire-and-forget; the whole
    /// read-decide-write runs under the per-group name chain.
    public func receiveGroupName(
        groupIdHex: String,
        name: String,
        updatedAtMs: UInt64,
        senderLeaf: UInt32?
    ) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        // The name was received by this binding's account: it is not judged
        // against, nor stored for, an account that signed in while it queued.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self else { return }
            try? await self.groupNameLock.run(groupIdHex: groupIdHex) { [weak self] in
                guard bound.isCurrent else { return }
                self?.performReceiveGroupName(
                    groupIdHex: groupIdHex, name: name, updatedAtMs: updatedAtMs,
                    senderLeaf: senderLeaf)
            }
        }
    }

    /// The locked body of ``receiveGroupName(groupIdHex:name:updatedAtMs:senderLeaf:)``.
    /// Reads the live `Group` on the actor (roster + role table are the ONLY
    /// authority for who the sender is and what they may do), then the
    /// CURRENT stored row, and decides through ``GroupNameGate``.
    func performReceiveGroupName(
        groupIdHex: String,
        name: String,
        updatedAtMs: UInt64,
        senderLeaf: UInt32?
    ) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        // ADR-0062 section 7 — a one-to-one has no group name; applying one
        // would retitle the person. Dropped on receipt, read from the group's
        // own kind (the same answer on every member's device).
        guard ConversationKind.acceptsGroupName(groupKind: groupKind(groupIdHex: groupIdHex)) else {
            NSLog("ADR-0062 group name for \(groupIdHex) dropped: a one-to-one has no group name")
            return
        }
        let resolved = PushGroupRouting.resolveSenderAuthority(
            group: record.group, senderLeaf: senderLeaf)
        let stored = groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let verdict = GroupNameGate.classify(
            incomingName: name,
            incomingUpdatedAtMs: updatedAtMs,
            stored: stored,
            senderKey: resolved.senderKey,
            authority: resolved.authority
        )
        switch verdict {
        case .acceptIdentical:
            return
        case .accept:
            groupNameStore.record(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                value: StoredGroupName(name: name, updatedAtMs: updatedAtMs))
            groupNameRevision &+= 1
            // An admin's rename reaches the owner as this envelope.
            notePublicRecordChanged(groupIdHex: groupIdHex)
        case .rejectNotAuthorized, .rejectStale, .rejectUnverifiable:
            NSLog("ADR-0051 group name for \(groupIdHex) dropped: \(verdict)")
        }
    }

    /// ADR-0051 §3 — the envelope the growth re-send carries: the STORED name
    /// with its STORED stamp, verbatim. `nil` when this device holds no name
    /// for the group, is a plain member of a role-table group (its re-send
    /// would be refused by every peer), or can no longer send into it.
    func groupNameEnvelopeForResend(groupIdHex: String) -> Data? {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil,
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              let stored = groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
              stored.displayName != nil
        else { return nil }
        if ownRole(groupIdHex: groupIdHex) == .member { return nil }
        return try? composeGroupNameEnvelope(
            groupName: FfiGroupName(name: stored.name, updatedAtMs: stored.updatedAtMs))
    }

    /// ADR-0073 R.7 case 3 — carry the group's name into the successor with
    /// its ORIGINAL stamp (ADR-0051), so a later rename there still wins and
    /// a re-send can never clobber one. Idempotent: nothing is sent once the
    /// successor holds a name at least as new.
    func carryGroupName(oldGroupIdHex: String, successorGroupIdHex: String) async throws {
        guard let accountIdB64 = currentAccountIdB64, groups[successorGroupIdHex] != nil,
              let stored = groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: oldGroupIdHex),
              stored.displayName != nil
        else { return }
        if let held = groupNameStore.load(accountIdB64: accountIdB64, groupIdHex: successorGroupIdHex),
           held.updatedAtMs >= stored.updatedAtMs { return }
        let bytes = try composeGroupNameEnvelope(
            groupName: FfiGroupName(name: stored.name, updatedAtMs: stored.updatedAtMs))
        try await publishApplicationMessage(groupIdHex: successorGroupIdHex, plaintextBytes: bytes)
        try await groupNameLock.run(groupIdHex: successorGroupIdHex) { [self] in
            groupNameStore.record(
                accountIdB64: accountIdB64, groupIdHex: successorGroupIdHex,
                value: StoredGroupName(name: stored.name, updatedAtMs: stored.updatedAtMs))
            groupNamePublisher.notePublished(groupIdHex: successorGroupIdHex)
            groupNameRevision &+= 1
        }
    }

    // MARK: - ADR-0052 decision 3 — pins

    /// The conversation's shared pin map this device holds, pinned and
    /// unpinned rows alike (the bar filters; the gate needs both).
    public func sharedPins(groupIdHex: String) -> [Int64: StoredSharedPin] {
        guard let accountIdB64 = currentAccountIdB64 else { return [:] }
        return sharedPinStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// This device's own pins in the conversation, `messageId -> pinnedAtMs`.
    public func personalPins(groupIdHex: String) -> [Int64: UInt64] {
        guard let accountIdB64 = currentAccountIdB64 else { return [:] }
        return personalPinStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// UX spec section 4.1 "The sheet" — may this device pin for everyone
    /// here? Either participant of a one-to-one; the owner or an admin of a
    /// role-table group; anyone in a group with no table; nobody once their
    /// membership ended. Read from the LIVE group, never a cached role. The
    /// receivers run the same rule against their own live group; this only
    /// decides what the sheet offers.
    public func maySetSharedPin(groupIdHex: String) -> Bool {
        guard groups[groupIdHex] != nil else { return false }
        return PinGate.maySetShared(
            oneToOne: isOneToOne(groupIdHex: groupIdHex),
            ownRole: ownRole(groupIdHex: groupIdHex),
            membershipEnded: membershipEnded(membershipStateNow(groupIdHex: groupIdHex)))
    }

    /// "Pin for me" / "Unpin for me": a flag on this device. Nothing is sent.
    public func setPersonalPin(groupIdHex: String, messageId: Int64, pinned: Bool) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        if pinned {
            personalPinStore.set(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId,
                pinnedAtMs: UInt64(max(0, Self.nowMs())))
        } else {
            personalPinStore.remove(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId)
        }
        pinRevision &+= 1
    }

    /// "Pin for everyone" / "Unpin for everyone": compose ONE envelope for ONE
    /// message stamped with this device's clock, send it through the locked
    /// application path, and only THEN apply it locally exactly as a receiver
    /// would — the sender never receives its own message back (UX spec
    /// section 4.1 "What travels"). A failed send changes nothing locally and
    /// throws what the send threw.
    ///
    /// The standing check is the receivers' (every peer runs ``PinGate``
    /// against its live group); locally the sheet only offers the row when
    /// ``maySetSharedPin(groupIdHex:)`` says so.
    public func setSharedPin(groupIdHex: String, messageId: Int64, pinned: Bool) async throws {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let stamp = UInt64(max(0, Self.nowMs()))
        let op = FfiPinOp(messageId: messageId, pinned: pinned, updatedAtMs: stamp)
        let bytes = composePinEnvelope(op: op)
        let bound = binding.capture()
        try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bytes)
        // Under the pin chain so a concurrent receive cannot interleave its
        // read-decide-write with this write.
        try await pinLock.run(groupIdHex: groupIdHex) { [self] in
            let stored = sharedPinStore.load(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex)[messageId]
            // The same order a receiver applies: a stamp that is not newer
            // than what landed meanwhile (another admin's action) does not win
            // here either, so this device never disagrees with the room.
            if let stored, !pinIsNewer(incomingUpdatedAtMs: stamp, storedUpdatedAtMs: stored.updatedAtMs) {
                return
            }
            sharedPinStore.record(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId,
                value: StoredSharedPin(pinned: pinned, updatedAtMs: stamp))
            // As in ``setGroupName(groupIdHex:name:)``.
            guard bound.isCurrent else { return }
            pinPublisher.notePublished(groupIdHex: groupIdHex)
            pinRevision &+= 1
        }
    }

    /// A pin envelope arrived on the conversation's journal: run the standing
    /// check against the LIVE group, then the strictly-newer order for that
    /// message, then (on accept) store it. Fire-and-forget; the whole
    /// read-decide-write runs under the per-group pin chain.
    public func receivePin(groupIdHex: String, op: FfiPinOp, senderLeaf: UInt32?) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        // As in ``receiveGroupName(groupIdHex:name:updatedAtMs:senderLeaf:)``.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self else { return }
            try? await self.pinLock.run(groupIdHex: groupIdHex) { [weak self] in
                guard bound.isCurrent else { return }
                self?.performReceivePin(groupIdHex: groupIdHex, op: op, senderLeaf: senderLeaf)
            }
        }
    }

    /// The locked body of ``receivePin(groupIdHex:op:senderLeaf:)``. Reads the
    /// live `Group` on the actor (roster + role table + people count are the
    /// ONLY authority for who the sender is and what they may do), then the
    /// CURRENT stored row for that message, and decides through ``PinGate``.
    private func performReceivePin(groupIdHex: String, op: FfiPinOp, senderLeaf: UInt32?) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        // A pin of a message this device holds as a tombstone is dropped
        // outright: the record was dropped with the message and must not come
        // back through a late envelope.
        if record.messages.contains(where: { $0.messageId == op.messageId && $0.deleted }) {
            return
        }
        let resolved = PushGroupRouting.resolveSenderAuthority(
            group: record.group, senderLeaf: senderLeaf)
        let stored = sharedPinStore.load(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex)[op.messageId]
        let verdict = PinGate.classify(
            incomingPinned: op.pinned,
            incomingUpdatedAtMs: op.updatedAtMs,
            stored: stored,
            senderKey: resolved.senderKey,
            authority: resolved.authority,
            oneToOne: isOneToOne(groupIdHex: groupIdHex)
        )
        switch verdict {
        case .acceptIdentical:
            return
        case .accept:
            sharedPinStore.record(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: op.messageId,
                value: StoredSharedPin(pinned: op.pinned, updatedAtMs: op.updatedAtMs))
            pinRevision &+= 1
        case .rejectNotAuthorized, .rejectStale, .rejectUnverifiable:
            NSLog("ADR-0052 pin for \(groupIdHex) dropped: \(verdict)")
        }
    }

    /// UX spec section 4.1 — a deleted message leaves the bar and its shared
    /// pin record is dropped with it. Runs under the pin chain so it cannot
    /// race a receive; the receive path independently refuses a pin of a
    /// tombstone, so the two agree whichever runs first.
    private func dropSharedPinsOfDeletedMessages(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        let held = record.messages
        Task { @MainActor [weak self] in
            guard let self else { return }
            try? await self.pinLock.run(groupIdHex: groupIdHex) { [weak self] in
                guard let self else { return }
                let shared = self.sharedPinStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                let doomed = PinnedBar.recordsToDrop(shared: shared, held: held)
                guard !doomed.isEmpty else { return }
                for messageId in doomed {
                    self.sharedPinStore.remove(
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId)
                    // The personal bookmark of a deleted message goes too: the
                    // bar could never show it again.
                    self.personalPinStore.remove(
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId)
                }
                self.pinRevision &+= 1
            }
        }
    }

    /// The envelopes the growth re-send carries: every CURRENTLY PINNED shared
    /// row with its STORED stamp, verbatim, newest first. Empty when this
    /// device holds none, may not set shared pins here (its re-send would be
    /// refused by every peer), or can no longer send into the group. Unpins
    /// are not re-sent: a newcomer holds nothing to undo.
    private func pinEnvelopesForResend(groupIdHex: String) -> [Data] {
        guard let accountIdB64 = currentAccountIdB64, groups[groupIdHex] != nil,
              maySetSharedPin(groupIdHex: groupIdHex)
        else { return [] }
        return sharedPinStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            .filter { $0.value.pinned }
            .sorted { $0.value.updatedAtMs > $1.value.updatedAtMs }
            .map {
                composePinEnvelope(
                    op: FfiPinOp(messageId: $0.key, pinned: true, updatedAtMs: $0.value.updatedAtMs))
            }
    }

    /// ADR-0048 §2 — the own profile changed: publish it into every group.
    public func publishOwnProfileToAllGroups() async {
        await profilePublisher.publishToAll(groupIdHexes: Array(groups.keys).sorted())
    }

    /// The durable marker of the profile re-send a key change owes.
    private let profileResendOwed = ProfileResendOwed.Store()
    /// One re-send round at a time: the rotation's owed step, a kept device's
    /// adoption and the session-start retry all come through here.
    private var profileResendRunning = false

    /// ADR-0074 section 3.5a rule 3 — the account's keys moved on this
    /// device (it rotated them, or it adopted a rotation): the profile
    /// re-send is owed from now until every conversation took the envelope.
    public func markProfileResendOwed() {
        guard let accountIdB64 = currentAccountIdB64,
              let key = accountIdentity?.signaturePublicKey() else { return }
        profileResendOwed.put(
            ProfileResendOwed.marking(
                profileResendOwed.load(accountIdB64: accountIdB64), accountKeyB64: base64Encode(key)),
            accountIdB64: accountIdB64)
    }

    /// Whether a profile re-send is still owed on this device.
    public var profileResendIsOwed: Bool {
        currentAccountIdB64.flatMap { profileResendOwed.load(accountIdB64: $0) } != nil
    }

    /// ADR-0074 section 3.5a rule 3 — the account moved to the new mailbox,
    /// and the grants every peer holds open only the old one. Re-send the own
    /// profile, which carries this conversation's grants derived from the NEW
    /// seed, into every one-to-one and group. Throws while any send has not
    /// landed (or the chat layer is not up, or the reach keys are not on the
    /// new mailbox yet), so the caller keeps it owed; a conversation that was
    /// reached is recorded in the durable marker and is not sent to again by
    /// a retry, in this process or the next. Nothing owed returns at once.
    public func resendProfileAfterKeyRotation() async throws {
        guard state.bootstrapped, let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = profileResendOwed.load(accountIdB64: accountIdB64) else { return }
        guard !profileResendRunning else {
            throw ChatError.argument("profile re-send after key rotation: a round is already running")
        }
        profileResendRunning = true
        defer { profileResendRunning = false }
        // A grant handed out before its key is registered names a key the
        // mailbox does not know yet.
        guard await ReachCenter.shared.syncNow() else {
            throw ChatError.argument("profile re-send after key rotation: the reach keys are not registered yet")
        }
        let owed = ProfileResendOwed.targets(groups: Array(groups.keys), record: record)
        let failed = await profilePublisher.publishToAllReporting(groupIdHexes: owed)
        guard currentAccountIdB64 == accountIdB64 else { return }
        profileResendOwed.put(
            ProfileResendOwed.after(record, attempted: owed, failed: failed), accountIdB64: accountIdB64)
        guard failed.isEmpty else {
            throw ChatError.argument("profile re-send after key rotation: \(failed.count) conversation(s) still owed")
        }
        // The `fof` grant of the introduction records moved with the seed too.
        await sendIntroductions()
    }

    /// ADR-0074 section 3.5a rule 3 — a KEPT device adopted the rotation: it
    /// owes the re-send too. The account's devices all adopt within moments
    /// of each other, so this one waits a random moment first; what does not
    /// land stays in the durable marker and is retried at the next session
    /// start (``retryOwedProfileResend()``).
    public func resendProfileAfterAdoptingKeyRotation() async {
        markProfileResendOwed()
        try? await Task.sleep(
            nanoseconds: UInt64.random(in: 0...UInt64(ProfilePublisher.maxJitterMs)) * 1_000_000)
        await retryOwedProfileResend()
    }

    /// A backup restore owes the own profile to every conversation this
    /// device holds, as any change of the name does. Marked here, sent by the
    /// retry that runs after it (the session start, or the caller below).
    public func republishProfileAfterRestore() async {
        guard state.bootstrapped, let accountIdB64 = currentAccountIdB64,
              RestoredDeviceStore.takeProfileRepublish(accountIdB64: accountIdB64) else { return }
        markProfileResendOwed()
        await retryOwedProfileResend()
    }

    /// Retry an owed profile re-send (a session start). Best effort: what
    /// does not land stays owed.
    public func retryOwedProfileResend() async {
        guard profileResendIsOwed else { return }
        do {
            try await resendProfileAfterKeyRotation()
        } catch {
            NSLog("ADR-0074 3.5a: the profile re-send is still owed: \(type(of: error))")
        }
    }

    /// ADR-0048 §2 — republish the own profile (throttled, jittered) when the
    /// roster gained a key it did not have before.
    private func noteProfileGrowthIfMembershipGrew(
        groupIdHex: String,
        previousMemberKeys: [String]?,
        ownAdd: Bool = false
    ) {
        guard let record = groups[groupIdHex] else { return }
        let current = PushGroupRouting.memberSignatureKeys(group: record.group)
        guard membershipGrew(previousMemberKeys: previousMemberKeys, currentMemberKeys: current) else {
            return
        }
        profilePublisher.noteGrowth(groupIdHex: groupIdHex)
        // ADR-0051 §3 — the newcomer cannot read the epoch the group name was
        // sent in either; an owner / admin holding one re-sends it. The device
        // that committed the add sends at once: the newcomer's list has no name
        // until it does, and the schedule exists for the other members.
        if ownAdd {
            let publisher = groupNamePublisher
            let photo = groupPhotoPublisher
            let reactions = groupReactionsPublisher
            let handle = groupHandlePublisher
            Task {
                await publisher.publishNow(groupIdHex: groupIdHex)
                await photo.publishNow(groupIdHex: groupIdHex)
                await reactions.publishNow(groupIdHex: groupIdHex)
                await handle.publishNow(groupIdHex: groupIdHex)
            }
        } else {
            groupNamePublisher.noteGrowth(groupIdHex: groupIdHex)
            // ADR-0087 section 18.1 — the photo and the allowed reactions
            // travel the same way.
            groupPhotoPublisher.noteGrowth(groupIdHex: groupIdHex)
            groupReactionsPublisher.noteGrowth(groupIdHex: groupIdHex)
            groupHandlePublisher.noteGrowth(groupIdHex: groupIdHex)
        }
        // ADR-0052 decision 3 — nor the epochs the shared pins were sent in.
        pinPublisher.noteGrowth(groupIdHex: groupIdHex)
        // ADR-0059 section 6 — nor the channels this device shares there.
        sharedChannels.noteGrowth(groupIdHex: groupIdHex)
    }

    /// ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/`
    /// link capability (the wire bytes the slug base64url-decodes to). Decodes
    /// + verifies the capability fail-closed (before any post), then knocks
    /// (ADR-0053 point 6): a probe knock returns the group's registered epoch,
    /// an external-join proposal (ADR-0027) is built for it, and a second
    /// knock carries it with the join attribution. Returns the group id + the
    /// joiner's signature key — the caller waits with `pollForJoinWelcome`
    /// until an admin approves and the Welcome reaches this device's mailbox.
    public func requestToJoinFromLink(
        capabilityWire: Data,
        nowMs: UInt64,
        blobId: Data
    ) async throws -> (groupId: Data, joinerSignatureKey: Data) {
        guard let identity = canonicalIdentity, let joinerAccountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        let cap: GroupInviteCapability
        do {
            cap = try GroupInviteCapability.decodeAndVerify(wire: capabilityWire)
        } catch {
            throw ChatError.wrapped("group invite link decode/verify failed", underlying: error)
        }
        guard cap.isValidNow(nowMs: nowMs) else {
            throw ChatError.argument("group invite link has expired")
        }
        // `identity` is this binding's: the request is never continued with
        // another account's reply key, nor persisted into its snapshot.
        let bound = binding.capture()
        // ADR-0028 OQ1 — register one consume against the ledger's soft
        // `max_uses` ceiling before posting the request (anti-abuse
        // defence-in-depth; the admin approval gate remains the real
        // boundary). A repeat from this joiner is idempotent.
        if let accountIdB64 = currentAccountIdB64,
           let joinerAccountId = base64Decode(accountIdB64) {
            let outcome: ConsumeInviteResult
            do {
                outcome = try await discovery.consumeGroupInvite(
                    blobId: blobId,
                    capabilityBytes: capabilityWire,
                    joinerAccountId: joinerAccountId
                )
            } catch {
                throw ChatError.wrapped("group invite consume failed", underlying: error)
            }
            switch outcome {
            case .notFound:
                throw ChatError.argument("group invite not found or expired")
            case .exhausted:
                throw ChatError.argument("group invite has reached its use limit")
            case .consumed, .alreadyConsumed:
                break
            }
            try bound.require()
        }
        let groupId = cap.groupId()
        // ADR-0060 section 6 — the joiner's `reply` grant, registered before
        // the knock: the committer that admits it writes the Welcome to this
        // account's closed mailbox with it. Without it nobody could.
        let replyGrant: FfiReachGrant
        do {
            replyGrant = try await ReachCenter.shared.newReplyGrant(
                purpose: ReplyPurpose.groupJoin(groupId))
        } catch {
            throw ChatError.wrapped("reply key registration failed", underlying: error)
        }
        try bound.require()
        // ADR-0087 section 5 — the request is a fresh KeyPackage sealed to the
        // group's knock key (named by the link) with the reply grant, a
        // profile and the link's capability. It carries no epoch, so it waits
        // through every commit. One knock, one `ds-knock` token (ADR-0054).
        guard let knockKey = cap.knock() else {
            throw ChatError.argument("group invite link names no knock key")
        }
        let client = dsClient
        do {
            let keyPackage = try identity.createKeyPackage()
            // The KeyPackage's private key is the only thing that can open the
            // Welcome the sweep sends later: durable BEFORE the knock, fail
            // closed. A request whose key is not on disk is never sent.
            try bound.require()
            try persistJoinRequestOrThrow(identity: identity, accountIdB64: joinerAccountIdB64)
            let row = try sealKnockRow(
                knockPublicKey: knockKey.publicKey,
                version: knockKey.version,
                keyPackage: keyPackage,
                replyGrant: replyGrant,
                profile: ProfileDirectory.shared.ownDeviceEnvelope() ?? Data(),
                capability: capabilityWire)
            _ = try await client.knock(groupId: groupId, payloads: [row])
        } catch let error as DsKnockJoinError {
            throw error
        } catch {
            throw ChatError.wrapped("DS knock (request to join) failed", underlying: error)
        }
        try bound.require()
        // The key was made durable before the knock; this also writes the
        // message log beside it.
        persistSnapshotBestEffort()
        return (groupId, identity.signaturePublicKey())
    }

    /// Compose `(plaintext, attachments)` into the canonical
    /// 1.19a JSON envelope, encrypt for the current epoch of
    /// `groupIdHex`, and post the wire bytes to the DS. Appends
    /// the message to the in-memory timeline + persists the
    /// snapshot.
    ///
    /// Phase 1.19c3: empty `attachments` ⇒ text-only envelope
    /// (binary-identical to a `composeEnvelope(text: …,
    /// attachments: [])` call, which the 1.19a parser also
    /// accepts as a v=1 message). Empty `plaintext` is valid
    /// when at least one attachment is present (attachment-only
    /// message). Empty-and-empty is rejected at the call site
    /// upstream — the view model and screen layer enforce that.
    /// G3-1-call-key — derive the per-call SFrame seed + the
    /// local leaf index for a call inside `groupIdHex`.
    ///
    /// The seed is the MLS exporter output
    /// `export_secret(label = "lakko/call/v1", context = call_id,
    /// length = 32)` per audit-pack invariant C-1 — a
    /// deterministic function of the group's current epoch
    /// secret, so both the offerer and the answerer (same group,
    /// same `callId`, same epoch) derive **identical** seed
    /// bytes; the per-sender SFrame key then forks on each
    /// member's leaf index. Returns `nil` (call must refuse per
    /// security invariant #4 — never fall back to non-E2EE) when
    /// the repository is not bootstrapped, the group handle is
    /// missing, or the exporter call fails (e.g. epoch skew).
    ///
    /// The derivation lives here, in the layer that owns the
    /// MLS `Group` + `Identity`, rather than in the view model.
    public func deriveCallKey(
        groupIdHex: String,
        callId: Data
    ) -> (seed: Data, localLeaf: UInt32)? {
        guard let identity = canonicalIdentity else {
            NSLog("deriveCallKey: ChatRepository not bootstrapped")
            return nil
        }
        guard let record = groups[groupIdHex] else {
            NSLog("deriveCallKey: no group handle")
            return nil
        }
        do {
            let seed = try record.group.exportSecret(
                identity: identity,
                label: "lakko/call/v1",
                context: callId,
                length: 32
            )
            let leaf = try record.group.ownLeafIndex()
            return (seed, leaf)
        } catch {
            NSLog("deriveCallKey: exporter failed: %@", String(describing: error))
            return nil
        }
    }

    /// The peer announcer of a group call in `groupIdHex` (ADR-0078 section
    /// 2): this device signs who it is with its leaf key, and every other
    /// peer's announcement is verified against the group's roster as it is
    /// when the announcement arrives. `nil` before bootstrap.
    func callPeerAnnouncer(groupIdHex: String, callId: Data) -> CallPeerAnnouncer? {
        guard let identity = canonicalIdentity, callId.count == 16 else { return nil }
        return CallPeerAnnouncer.forGroup(
            identity: identity,
            callId: callId,
            group: { [weak self] in self?.groups[groupIdHex]?.group }
        )
    }

    /// Number of members in the live MLS group for `groupIdHex`, or
    /// `nil` if there is no handle. Used by the call surface to route a
    /// >2-member chat through the SFU group-call path and a 2-member
    /// chat through the 1×1 P2P path.
    public func memberCount(groupIdHex: String) -> Int? {
        guard let record = groups[groupIdHex] else { return nil }
        // ADR-0050 — PEOPLE, not leaves: a 1x1 between two people with three
        // devices each is still a 1x1.
        guard let roster = try? record.group.members() else { return nil }
        return PersonKey.people(roster).count
    }

    /// Current MLS epoch of the live group for `groupIdHex`, or `nil` when there
    /// is no handle. Exposed so the ADR-0040 (G-35) F1 reset can be asserted —
    /// after a commit persist-abort the in-memory group must read back at the
    /// pre-commit epoch N (not the eager-merged N+1).
    public func epoch(groupIdHex: String) -> UInt64? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.epoch()
    }

    /// `true` while this repository holds a live handle for `groupIdHex` — the
    /// chat is still in the registry. The group call host ends a call whose
    /// chat is gone rather than keep media flowing under a key nobody rotates.
    func hasGroup(groupIdHex: String) -> Bool {
        groups[groupIdHex] != nil
    }

    /// ADR-0045 UX §6.2 — is this device still a member of `groupIdHex`?
    ///
    /// `nil` when there is no handle or the handle could not be read — which
    /// ``membershipState(isActive:leaveRequestedAtMs:)`` reads as still a
    /// member, deliberately: an unreadable group is not evidence of an
    /// eviction, and this is a claim about FACT rather than about
    /// cryptographic capability.
    ///
    /// This is the ONE signal there is. `processIncomingVerifyingRoles` returns
    /// the same content-less `Commit` for a commit that removes our own leaf as
    /// for any other, so nothing in the receive path announces it; and the
    /// available inference (*no member record whose signature key is ours*)
    /// rests on a leaf's signature key being the account's, which ADR-0022
    /// names as an open follow-up. `Group.isActive()` is what OpenMLS itself
    /// consults before refusing to encrypt, export or decrypt.
    ///
    /// **Never reason about this from the epoch.** After a self-removing commit
    /// merges, `epoch()` reports N+1 over epoch-N secrets the merge's
    /// `PublicState` branch never replaced.
    public func isActive(groupIdHex: String) -> Bool? {
        guard let record = groups[groupIdHex] else { return nil }
        return readIsActive { try record.group.isActive() }
    }

    /// ADR-0045 UX §6.2.3 — when this device published a self-targeted leave
    /// request for `groupIdHex`, or `nil` when it never did (or lost the
    /// record). `nil` narrows to nothing: the copy falls back to the removed
    /// form, which is true either way.
    /// ADR-0045 UX §6.2 — the membership verdict for `groupIdHex`, read off the
    /// LIVE handle right now.
    ///
    /// The repository's own gates use this rather than anything the UI holds:
    /// a `@Published` mirror is refreshed by an observer, so it can still carry
    /// its pre-batch value in the turn that immediately follows a poll — which
    /// is exactly where the rotation fires.
    func membershipStateNow(groupIdHex: String) -> MembershipState {
        // ADR-0087 section 9 — an owed end stops everything that consults
        // this, whether or not the commit was applied here.
        if isEnding(groupIdHex: groupIdHex) {
            return .ended(left: leaveRequestedAt(groupIdHex: groupIdHex) != nil)
        }
        return membershipState(
            isActive: isActive(groupIdHex: groupIdHex),
            leaveRequestedAtMs: leaveRequestedAt(groupIdHex: groupIdHex))
    }

    public func leaveRequestedAt(groupIdHex: String) -> Int64? {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return nil }
        return leaveRequestStore.load(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId))
    }

    /// ADR-0045 UX §6.2.4 — `Delete this chat`.
    ///
    /// Reachable only once the membership has ended (the exit block's `ended`
    /// arm), so this never removes a live conversation — and it re-checks that
    /// here rather than trusting the control it was drawn behind, the same
    /// layered-defence posture `leaveGroupAffordance` and `exitBlock` already
    /// keep between them.
    ///
    /// Since ADR-0087 section 9 a person who is out of a group has no such
    /// control: the group goes by itself (``ChatRepository+MembershipEnd``).
    /// What still reaches this is a one-to-one whose leaf was retired and a
    /// group this device's leaf was retired from while the account stays.
    ///
    /// The REGISTRY rows go LAST, so a failure above leaves the chat reachable
    /// rather than orphaning a transcript behind a row the user can no longer
    /// open.
    @discardableResult
    public func deleteChat(groupIdHex: String) -> Bool {
        // ADR-0073 R.5 — what is deleted is the CONVERSATION: every segment
        // of it this device holds, the current group last, so a failure part
        // way leaves the row reachable rather than orphaning a transcript
        // behind it. The gate is the current group's: an older segment's own
        // membership says nothing about whether the conversation is over.
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return false }
        let headHex = headGroupIdHex(of: groupIdHex)
        guard membershipEnded(membershipStateNow(groupIdHex: headHex)) else { return false }
        let plan = conversationPlan(ofGroup: groupIdHex)
        let segments = (plan?.heldSegments ?? [groupIdHex]).map {
            LineageViews.ConversationSegment(groupIdB64: $0, groupIdHex: $0)
        }
        var deleted = false
        for segment in LineageViews.conversationDeleteOrder(segments, headGroupIdB64: headHex) {
            deleted = removeGroupLocally(groupIdHex: segment.groupIdHex) || deleted
        }
        if let conversation = plan?.conversationHex { forgetHeld(conversationHex: conversation) }
        state = state.with(threads: buildThreadList())
        return deleted
    }

    /// How a group's MLS state goes (``removeGroupNow(groupIdHex:mls:)``).
    enum MlsGroupDelete {
        /// The commit that ended it was applied here. An active group is
        /// refused: an older segment of a deleted chat stays inert.
        case applied
        /// ADR-0087 section 9 — this device heard of the end from its own
        /// account's register at this epoch.
        case endedElsewhere(epoch: UInt64)
    }

    /// Remove ONE group and everything this device keeps for it. No gate:
    /// the caller decided (``deleteChat(groupIdHex:)``). A provider that
    /// refuses the delete leaves the row dropped all the same: the chat was
    /// deleted, and the leftover state is inert.
    @discardableResult
    func removeGroupLocally(groupIdHex: String) -> Bool {
        guard groups[groupIdHex] != nil else { return false }
        do {
            try removeGroupNow(groupIdHex: groupIdHex, mls: .applied)
        } catch {
            NSLog("chat delete: the MLS group stays in the provider: \(error)")
            dropRegistryRows(groupIdHex: groupIdHex)
        }
        return true
    }

    /// ADR-0087 section 9, step 4: wipe, then the MLS group, then the provider
    /// snapshot, durably, and the registry rows LAST. Synchronous, so nothing
    /// interleaves. Throws with the registry rows still there when the group
    /// could not be deleted or persisted; the caller's owed record resumes it,
    /// and every step here runs again harmlessly.
    func removeGroupNow(groupIdHex: String, mls: MlsGroupDelete) throws {
        guard let accountIdB64 = currentAccountIdB64 else { throw StaleBindingError() }
        wipeGroupStores(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        if let record = groups[groupIdHex], let identity = canonicalIdentity {
            switch mls {
            case .applied:
                do {
                    try record.group.deleteLocally(identity: identity)
                } catch FfiError.MembershipGroupStillActive {
                    // An older segment of a conversation, still live.
                }
            case .endedElsewhere(let epoch):
                try record.group.deleteAfterEndedElsewhere(identity: identity, endedEpoch: epoch)
            }
            try persistSnapshotDurableOrThrow()
        }
        dropRegistryRows(groupIdHex: groupIdHex)
    }

    /// LAST, and in this order: the registry is what a relaunch rehydrates
    /// from, so dropping it after the live handle means a crash between the
    /// two leaves the chat present rather than half-gone.
    private func dropRegistryRows(groupIdHex: String) {
        groups.removeValue(forKey: groupIdHex)
        unreadMessageIds.removeValue(forKey: groupIdHex)
        // Nothing can poll this journal once the registry row is gone, so any
        // own-echo registration left for it is unconsumable. The poll gate
        // above has usually dropped them already; this covers a delete that
        // never polled after the eviction.
        chatClient.forgetOwnPublished(groupIdHex: groupIdHex)
        if let accountIdB64 = currentAccountIdB64 {
            knownGroupsStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
        // A deleted chat is not carried in this device's backups either.
        dropRestoredKnownGroup(groupIdHex: groupIdHex)
        state = state.with(threads: buildThreadList())
    }

    /// Everything this device keeps for one group, except the MLS group and
    /// the registry rows. Best-effort by construction, and repeatable: an
    /// owed end runs it again after a crash, and a Welcome that replaced an
    /// ended group runs it with the new group not yet registered.
    func wipeGroupStores(accountIdB64: String, groupIdHex: String) {
        let groupIdB64 = groups[groupIdHex].map { base64Encode($0.groupId) }
            ?? Self.hexToBytes(groupIdHex).map(base64Encode)
        // The transcript first: it is the only thing here that is irreplaceable,
        // so if anything is going to fail it should fail before the row that
        // makes the chat reachable is gone.
        groups[groupIdHex]?.forgetHistory()
        _ = messageLogStore.deleteGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        _ = waitingLogSlots.take(groupIdHex)
        if let groupIdB64 {
            // ADR-0073 R.6 — the claim state of a group that is gone from here.
            groupClosedStore.drop(groupIdB64: groupIdB64)
            pendingRequestStore.prune(
                accountIdB64: accountIdB64, groupIdB64: groupIdB64, liveKeys: [])
            unappliedRequestStore.clearForGroup(
                accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            pendingRequestStore.clearIntents(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            leaveRequestStore.clear(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            lastObservedOwnerStore.clear(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
            beaconStore.prune(accountIdB64: accountIdB64, groupIdB64: groupIdB64, liveKeys: [])
            // ADR-0040 — a commit of a group this device is out of has nowhere
            // to go; a staged sweep went with the MLS group.
            for entry in commitOutbox.loadUnacked(accountIdB64: accountIdB64, groupIdB64: groupIdB64) {
                commitOutbox.deleteEntryBestEffort(accountIdB64: accountIdB64, id: entry.id)
            }
        }
        readWatermarkStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        readWatermarksByGroup.removeValue(forKey: groupIdHex)
        // ADR-0052 decision 3 — the pins are wiped with the conversation, both
        // scopes: a bookmark of a chat that no longer exists here points at
        // nothing.
        sharedPinStore.removeGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        personalPinStore.removeGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        groupArrivalStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // ADR-0051 — the name; the notice holds its own copy.
        groupNameStore.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        groupNameRevision &+= 1
        // ADR-0087 section 9 step 4 — the photo, the reactions setting, the
        // @name and the knock seeds go with it.
        groupSettingsStore.removeGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        groupSettingsRevision &+= 1
        unseenReactionsStore.removeGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        unseenReactionsRevision &+= 1
        // ADR-0059 section 6 — the lists members shared there and the switch.
        sharedChannels.forgetGroup(groupIdHex: groupIdHex)
        if groupCalls.removeValue(forKey: groupIdHex) != nil {
            groupCallStore.save(accountIdB64: accountIdB64, records: groupCalls)
        }
        CallHistoryStore.shared.bind(accountIdB64: accountIdB64)
        CallHistoryStore.shared.forget(conversationId: groupIdHex)
        // ADR-0057 — this device's standing in the journal: a group it comes
        // back to appoints it again.
        registrarSeeds.forgetGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // ADR-0060 section 3 — the grants members gave here.
        ReachCenter.shared.forgetHeld(scopeHex: groupIdHex)
        // ADR-0050 — which leaves left this group: a person added back is not
        // a device someone removed.
        DeviceReadmissionMemory.forgetGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        ProfileRequestStore().forgetGroup(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        StateBundle.forgetKnownGroup(groupIdHex: groupIdHex, accountIdB64: accountIdB64)
        // Decrypted media is cached by blob, not by group: all of it goes, and
        // the rest is fetched again.
        MediaSessionCache.shared.purge()
        // ADR-0053 — the knock cursor.
        UserDefaults.standard.removeObject(
            forKey: "\(AccountLocalDataWipe.knockCursorPrefix)\(accountIdB64).\(groupIdHex)")
        forgetGroupSessionState(groupIdHex: groupIdHex)
    }

    /// What this session keeps per group: a group this device comes back to
    /// starts over, signers and claims included.
    private func forgetGroupSessionState(groupIdHex: String) {
        sweptGroupsThisSession.remove(groupIdHex)
        claimedGroupsThisSession.remove(groupIdHex)
        claimOnOpen.forget(journalIdHex: groupIdHex)
        dsSigners.forget(journal: groupIdHex)
        lastBeaconAtMs.removeValue(forKey: groupIdHex)
        sweepRegistrationBackoff.removeValue(forKey: groupIdHex)
        supersededTrigger.forget(groupIdHex: groupIdHex)
        registrarSetsSent.removeValue(forKey: groupIdHex)
        rootHandoverNotBefore.removeValue(forKey: groupIdHex)
        rootHandoverLastSentAt.removeValue(forKey: groupIdHex)
        rootTakeoverNotBefore.removeValue(forKey: groupIdHex)
        rootProbeNotBefore.removeValue(forKey: groupIdHex)
        claimIdentityCheckedAtMs.removeValue(forKey: groupIdHex)
    }

    /// ADR-0045 UX §6.2.3 — record that this device asked to leave.
    ///
    /// Called from the leave action at PUBLISH time, whatever the outcome
    /// (since the ADR-0081 amendment of 2026-10-19 a leave is always a
    /// proposal; a group with no role table is refused). First-wins.
    public func noteLeaveRequested(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        leaveRequestStore.remember(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            atMs: Self.nowMs())
        // UX 9.4 — the row now says it is waiting.
        state = state.with(threads: buildThreadList())
    }

    /// Roster of the live MLS group for `groupIdHex` — `(leafIndex,
    /// signaturePublicKey)` per member at the current epoch — or `nil` when
    /// there is no handle. The group-chat UI matches each `signaturePublicKey`
    /// against the accepted-contacts list to render a name, else a fingerprint.
    public func members(groupIdHex: String) -> [GroupMember]? {
        guard let record = groups[groupIdHex] else { return nil }
        // ADR-0050 — one row per PERSON: `signaturePublicKey` is the member's
        // account key (what roles, names and authorship key on), `leafIndex`
        // this device's own leaf for its own person, else the person's first
        // leaf. Removing the row removes every leaf of the person.
        guard let roster = try? record.group.members() else { return nil }
        return PersonKey.collapse(roster, ownLeafIndex: try? record.group.ownLeafIndex())
    }

    /// ADR-0048 — the other member's signature key in a one-to-one, or `nil`
    /// for a group (by its ADR-0062 kind, even with two people), a chat with
    /// nobody else yet, or a torn roster read. What the chat list, a 1x1
    /// header and a call ring name the conversation by.
    public func oneToOnePeerSignatureKey(groupIdHex: String) -> Data? {
        // ADR-0062 — a two-person GROUP is not a one-to-one: its title, its
        // header and "the chat with this person" never name one member.
        guard isOneToOne(groupIdHex: groupIdHex) else { return nil }
        guard let record = groups[groupIdHex],
              let leaves = try? record.group.members(),
              let own = try? record.group.ownLeafIndex()
        else { return nil }
        // ADR-0050 — two PEOPLE, whatever their device count.
        let roster = PersonKey.collapse(leaves, ownLeafIndex: own)
        guard roster.count == 2 else { return nil }
        return roster.first { $0.leafIndex != own }?.signaturePublicKey
    }

    /// THE one-to-one conversation with a contact on this device, or `nil`
    /// when it holds none (``ContactConversation`` is the rule): the one
    /// linked on the contact record while this device still holds it and it
    /// is a one-to-one with that contact, else
    /// the conversation of one-to-one kind whose two PEOPLE are this account
    /// and the contact. Never a group that happens to hold the same two
    /// people. A read over the live rosters, no state change.
    public func oneToOneConversation(contactKey: Data, linkedGroupIdHex: String?) -> String? {
        ContactConversation.find(
            contactKey: contactKey,
            linkedId: linkedGroupIdHex,
            // ADR-0073 R.5 — one entry per conversation, the group it
            // currently is: an older segment is history, never an address.
            conversations: conversationHeadIdHexes,
            idOf: { $0 },
            ownKey: conversationAccountIdentity?.signaturePublicKey(),
            peopleOf: { (members(groupIdHex: $0) ?? []).map(\.signaturePublicKey) },
            isOneToOne: { isOneToOne(groupIdHex: $0) },
            // ADR-0074 section 3.7 — a proven rotation names one person by
            // two keys; the contact row may hold either.
            samePerson: { first, second in
                first == second
                    || ReachCenter.shared.currentAccountKey(of: first)
                        == ReachCenter.shared.currentAccountKey(of: second)
            }
        )
    }

    /// Our own leaf index in `groupIdHex`, or `nil` when there is no handle.
    /// The roster UI uses it to mark the local member "(you)" and to refuse
    /// removing ourselves.
    public func ownLeafIndex(groupIdHex: String) -> UInt32? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.ownLeafIndex()
    }

    /// ADR-0023 — this device's own IDENTITY in `groupIdHex`: its Ed25519
    /// signature key, base64, resolved THROUGH the roster rather than stored,
    /// so a leaf renumber cannot make it name someone else.
    ///
    /// This is what every authorship decision on our own side reads — the
    /// own-reaction highlight and the "read by every other member" exclusion.
    /// ``ownLeafIndex(groupIdHex:)`` above stays for what a leaf is genuinely
    /// for: a POSITION the MLS / SFU surfaces take and the roster marks "(you)"
    /// with. Nothing that attributes authorship may compare it.
    public func ownSignatureKeyB64(groupIdHex: String) -> MemberIdentity? {
        guard let record = groups[groupIdHex] else { return nil }
        return AuthorIdentity.own(group: record.group)
    }

    /// Our own ADR-0022 role in `groupIdHex` (`.owner` / `.admin` / `.member`),
    /// or `nil` for an open / legacy group with no role table (or no handle).
    /// The members UI gates the add / remove affordances on this.
    public func ownRole(groupIdHex: String) -> Role? {
        guard let identity = canonicalIdentity else { return nil }
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.ownRole(identity: identity)
    }

    /// The group's ADR-0022 role table (`owner` + `admins` signature keys), or
    /// `nil` for an open / legacy group. The members UI uses it to label the
    /// per-member promote / demote toggle.
    public func groupRoles(groupIdHex: String) -> RoleTable? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.groupRoles()
    }

    /// Promote / demote a member as admin (owner only — enforced on the
    /// receiving side). Reads the role table, adds or removes the member's
    /// signature key from the admin set, and publishes the `setGroupRoles`
    /// commit (own-marked for echo suppression). Returns the commit's DS id.
    public func setMemberAdmin(
        groupIdHex: String,
        memberSignaturePubkey: Data,
        makeAdmin: Bool
    ) async throws -> MembershipOutcome {
        // Serialized against every other membership operation on this group.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            // ADR-0045 — a role change is a PROPOSAL; a group with no role
            // table is refused (ADR-0081 amendment of 2026-10-19).
            // UX §6, seventh row — the ONE string, defined once in
            // `OwnershipTransfer.swift` and used by both C-65 pre-checks. This
            // used to throw its own near-variant prose, which is precisely the
            // drift class §12.6 exists to prevent.
            if hasPendingRoleChange(groupIdHex: groupIdHex) {
                throw ChatError.argument(roleChangeAlreadyPendingMessage)
            }
            let outcome = try await proposeMemberAdminLocked(
                groupIdHex: groupIdHex,
                memberSignaturePubkey: memberSignaturePubkey,
                makeAdmin: makeAdmin
            )
            // ADR-0050 2.5a — the DELTA, never the table.
            recordMembershipIntent(
                groupIdHex: groupIdHex,
                .setAdmin(personKeyB64: base64Encode(memberSignaturePubkey), makeAdmin: makeAdmin))
            return outcome
        }
    }

    /// ADR-0045 — publish a role-table rewrite as a **proposal**.
    ///
    /// The epoch does not advance and there is nothing to roll back, so unlike
    /// ``setMemberAdminLocked(groupIdHex:memberSignaturePubkey:makeAdmin:)``
    /// there is no ADR-0040 outbox entry and no reset guard — that is the whole
    /// point of the option: a proposal that loses a race costs nothing, where a
    /// commit that loses one forks its author off the group.
    ///
    /// The proposal is retained in this device's own pending set as well as
    /// published, and that is not redundant even when this device will sweep it
    /// a moment later: OpenMLS resolves a commit's by-reference proposals
    /// through the proposal store on **every receiving member**, so a peer that
    /// never saw the proposal refuses the commit carrying it with
    /// `InvalidCommit(MissingProposal)`.
    private func proposeMemberAdminLocked(
        groupIdHex: String,
        memberSignaturePubkey: Data,
        makeAdmin: Bool
    ) async throws -> MembershipOutcome {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        guard let table = try? record.group.groupRoles() else {
            throw ChatError.argument("group has no role table")
        }
        // ADR-0050 2.5a — over the CURRENT table, so a re-request of a
        // superseded toggle re-applies only its own delta.
        let admins = adminsAfterToggle(
            current: table.admins, subject: memberSignaturePubkey, makeAdmin: makeAdmin)
        let proposalBytes: Data
        do {
            proposalBytes = try record.group.proposeGroupRoles(
                identity: identity,
                owner: table.owner,
                admins: admins
            )
        } catch {
            throw ChatError.wrapped("proposeGroupRoles failed", underlying: error)
        }
        return try await publishProposal(
            record: record, groupIdHex: groupIdHex, proposalBytes: proposalBytes)
    }

    /// ADR-0045 UX §6.1 — hand the group's owner slot to another member.
    ///
    /// **Always the proposal arm, and there is no second one here.** The UniFFI
    /// surface carries `transferOwnership` too, and this method deliberately
    /// never reaches it, because for this flow the direct arm is *structurally
    /// unreachable*: the UI gates the control on `isOwner`, which is read off
    /// the role table (``ownRole(groupIdHex:)``), and
    /// ``usesProposalPath(groupIdHex:)`` is the identical `groupRoles()` read. A
    /// group with no role table has no owner, so `isOwner && !usesProposalPath`
    /// cannot hold. That makes transfer the one membership action needing a
    /// single arm rather than two, unlike add / remove / role change, each of
    /// which carries a direct fallback for the table-less case. A table-less
    /// group reaching here anyway is refused by the core, which is the right
    /// place for it.
    ///
    /// Everything else mirrors ``setMemberAdmin(groupIdHex:memberSignaturePubkey:makeAdmin:)``
    /// exactly, because a transfer **is** a role change — deliberately not a new
    /// commit type, just the owner-only role-table rewrite with the owner slot
    /// moved. So it inherits that path's discipline verbatim: the per-group
    /// commit lock, the C-65 pre-check, no ADR-0040 outbox entry and no reset
    /// region (a proposal advances no epoch, so there is nothing to roll back),
    /// and a `.proposed` outcome the caller sweeps immediately.
    ///
    /// `newOwnerSignaturePubkey` is the successor's 32-byte Ed25519 signature
    /// public key — the stable handle `Group.members()` reports and the role
    /// table keys on — never a leaf index: OpenMLS hands a vacated leaf to the
    /// next Add.
    ///
    /// `outgoing` decides what becomes of THIS device's own role and is
    /// **required**. The core refuses to default it because either silent choice
    /// is wrong in one direction, and UniFFI has no `Default` derive, so the
    /// generated signature is mandatory; this one keeps the property rather than
    /// handing back a defaulted argument the core deliberately withheld.
    public func transferGroupOwnership(
        groupIdHex: String,
        newOwnerSignaturePubkey: Data,
        outgoing: OutgoingOwner
    ) async throws -> MembershipOutcome {
        // ADR-0040 (G-35) — serialize against every other membership operation
        // on this group, exactly as the sibling role-change path does.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            // C-65, and it collides in BOTH directions: a pending role change
            // refuses a transfer, and a pending transfer refuses a role change.
            // It falls out of `propose_ownership_transfer` delegating to
            // `propose_group_roles` rather than being restated, which is the
            // point. The UI disables the control with this same string as its
            // reason once the blocking request is past the quiet window (UX
            // §6.1.8's deliberate asymmetry with the admin toggles, which keep
            // refuse-on-tap); this is the check for the window in which the
            // control is still live.
            if hasPendingRoleChange(groupIdHex: groupIdHex) {
                throw ChatError.argument(roleChangeAlreadyPendingMessage)
            }
            let outcome = try await proposeOwnershipTransferLocked(
                groupIdHex: groupIdHex,
                newOwnerSignaturePubkey: newOwnerSignaturePubkey,
                outgoing: outgoing)
            recordMembershipIntent(
                groupIdHex: groupIdHex,
                .transferOwnership(
                    toPersonKeyB64: base64Encode(newOwnerSignaturePubkey), outgoing: outgoing))
            return outcome
        }
    }

    /// The serialized body of
    /// ``transferGroupOwnership(groupIdHex:newOwnerSignaturePubkey:outgoing:)``.
    /// Runs only under ``withCommitLock(groupIdHex:_:)``; never call it directly.
    private func proposeOwnershipTransferLocked(
        groupIdHex: String,
        newOwnerSignaturePubkey: Data,
        outgoing: OutgoingOwner
    ) async throws -> MembershipOutcome {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let proposalBytes: Data
        do {
            proposalBytes = try record.group.proposeOwnershipTransfer(
                identity: identity,
                newOwner: newOwnerSignaturePubkey,
                outgoing: outgoing
            )
        } catch {
            throw ChatError.wrapped("proposeOwnershipTransfer failed", underlying: error)
        }
        return try await publishProposal(
            record: record,
            groupIdHex: groupIdHex,
            proposalBytes: proposalBytes,
            // UX §6.1.10 — the successor's key on OUR OWN observation record is
            // the only thing that later tells this transfer from an ordinary
            // promote, and it is what names the person in
            // `Waiting to make {name} the owner.`
            transferToB64: base64Encode(newOwnerSignaturePubkey))
    }

    /// ADR-0045 UX §6.1.9 — the owner signature key (base64) this device last
    /// observed for `groupIdHex`, or `nil` when it never observed one. `nil` is
    /// the fail-open case: a first observation is not a change.
    public func lastObservedOwner(groupIdHex: String) -> String? {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return nil }
        return lastObservedOwnerStore.load(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId))
    }

    /// ADR-0045 UX §6.1.9 — record the owner this device is observing now. The
    /// caller must decide the notice FIRST, or the comparison would always be
    /// against itself.
    public func rememberObservedOwner(groupIdHex: String, ownerKeyB64: String) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        lastObservedOwnerStore.remember(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            ownerKeyB64: ownerKeyB64)
    }

    // ADR-0045 — the direct `setGroupRoles` commit body that used to live here
    // is DELETED, not kept dormant. It guarded on the role table, so it could
    // never have served the one case ADR-0045 leaves on the direct path (a
    // group with NO role table); with role changes routed through proposals it
    // therefore had no reachable caller at all. The web reference keeps its
    // equivalent arm and says so — `contacts/client.ts` calls it "unreachable in
    // practice … kept only because the local role read is what proves it" — but
    // an unreachable branch is not proof of anything, and `git log` is where a
    // deleted body belongs.

    /// Add an already-resolved recipient to an EXISTING group (mirror of
    /// ``startGroupWith`` but for a live group). The caller resolves +
    /// G-4-verifies `@username -> (accountId, signaturePubkey)` first (the
    /// pin-store verify lives in `ContactsClient`).
    ///
    /// Unlike the 1×1 create, the resulting commit MUST be published to the
    /// group journal so existing members advance their epoch, and marked as
    /// own so the poll loop suppresses the echo — re-applying our own commit
    /// would fork the MLS epoch. Returns the commit's DS message id.
    ///
    /// `openMailbox` is the verified mailbox of a person found in Everyone
    /// (ADR-0060 section 6, "An add of a person found by search"): recorded
    /// for this device's own sweep before the proposal, and named in a
    /// committer-delegation envelope with no delegation after it, unless a
    /// grant held for the person signs real delegations.
    public func addMemberToGroup(
        groupIdHex: String,
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        openMailbox: MailboxTarget? = nil
    ) async throws -> MembershipOutcome {
        // Fast-fail pre-checks, kept OUTSIDE the lock so an obviously-invalid
        // call reports the same error it always did without queueing. They are
        // re-run inside the locked section against fresh state — these two are
        // an early exit, not the authority.
        guard canonicalIdentity != nil else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // The KeyPackage fetch touches NO MLS state, so it is deliberately
        // taken before the lock: it is a third-party network round-trip, and
        // holding a group's commit chain across it would stall unrelated
        // membership work on that group for its whole duration.
        // ADR-0050 — every device of the person.
        let bound = binding.capture()
        // ADR-0073 R.2 / ADR-0074 3.5 — the identity gate for the account
        // first (it is what records a proven rotation, and this is also the
        // path the ADR-0050 re-request of an older add rides). The directory
        // is then not trusted to say whose devices it returned: only those
        // bound to the expected key or its proven chain, never a
        // reset-retired key, are added. None left: the same answer as an
        // empty directory, nothing merged or published.
        let devices = try await expectedPersonDevices(
            accountId: recipientAccountId, expectedKey: recipientSignaturePubkey)
        guard !devices.isEmpty else {
            throw ChatError.argument("recipient has no KeyPackage uploaded yet")
        }
        // The account that asked for this add left while the directory was
        // read: nothing is noted for, or proposed by, whoever is here now.
        try bound.require()
        // ADR-0060 — remember whose leaves these are, so the Welcome of the
        // commit that adds them goes to the person's account mailbox.
        noteAddedLeafAccounts(keyPackages: devices.map(\.keyPackage))
        // ADR-0060 section 6 — a person found in Everyone: this device's own
        // sweep may write their Welcome to the open mailbox. Recorded before
        // the proposal, so a sweep that runs right after it finds it.
        let reach = committerEnvelopeReach()
        let open = Self.openMailboxFor(
            openMailbox, addedAccountKey: recipientSignaturePubkey,
            current: { reach.currentAccountKey(of: $0) })
        if let open {
            reach.recordOpenAdd(groupIdHex: groupIdHex, target: open)
        }

        // ADR-0050 — one proposal / one commit per device of the person. The
        // outcome reported is the last device's; every earlier one either
        // landed or threw.
        var outcome: MembershipOutcome?
        for device in devices {
            outcome = try await addOneDeviceToGroup(
                groupIdHex: groupIdHex, keyPackageBytes: device.keyPackage)
        }
        guard let outcome else {
            throw ChatError.argument("recipient has no KeyPackage uploaded yet")
        }
        try bound.require()
        // ADR-0050 2.5a — remember what was asked, so a commit that drops the
        // proposal is answered with a fresh one.
        if case .proposed = outcome {
            recordMembershipIntent(
                groupIdHex: groupIdHex,
                .add(
                    personKeyB64: base64Encode(recipientSignaturePubkey),
                    accountIdB64: base64Encode(recipientAccountId),
                    leafKeysB64: devices.compactMap {
                        (try? keyPackageSignatureKey(keyPackage: $0.keyPackage)).map(base64Encode)
                    }))
        }
        // ADR-0060 section 6 — under ADR-0045 another device may commit this
        // Add, and the added person's mailbox is closed to it unless it holds
        // a grant. Hand each committer candidate a delegation signed with this
        // device's grant for the person, in the group beside the proposal.
        if case .proposed = outcome {
            await sendCommitterDelegations(
                groupIdHex: groupIdHex, addedAccountKey: recipientSignaturePubkey, openMailbox: open)
        }
        return outcome
    }

    /// The Everyone mailbox an add may name for the person `addedAccountKey`:
    /// `candidate` when its binding verified under that person's key (the
    /// same person under `current`), else `nil`.
    nonisolated static func openMailboxFor(
        _ candidate: MailboxTarget?, addedAccountKey: Data, current: (Data) -> Data
    ) -> MailboxTarget? {
        guard let candidate, current(candidate.accountKey) == current(addedAccountKey) else { return nil }
        return candidate
    }

    /// The proven-predecessor map the add-member key check reads. Injectable
    /// for tests.
    var provenPredecessorsProvider: () -> ProvenPredecessors = { ProvenPredecessorStore().load() }

    /// ADR-0073 R.2 / ADR-0074 section 3.5 — the identity gate an account is
    /// put through before its devices are fetched to be added. Injectable for
    /// tests; production runs the fail-closed check of
    /// ``ContactsClient/checkContactIdentity(accountId:heldKeyB64:)`` with no
    /// roster key, as a contact request does: a pin decides, else a verified
    /// binding stands in for it, else the first observation is pinned.
    var addIdentityGate: (@MainActor (_ accountId: Data) async throws -> Void)?

    /// THE way an account's devices are fetched to be added to a group: the
    /// user-facing add, a successor build, the re-admission arm and the
    /// ADR-0050 membership re-request all end here, so no add path can
    /// forget either step.
    ///
    /// 1. The identity gate for the account runs FIRST, unless the caller
    ///    just ran it (`gateAlreadyRan`). After an ADR-0074 rotation the
    ///    account's kept devices are still bound to the OLD key while the
    ///    directory serves the new one; the gate is what records the proven
    ///    rotation (old key -> new key), and for a person who is not a
    ///    contact it is what turns the verified binding this device held
    ///    into a pin with its proven chain. A change that is neither proven
    ///    nor an attested reset throws here, and nothing is fetched.
    /// 2. The directory's list is filtered to the devices bound to the
    ///    expected key OR any key of its proven chain
    ///    (`AddMemberKey.acceptableBoundKeys`). A key an accepted RESET
    ///    retired is in no proven chain and is never accepted.
    ///
    /// Returns the devices to add; empty when the directory lists none that
    /// are this person's.
    func expectedPersonDevices(
        accountId: Data, expectedKey: Data, gateAlreadyRan: Bool = false
    ) async throws -> [DeviceKeyPackage] {
        if !gateAlreadyRan {
            if let addIdentityGate {
                try await addIdentityGate(accountId)
            } else {
                _ = try await identityGateClient().checkContactIdentity(accountId: accountId)
            }
        }
        let listed = try await discovery.fetchKeyPackages(accountId: accountId) ?? []
        return devicesOfExpectedPerson(listed, expectedKey: expectedKey)
    }

    /// ADR-0073 R.2 — the listed devices that are the expected person's.
    func devicesOfExpectedPerson<D>(
        _ devices: [D], expectedKey: Data, keyPackageOf: (D) -> Data
    ) -> [D] {
        AddMemberKey.devicesBoundTo(
            devices,
            acceptedKeysB64: AddMemberKey.acceptableBoundKeys(
                expectedKeyB64: base64Encode(expectedKey), proven: provenPredecessorsProvider()),
            boundKeyOf: { AddMemberKey.boundKeyB64(keyPackage: keyPackageOf($0)) })
    }

    func devicesOfExpectedPerson(
        _ devices: [DeviceKeyPackage], expectedKey: Data
    ) -> [DeviceKeyPackage] {
        devicesOfExpectedPerson(devices, expectedKey: expectedKey, keyPackageOf: { $0.keyPackage })
    }

    /// One device's Add: a proposal (a group with no role table is refused).
    /// Serialized on the group's commit chain.
    private func addOneDeviceToGroup(
        groupIdHex: String,
        keyPackageBytes: Data
    ) async throws -> MembershipOutcome {
        // ADR-0040 (G-35) — serialize the whole eager-merge-through-publish
        // region against every other membership commit on this group.
        return try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            // ADR-0045 — membership goes through proposals. ADR-0081
            // amendment (2026-10-19): the direct-commit path for a group with
            // no role table is gone; such a group is refused, fail closed.
            try requireRoleTable(groupIdHex: groupIdHex, op: "add")
            return try await proposeAddMemberLocked(
                groupIdHex: groupIdHex,
                keyPackageBytes: keyPackageBytes
            )
        }
    }

    /// Remove a member from an EXISTING group by leaf index. Publishes the
    /// removal commit to the group journal (own-marked for echo suppression).
    /// Returns the commit's DS message id.
    public func removeMemberFromGroup(
        groupIdHex: String,
        leafIndex: UInt32
    ) async throws -> MembershipOutcome {
        // ADR-0050 — removing a member removes the PERSON: every leaf of the
        // account the named leaf belongs to (leaving, naming one's own leaf,
        // leaves from every device at once). Leaves are named by signature key
        // and re-resolved to an index per step, since a commit between steps
        // may renumber.
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // The intent filed after the removal is this binding's account's.
        let bound = binding.capture()
        let roster = (try? record.group.members()) ?? []
        let leafKeys = PersonKey.leavesOfSamePerson(roster, leafIndex: leafIndex)
            .compactMap { leaf in roster.first(where: { $0.leafIndex == leaf })?.signaturePublicKey }
        // ADR-0050 2.5a — the intent names the PERSON by key, never the leaf:
        // a re-request re-resolves their current leaves.
        let personKeyB64 = roster.first(where: { $0.leafIndex == leafIndex })
            .map { base64Encode(PersonKey.of($0)) }
        let intent: MembershipIntent.Request? = personKeyB64.map { person in
            person == AuthorIdentity.own(group: record.group)
                ? .leave(ownPersonKeyB64: person)
                : .remove(personKeyB64: person)
        }
        if leafKeys.count <= 1 {
            let outcome = try await removeOneLeafFromGroup(
                groupIdHex: groupIdHex, leafIndex: leafIndex)
            try bound.require()
            if case .proposed = outcome, let intent {
                recordMembershipIntent(groupIdHex: groupIdHex, intent)
            }
            return outcome
        }
        var outcome: MembershipOutcome?
        for key in leafKeys {
            guard let current = groups[groupIdHex],
                  let leaf = (try? current.group.members())?
                    .first(where: { $0.signaturePublicKey == key })?.leafIndex
            else { continue }
            outcome = try await removeOneLeafFromGroup(groupIdHex: groupIdHex, leafIndex: leaf)
            try bound.require()
        }
        guard let outcome else {
            throw ChatError.argument("member is no longer in \(groupIdHex)")
        }
        if case .proposed = outcome, let intent {
            recordMembershipIntent(groupIdHex: groupIdHex, intent)
        }
        return outcome
    }

    /// Remove ONE leaf: a proposal (a group with no role table is refused).
    ///
    /// The ADR-0043 / C-61 rotation runs when the sweep's commit lands —
    /// directly in ``sweepIfCommitter(groupIdHex:)`` on the committer, and via
    /// the poll's membership-shrink diff on every other device — so nothing is
    /// rotated here: there is no Remove commit yet.
    func removeOneLeafFromGroup(
        groupIdHex: String,
        leafIndex: UInt32
    ) async throws -> MembershipOutcome {
        // Fast-fail pre-checks; the locked section re-runs them against fresh
        // state.
        guard canonicalIdentity != nil else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard groups[groupIdHex] != nil else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // ADR-0081 amendment (2026-10-19) — the direct-commit path for a
        // group with no role table is gone; such a group is refused.
        try requireRoleTable(groupIdHex: groupIdHex, op: "remove")
        return try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            try await proposeRemoveMemberLocked(
                groupIdHex: groupIdHex,
                leafIndex: leafIndex
            )
        }
    }

    /// ADR-0027 (G-3) — the joiner signature keys of the external-join
    /// requests this group has retained pending an owner/admin decision. Each
    /// entry is the `proposalRef` handle `approveExternalJoin` /
    /// `rejectExternalJoin` take. Empty when there are none (or no live Group
    /// handle for the id).
    public func pendingExternalJoins(groupIdHex: String) -> [Data] {
        guard let record = groups[groupIdHex] else { return [] }
        let all = (try? record.group.pendingExternalJoins()) ?? []
        // A request rejected here whose reject did not reach the snapshot is
        // not shown again (UX section 8).
        guard let accountIdB64 = currentAccountIdB64 else { return all }
        let rejected = groupSettingsStore.rejected(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        guard !rejected.isEmpty else { return all }
        return all.filter { rejected[base64Encode($0)] == nil }
    }

    /// ADR-0045 UX §7 — the same retained set, but with a TORN read reported as
    /// `nil` rather than folded into an empty list.
    ///
    /// A panel wants the fold (nothing to render either way). The retry verdict
    /// does not: "no request retained" is a reason to DROP an armed approve,
    /// and a failed read must not be allowed to say that.
    public func retainedExternalJoinKeys(groupIdHex: String) -> [Data]? {
        guard let record = groups[groupIdHex] else { return nil }
        return try? record.group.pendingExternalJoins()
    }

    /// ADR-0053 — the ACCOUNT key a pending external-join request belongs to,
    /// from the joiner's verified device binding (the joiner's own key when its
    /// leaf is unbound). A request is keyed on the joiner's DEVICE leaf key and
    /// contacts on the account key, so this is what names a contact on the
    /// request row. Approve / reject still take the leaf key. `nil` when the
    /// group is unknown here or the read fails.
    public func pendingExternalJoinAccountKey(groupIdHex: String, joinerSignatureKey: Data) -> Data? {
        guard let record = groups[groupIdHex] else { return nil }
        return (try? record.group.pendingExternalJoinAccountKey(
            joinerSignatureKey: joinerSignatureKey)) ?? nil
    }

    /// ADR-0027 / UX §7.1 — whether a request the quarantine turned away has
    /// been observed for this group since the last time an admin acted.
    ///
    /// Not derivable from ``pendingExternalJoins(groupIdHex:)``: an unkept
    /// request is precisely the one that never entered that set.
    public func sawUnkeptExternalJoin(groupIdHex: String) -> Bool {
        groups[groupIdHex]?.sawUnkeptExternalJoin ?? false
    }

    /// Forget the unkept-request observation, because acting on the queue is
    /// what may have made room for the request it turned away.
    public func clearUnkeptExternalJoin(groupIdHex: String) {
        groups[groupIdHex]?.sawUnkeptExternalJoin = false
    }

    /// ADR-0028 OQ2 (G-3) — read the join-attribution envelopes from this group's
    /// knocks (ADR-0053) so the admin UI can show which member's `/g/` link each pending
    /// request arrived on. For every valid attribution targeting this group
    /// (the embedded capability re-verified via `parseGroupJoinAttribution`),
    /// returns the joiner signature key, the issuer fingerprint, and whether the
    /// issuer is in the current roster. Advisory — the role-gated approval gate
    /// is the boundary, so failures degrade to an empty list.
    ///
    /// `nil` when the knocks could not be read (retried by the caller later);
    /// an empty list when they were read and carry no attribution.
    public func fetchJoinAttributions(
        groupIdHex: String
    ) async -> [(joinerSignatureKey: Data, issuerFingerprint: String, issuerInRoster: Bool)]? {
        guard let record = groups[groupIdHex] else { return nil }
        guard let groupId = try? record.group.groupId() else { return nil }
        // ADR-0050 — the capability issuer is an ACCOUNT key.
        let rosterKeys = Set(PersonKey.people((try? record.group.members()) ?? []))
        // ADR-0053 point 6 — a joiner's attribution rides its knock, not the
        // journal: read the knocks, signed like a group read.
        let bound = binding.capture()
        guard let raw = try? await dsClient.fetchKnocks(
            groupId: groupId, after: 0, waitSecs: 0, auth: dsAuth(groupIdHex: groupIdHex))
        else { return nil }
        // The reply grants kept below go into the reach record of whoever is
        // signed in: not after this binding's account has left.
        guard bound.isCurrent else { return nil }
        var out: [(Data, String, Bool)] = []
        for m in raw.messages {
            guard let attr = try? parseGroupJoinAttribution(bytes: m.payload) else { continue }
            if attr.groupId != groupId { continue }
            keepJoinAttributionGrant(record: record, payload: m.payload)
            out.append(
                (attr.joinerSignatureKey, attr.issuerFingerprint,
                 rosterKeys.contains(attr.issuerSignaturePubkey)))
        }
        return out
    }

    /// ADR-0027 (G-3), on the ADR-0045 proposal path since **S7** — approve a
    /// pending external-join request (owner/admin only; the core role-gates it,
    /// fail-closed).
    ///
    /// ## One artifact, and no epoch
    ///
    /// The core publishes an ordinary admin-authored `Add` **proposal**
    /// carrying the joiner's retained KeyPackage — the admin's own
    /// MLS-authenticated signature *is* the approval — so this posts exactly one
    /// thing to the group journal and the epoch does not move. Everything the
    /// old direct-commit shape needed is therefore gone: no Welcome to compute
    /// here, no `WelcomeRendezvous` to post here, and no ADR-0040 outbox entry,
    /// because a proposal makes nothing durable that a crash could strand.
    ///
    /// The joiner's Welcome is produced by whichever device later sweeps this
    /// proposal into a commit. See `Chat/CommitterSweep.swift`: that sweep sends
    /// it to every added leaf's own mailbox (ADR-0053 point 8), blind to which
    /// of them came from a link.
    ///
    /// ## No pre-drain, and no pending-set refusal
    ///
    /// Both are gone with the direct commit they existed for. The core's
    /// `refuse_if_proposals_pending` no longer guards this path, so an approve
    /// composes with whatever else is waiting — which matters, because under
    /// ADR-0045 a non-empty pending set is the ORDINARY state and an admin
    /// standing down behind a live owner could otherwise essentially never
    /// approve.
    ///
    /// A refusal throws and leaves the request pending in the core, so the
    /// Approve affordance stays and can be retried.
    ///
    /// Returns the proposal's DS id.
    public func approveExternalJoin(
        groupIdHex: String,
        proposalRef: Data
    ) async throws -> Int64 {
        // Serialized against every other membership operation on this group: a
        // proposal grows the pending set, so two approves taken at once are
        // still a read-modify-write on the same MLS store.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            let id = try await approveExternalJoinLocked(
                groupIdHex: groupIdHex,
                proposalRef: proposalRef
            )
            recordMembershipIntent(
                groupIdHex: groupIdHex, .approveJoin(joinerKeyB64: base64Encode(proposalRef)))
            return id
        }
    }

    /// The ``approveExternalJoin(groupIdHex:proposalRef:)`` critical section.
    /// Runs only under ``withCommitLock(groupIdHex:_:)`` and re-reads
    /// `canonicalIdentity` + `groups` itself — a predecessor may have replaced
    /// or evicted the record while this section was queued.
    private func approveExternalJoinLocked(
        groupIdHex: String,
        proposalRef: Data
    ) async throws -> Int64 {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let proposalBytes: Data
        do {
            proposalBytes = try record.group.approveExternalJoin(
                identity: identity,
                proposalRef: proposalRef
            )
        } catch {
            throw ChatError.wrapped("approveExternalJoin failed", underlying: error)
        }
        // Best-effort, like every other propose path: a proposal advances no
        // epoch, so a snapshot that does not land costs a re-fetch of a journal
        // row, never a desync. A REFUSED write is not that case: see
        // ``persistSnapshotBeforePublish()``.
        try persistSnapshotBeforePublish()

        let bound = binding.capture()
        let publish: PublishResult
        do {
            publish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: proposalBytes,
                auth: dsAuth(groupIdHex: groupIdHex)
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (approve proposal) failed", underlying: error)
        }
        // The bookkeeping below is filed under the account that is bound now.
        try requireCommitRegion(bound)
        chatClient.markOwnPublishedCommit(groupIdHex: groupIdHex, messageId: publish.id)
        // ADR-0045 S7 residual 1 — record the approval as OUR OWN request, and
        // flag where it came from. Only this device can know that: on the wire
        // an approved add is indistinguishable from an admin adding a contact.
        // If the sweep never carries it, that flag is what makes the notice say
        // `Ask them to request to join again` rather than offering a retry this
        // side cannot perform — a successful approve CONSUMED the retained
        // request, so the admin cannot even re-approve.
        if let accountIdB64 = currentAccountIdB64,
           let atMs = Self.parseDsTimestampMs(publish.createdAt) {
            pendingRequestStore.record(
                accountIdB64: accountIdB64,
                groupIdB64: base64Encode(record.groupId),
                key: "add:\(base64Encode(proposalRef))",
                atMs: atMs,
                own: true,
                joinRequest: true
            )
        }
        state = state.with(threads: buildThreadList())
        return publish.id
    }

    /// ADR-0027 (G-3) — reject a pending external-join request (owner/admin
    /// only). Drops the retained proposal so it leaves the pending list. No
    /// commit, no journal post — the joiner never receives a Welcome.
    ///
    /// The retained set lives inside the MLS storage snapshot (which is why the
    /// core takes the identity: it writes the durable copy), so the
    /// `persistSnapshotBestEffort()` below is what makes the rejection survive
    /// a restart — without it the request would reappear on the next launch.
    public func rejectExternalJoin(
        groupIdHex: String,
        proposalRef: Data
    ) throws {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // UX section 8 — the reject is remembered durably before the core
        // drops the request, so neither the knock row (it stays on the DS)
        // nor a snapshot that missed the reject brings it back after a
        // restart; the joiner's reply grant goes with it.
        let joinerB64 = base64Encode(proposalRef)
        let accountKey = try? record.group.pendingExternalJoinAccountKey(joinerSignatureKey: proposalRef)
        if let accountIdB64 = currentAccountIdB64 {
            let meta = groupSettingsStore.requestMeta(accountIdB64: accountIdB64, groupIdHex: groupIdHex)[joinerB64]
            groupSettingsStore.noteRejected(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, joinerKeyB64: joinerB64,
                throughRowId: max(meta?.knockRowId ?? 0, record.knockCursor))
        }
        do {
            try record.group.rejectExternalJoin(identity: identity, proposalRef: proposalRef)
        } catch {
            throw ChatError.wrapped("rejectExternalJoin failed", underlying: error)
        }
        persistSnapshotBestEffort()
        if let accountKey = accountKey ?? nil {
            ReachCenter.shared.withdrawReplyGrants(ofAccountKeyB64: base64Encode(accountKey))
        }
    }

    /// ADR-0028 (G-3) — mint a group invite-link (owner/admin side). Signs a
    /// `GroupInviteCapability` over this group's raw MLS `group_id` with the
    /// local identity's Ed25519 key and returns the `https://lakko.me/g/<slug>`
    /// link the owner shares out-of-band. A non-member opens it, reads the
    /// current epoch from the journal, and posts an external-join proposal an
    /// admin approves (ADR-0027). The link conveys only the `group_id` — it
    /// authorizes nobody; the admin gate is the security boundary. `0` = never
    /// expires.
    ///
    /// **ADR-0028 v2 - the link carries a sealed preview.** The group's name
    /// (the shared store's, empty = none), this device's own profile display
    /// name (empty = none) and the roster size at this moment are sealed under
    /// a key derived from a fresh 12-byte link secret (bound to the group id and
    /// the issuer) inside the signed capability, and the secret rides the URL
    /// **fragment** - `https://lakko.me/g/<8-char id>#<16-char secret>` - which never
    /// reaches the discovery ledger (the pointer upload carries the ciphertext
    /// only) and is never logged; `Copy link` copies the whole string. A failed
    /// upload, a network one included, mints no link (the inline form was
    /// removed on 2026-10-19). A name the display-name
    /// rule refuses fails the mint rather than minting a link that says
    /// something else.
    public func createGroupInviteLink(groupIdHex: String) async throws -> String {
        // ADR-0074 Stage A — the issuer is the key the roster knows this
        // person by.
        guard let identity = conversationAccountIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // ADR-0087 section 5 — a link carries the group's knock key, so the
        // request it brings is a row only an owner or admin can open.
        guard let knock = knockKey(groupIdHex: groupIdHex) else {
            throw ChatError.argument("no knock key for \(groupIdHex) on this device")
        }
        let mint: InviteLinkMint
        do {
            mint = try composeGroupInviteCapabilityWithPreview(
                issuerSigningSeed: identity.ed25519Seed(),
                groupId: record.group.groupId(),
                expiryMs: 0,
                groupName: groupName(groupIdHex: groupIdHex),
                inviterName: ProfileDirectory.shared.ownProfile?.displayName,
                memberCount: UInt16(clamping: memberCount(groupIdHex: groupIdHex) ?? 0),
                knockPublicKey: knock.publicKey,
                knockVersion: knock.version,
                avatar: groupPhoto(groupIdHex: groupIdHex)?.ffi
            )
        } catch {
            throw ChatError.wrapped(
                "composeGroupInviteCapabilityWithPreview failed", underlying: error)
        }
        // Production default (ADR-0028 OQ1): upload to the discovery ledger —
        // the server-side anti-abuse surface (per-joiner consume dedup + soft
        // max_uses) — and share the short pointer URL. `max_uses` is a soft
        // throttle, not an access control: the admin approval gate stays the
        // boundary. ADR-0028 amendment 2026-09-28: the ledger gets the SEALED
        // blob and the clear fields; the secret rides the fragment.
        //
        // The upload is session-gated (`require_session` on discovery), so it
        // goes out on a bearer copy of the client; with no session at all the
        // mint fails here. Any upload failure surfaces and mints nothing.
        let bearer = try bearerDiscovery()
        // ADR-0078 section 10: the ledger row carries a key minted for this
        // link alone, never the issuer's identity key; its seed is kept in
        // this device's record of the link and is the only thing that revokes
        // it.
        let revocation: (seed: Data, publicKey: Data)
        do {
            revocation = try LinkRevocationKey.fresh()
        } catch {
            throw ChatError.wrapped("link revocation key", underlying: error)
        }
        let base: String
        do {
            let blobId = try await bearer.uploadGroupInvite(
                mint: mint, revocationPubkey: revocation.publicKey, expiryMs: 0, maxUses: UInt32.max)
            base = try GroupInviteUrl.composePointerGroupInviteUrl(blobId: blobId)
            if let accountIdB64 = currentAccountIdB64 {
                OwnLinkStore.shared.put(accountIdB64: accountIdB64, OwnLink(
                    kind: .group, slug: base64UrlEncodeNoPad(blobId), revocationSeed: revocation.seed,
                    label: "", expiryMs: 0, maxUses: Int64(UInt32.max), bootstrapReferral: false,
                    mintedAtMs: Int64(Date().timeIntervalSince1970 * 1000)))
            }
        } catch {
            NSLog("group invite upload failed (\(Self.discoveryFailureSummary(error)))")
            throw ChatError.wrapped("group invite upload failed", underlying: error)
        }
        return InviteLinkSecret.link(base: base, secret: mint.linkSecret)
    }

    /// The signed-in session's bearer token, read at call time. Defaults to
    /// ``AmbientSessionToken/shared``, which ``AuthRepository`` keeps in step
    /// with every sign-in, restore, refresh and logout — unlike the token
    /// captured at ``bootstrap(secrets:)``, which runs once per account and
    /// goes stale on a re-login to the same account. Tests replace it.
    var bearerSessionToken: () -> String? = { AmbientSessionToken.shared.current }

    /// A bearer copy of ``discovery`` for one `require_session` request
    /// (group-invite upload and revoke). Throws when there is no session
    /// token, so the caller fails loudly instead of sending a request the
    /// server can only answer with a 401. The shared ``discovery`` stays
    /// anonymous: its KeyPackage and invite reads must not name the account.
    func bearerDiscovery() throws -> DiscoveryClient {
        guard let token = bearerSessionToken(), !token.isEmpty else {
            throw ChatError.argument("not signed in: no session token for discovery")
        }
        return discovery.withSessionToken(token)
    }

    /// A log-safe summary of a discovery failure: the kind and the HTTP
    /// status, never a body or an argument message (either may echo the
    /// capability).
    static func discoveryFailureSummary(_ error: Error) -> String {
        guard let discovery = error as? DiscoveryClientError else { return "unexpected error" }
        switch discovery {
        case .network: return "network"
        case .http(let status, _, _): return "http \(status)"
        case .argument: return "invalid argument"
        case .rateLimited: return "rate limited"
        }
    }

    /// ADR-0028 revoke — the issuer revokes a previously-created group invite
    /// link. Returns `true` when its ledger row was revoked, `false` for a
    /// malformed link, or one this device keeps no revocation key for
    /// (ADR-0078 section 10: it lives until it expires).
    public func revokeGroupInviteLink(link: String) async throws -> Bool {
        guard let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        // The slug ends where the fragment begins: the link key after `#` is
        // not part of the slug and is never handed to a parser or a server.
        let withoutFragment = InviteLinkSecret.split(link).base
        guard let range = withoutFragment.range(of: "/g/", options: .backwards) else {
            return false
        }
        let slug = String(withoutFragment[range.upperBound...])
        let parsed: GroupInviteUrl.ParsedGroupInvite
        do {
            parsed = try GroupInviteUrl.parseGroupInviteSlug(slug)
        } catch {
            return false
        }
        switch parsed {
        case .pointer(let blobId):
            // Session-gated like the upload (`require_session`); the signature
            // by the link's own revocation key is the authorisation, the
            // bearer the DoS gate.
            let slug = base64UrlEncodeNoPad(blobId)
            guard let own = OwnLinkStore.shared.link(accountIdB64: accountIdB64, kind: .group, slug: slug),
                  let seed = own.seed else { return false }
            let bearer = try bearerDiscovery()
            do {
                _ = try await bearer.revokeGroupInvite(blobId: blobId, signingSeed: seed)
            } catch {
                NSLog("group invite revoke failed (\(Self.discoveryFailureSummary(error)))")
                throw ChatError.wrapped("group invite revoke failed", underlying: error)
            }
            OwnLinkStore.shared.remove(accountIdB64: accountIdB64, kind: .group, slug: slug)
            return true
        }
    }

    /// ADR-0028 (G-3) — resolve a `/g/<slug>` slug (a 6-byte ledger id) to the
    /// SEALED link blob (ADR-0028 amendment 2026-09-28), fetched from the
    /// discovery ledger. The caller opens it with the fragment's secret.
    /// Returns the `blobId` so the caller can drive the consume gate before
    /// requesting to join.
    ///
    /// A slug of any other shape (the retired 43-character id and an inline
    /// sealed blob, the form removed on 2026-10-19, included) throws
    /// ``GroupInviteLinkError/malformed`` without a request, and a ledger 404
    /// throws ``GroupInviteLinkError/notFound``; both are "invalid or expired"
    /// to a person. A `429` surfaces as a wrapped
    /// ``DiscoveryClientError/rateLimited(endpoint:)`` - try again later, never
    /// "invalid".
    public func resolveGroupInviteSlug(
        slug: String
    ) async throws -> (sealed: Data, blobId: Data) {
        let parsed: GroupInviteUrl.ParsedGroupInvite
        do {
            parsed = try GroupInviteUrl.parseGroupInviteSlug(slug)
        } catch {
            throw GroupInviteLinkError.malformed
        }
        switch parsed {
        case .pointer(let blobId):
            let fetched: Data?
            do {
                fetched = try await discovery.fetchGroupInvite(blobId: blobId)
            } catch {
                throw ChatError.wrapped("group invite fetch failed", underlying: error)
            }
            guard let sealed = fetched else {
                throw GroupInviteLinkError.notFound
            }
            return (sealed, blobId)
        }
    }

    /// Send a row that did not get there the first time.
    ///
    /// Re-publishes the envelope from the row itself, so the words the person
    /// wrote are the words that go - they were never anywhere else to be
    /// fetched from.
    ///
    /// **It can deliver the message twice.** A publish that reached the
    /// server and lost its answer is indistinguishable here from one that
    /// never arrived: the send is not idempotent, there is no key to
    /// de-duplicate on, and our own message is one the MLS ratchet will not
    /// decrypt a second time, so the journal cannot be consulted either. The
    /// same is true of a person re-typing a message by hand, which is what
    /// they did before this existed; a button makes it cheaper to reach, not
    /// newly possible. Losing the message is the worse failure.
    public func retrySend(groupIdHex: String, localId: Int64) async throws {
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        guard let row = record.messages.first(where: { $0.localId == localId }),
              row.ack == .failed
        else { return }
        // ADR-0073 R.7 case 1 — a retry in a dead one-to-one holds the
        // message again: the ordinary retry of the ordinary failed state.
        if sendRoute(groupIdHex: groupIdHex) == .hold {
            guard holdMessage(
                groupIdHex: groupIdHex, text: row.text, attachments: row.attachments,
                reply: row.reply, forward: row.forward, existingLocalId: localId)
            else { throw ChatError.argument("the message could not be kept for sending") }
            return
        }
        record.markPending(localId: localId)
        state = state.with(threads: buildThreadList())
        let envelopeBytes: Data
        do {
            envelopeBytes = try Self.composeChatEnvelope(
                text: row.text,
                attachments: row.attachments,
                reply: row.reply,
                forward: row.forward
            )
        } catch {
            record.markFailed(localId: localId)
            state = state.with(threads: buildThreadList())
            throw ChatError.wrapped("envelope compose failed", underlying: error)
        }
        let result: SendMessageResult
        do {
            result = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                try await sealAndPublishLocked(
                    groupIdHex: groupIdHex, plaintextBytes: envelopeBytes, wake: .message)
            }
        } catch {
            record.markFailed(localId: localId)
            state = state.with(threads: buildThreadList())
            throw error
        }
        record.markSent(
            localId: localId, messageId: result.messageId, createdAt: result.createdAt)
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// The chat envelope for a message, a reply or not.
    ///
    /// ADR-0052 decision 1 — a reply is an ordinary chat message that also
    /// points somewhere, so it goes through `composeReplyEnvelope` (the same
    /// `text` + `attachments`, plus the reference and the snapshot) and never
    /// through the mutually exclusive op composers. The core truncates an
    /// over-long snapshot on a character boundary and drops an empty one.
    ///
    /// ADR-0052 decision 2 — a forward is likewise an ordinary chat message
    /// that also says where it came from, through `composeForwardEnvelope`.
    /// A forward never carries a quote (section 5.1: "a forwarded message's
    /// quote is NOT carried"), so the two never meet; the forward is checked
    /// first so that a row somehow holding both is sent as what it is.
    static func composeChatEnvelope(
        text: String,
        attachments: [AttachmentRef],
        reply: ReplyRef?,
        forward: ForwardRef? = nil
    ) throws -> Data {
        if let forward {
            return try AttachmentBridge.composeForwardBytes(
                text: text,
                attachments: attachments,
                forward: forward
            )
        }
        if let reply {
            return try AttachmentBridge.composeReplyBytes(
                text: text,
                attachments: attachments,
                reply: reply
            )
        }
        return try AttachmentBridge.composeEnvelopeBytes(
            text: text,
            attachments: attachments
        )
    }

    public func sendMessage(
        groupIdHex: String,
        plaintext: String,
        attachments: [AttachmentRef] = [],
        reply: ReplyRef? = nil,
        forward: ForwardRef? = nil
    ) async throws {
        guard canonicalIdentity != nil else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        // ADR-0073 R.7 case 1 — nothing is sent into a one-to-one whose other
        // person's reset this device accepted (only removed devices would get
        // it). The message is HELD: the bubble shows the ordinary sending
        // state and it goes out when the successor exists.
        if sendRoute(groupIdHex: groupIdHex) == .hold {
            guard holdMessage(
                groupIdHex: groupIdHex, text: plaintext, attachments: attachments,
                reply: reply, forward: forward)
            else { throw ChatError.argument("the message could not be kept for sending") }
            return
        }
        // ADR-0048 §2 — a newcomer this device owes its profile gets the
        // profile before this message (taken before the message's own lock;
        // the profile send takes the same per-group chain and is not nested).
        await profilePublisher.publishOwedBeforeSend(groupIdHex: groupIdHex)
        // 1.19c3: build the JSON envelope before encrypting.
        // `composeEnvelopeBytes` routes through the UniFFI
        // `composeEnvelope` free function — wrong-length byte
        // fields are rejected there (defence-in-depth: the
        // UI already validates the AttachmentRefs it builds via
        // `AttachmentBridge.encryptAndUpload`).
        //
        // ADR-0052 — a reply composes through `composeReplyEnvelope` instead,
        // carrying the target id and the snapshot beside the same text.
        let envelopeBytes: Data
        do {
            envelopeBytes = try Self.composeChatEnvelope(
                text: plaintext,
                attachments: attachments,
                reply: reply,
                forward: forward
            )
        } catch {
            throw ChatError.wrapped(
                "envelope compose failed",
                underlying: error
            )
        }
        // The row goes into the conversation BEFORE the publish, reading
        // "Sending...". It used to be appended only on success, so a message
        // written on a bad connection left the person looking at an error
        // over an unchanged composer with nothing in the conversation - the
        // words were safe but the message was not there. On failure the row
        // stays and says "Not sent - Retry" (`docs/ux/media-messages.md`
        // section 3), which is the same contract the web client keeps.
        //
        // ADR-0023 — stamp our own IDENTITY as the author so a later edit /
        // delete of this message passes the author-only gate locally (and, on
        // every peer, against the identity they resolved for us at receipt).
        //
        // Stamped at SEND time, from the roster we are a member of right now —
        // the same "resolve once, carry the key" shape the receive path uses
        // for everyone else. Reading a leaf here and comparing it at gate time
        // is what let any leaf, ours included, be reassigned to a member added
        // later.
        let localId = record.nextLocalId()
        // The reply rides the local row too, so the sender sees its own quote
        // (it never receives its own application message back).
        record.appendPending(
            localId: localId,
            text: plaintext,
            attachments: attachments,
            createdAt: Self.isoTimestamp(ms: Self.nowMs()),
            authorKeyB64: AuthorIdentity.own(group: record.group),
            reply: reply,
            forward: forward
        )
        state = state.with(threads: buildThreadList())
        // ADR-0045 — the seal and the publish are SERIALIZED against the
        // regions that move this group's epoch. See ``sealAndPublishLocked``.
        let result: SendMessageResult
        do {
            result = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                // ADR-0054 decision 1 — a chat message asks for a message
                // wake, carried on the publish itself.
                try await sealAndPublishLocked(
                    groupIdHex: groupIdHex, plaintextBytes: envelopeBytes, wake: .message)
            }
        } catch {
            record.markFailed(localId: localId)
            state = state.with(threads: buildThreadList())
            // Deliberately NOT persisted: an unsent row is not written to the
            // message log, so a relaunch is where a failed send stops being
            // retryable. Making that survive is a durable outbox, which no
            // client has.
            throw error
        }
        record.markSent(
            localId: localId,
            messageId: result.messageId,
            createdAt: result.createdAt)
        // The MLS ratchet snapshot was already durably persisted
        // (fail-closed) inside `chatClient.sendMessage` BEFORE the publish,
        // and neither the publish nor `appendOutgoing` advances MLS state,
        // so re-persisting the snapshot here is unnecessary. Only the
        // plaintext timeline changed (the just-appended row), so save that
        // best-effort — its loss cannot cause nonce reuse.
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0054 decision 1 — the wake rode the publish above as the
        // `x-lakko-wake` header; the DS forwards it to push-gw after the row
        // is appended. There is no second request from this device.
    }

    /// ADR-0023 — edit our own message `target` to `newText`.
    /// Composes the edit op envelope, publishes it via the
    /// existing application send path, and applies the new text
    /// locally (the sender never receives its own app message
    /// back). The author-only gate is enforced on every peer's
    /// receive side; locally we only edit our own outgoing rows.
    public func editMessage(
        groupIdHex: String,
        target: Int64,
        newText: String
    ) async throws {
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeEditBytes(
                target: target,
                newText: newText
            )
        } catch {
            throw ChatError.wrapped("edit envelope compose failed", underlying: error)
        }
        try await publishMessageOpLocked(
            groupIdHex: groupIdHex,
            envelopeBytes: envelopeBytes
        ) { record, opMessageId in
            record.applyLocalEdit(target: target, newText: newText, opMessageId: opMessageId)
        }
    }

    /// ADR-0023 — delete (tombstone) our own message `target`.
    /// Composes the delete op envelope, publishes it, and marks
    /// the message deleted locally.
    public func deleteMessage(
        groupIdHex: String,
        target: Int64
    ) async throws {
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeDeleteBytes(target: target)
        } catch {
            throw ChatError.wrapped("delete envelope compose failed", underlying: error)
        }
        try await publishMessageOpLocked(
            groupIdHex: groupIdHex,
            envelopeBytes: envelopeBytes
        ) { record, opMessageId in
            record.applyLocalDelete(target: target, opMessageId: opMessageId)
        }
    }

    /// ADR-0023 as amended — add or remove `emoji` on message `target`.
    ///
    /// The op is explicit (reactions-and-emoji spec section 17.3): the caller
    /// decides add or remove from ``reactionTapVerdict``, which also refuses a
    /// 4th reaction or a kind the conversation does not take, so this method
    /// never sends what the fold would ignore. `targetAuthor` is the target
    /// message's author account key, carried so a receiver's background reader
    /// can tell a reaction to one of its own messages. A reaction publishes
    /// with a message wake (section 12: a reaction to someone's message
    /// notifies them).
    public func reactToMessage(
        groupIdHex: String,
        target: Int64,
        emoji: String,
        add: Bool,
        targetAuthorB64: MemberIdentity?
    ) async throws {
        let envelopeBytes: Data
        do {
            envelopeBytes = try AttachmentBridge.composeReactionBytes(
                target: target,
                emoji: emoji,
                add: add,
                targetAuthor: targetAuthorB64.flatMap { Data(base64Encoded: $0) }
                    .flatMap { $0.count == 32 ? $0 : nil }
            )
        } catch {
            throw ChatError.wrapped("reaction envelope compose failed", underlying: error)
        }
        // ADR-0048 §2 — as for a message: an owed profile goes first.
        if add { await profilePublisher.publishOwedBeforeSend(groupIdHex: groupIdHex) }
        try await publishMessageOpLocked(
            groupIdHex: groupIdHex,
            envelopeBytes: envelopeBytes,
            wake: add ? .message : nil
        ) { [weak self] record, publishedId in
            // Read off the RE-READ record, for the same reason the record is
            // re-read at all — a predecessor's persist-abort replaces it.
            //
            // Our own person key, not our own leaf: the chip's "mine" test and
            // the peers' fold have to agree on one value that survives a
            // renumber and a second device of the same person.
            record.applyLocalReaction(
                target: target,
                emoji: emoji,
                reactor: self?.ownReactorKey(record: record) ?? AuthorIdentity.ownReactionSentinel,
                add: add,
                at: publishedId ?? 0)
        }
    }

    /// The key this device files its own reactions under: the person key the
    /// roster resolves this device's leaf to, else the conversation account
    /// key itself (both are the ADR-0050 account key on a bound leaf).
    func ownReactorKey(record: GroupRecord) -> MemberIdentity? {
        if let own = AuthorIdentity.own(group: record.group) { return own }
        return conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
    }

    /// The serialized body the three ADR-0023 message ops share: seal + persist
    /// + publish through ``sealAndPublishLocked``, then fold the op into OUR OWN
    /// timeline (the sender never receives its own application message back).
    ///
    /// ## Why an op takes the commit lock
    ///
    /// An edit / delete / reaction is an ordinary MLS application message, so it
    /// carries the ``sealAndPublishLocked`` hazard verbatim — that doc comment
    /// is the reasoning; this one records only what is specific to an op. These
    /// three called `ChatClient.sendMessage` bare, outside the chain, while the
    /// chat send and the control broadcast beside them were serialized.
    ///
    /// ## The local apply's ordering is deliberate, both ways
    ///
    /// **After the publish.** A delete that applied first would show the user
    /// "message deleted" for a message that never reached the wire and that
    /// every peer still renders — a user told their message is gone when it is
    /// not. A throw from ``sealAndPublishLocked`` therefore skips the apply
    /// entirely, which is what these paths already did and what routing them
    /// through the lock must not quietly change.
    ///
    /// **Inside the region.** ``withCommitLock(groupIdHex:_:)``'s contract is
    /// that a `GroupRecord` read BEFORE the wait can be stale: a predecessor
    /// may have REPLACED the record with a fresh one seeded from a *copy* of
    /// the timeline. An apply against the orphan would mutate nothing the UI reads
    /// and the edit would silently vanish. So the record is re-read here, the
    /// way every other `…Locked` body re-reads it — and the caller no longer
    /// looks one up at all.
    private func publishMessageOpLocked(
        groupIdHex: String,
        envelopeBytes: Data,
        wake: DsWake.Kind? = nil,
        applyLocally: @escaping @MainActor (GroupRecord, _ opMessageId: Int64?) -> Void
    ) async throws {
        // ADR-0073 R.7 case 1 — an op on a message of a dead one-to-one is
        // applied on this device alone: its target exists only in the old
        // session, so there is nothing a successor could apply it to.
        let localOnly = HeldSend.opRoute(
            kind: isOneToOne(groupIdHex: groupIdHex) ? .chat : .group,
            unrepairableKeyB64: unrepairableKeyB64(groupIdHex: groupIdHex)) == .localOnly
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            // ADR-0082 section 5 — the op's own DS id becomes the target's
            // last op id; `nil` for an op applied on this device alone.
            var opMessageId: Int64?
            if !localOnly {
                opMessageId = try await sealAndPublishLocked(
                    groupIdHex: groupIdHex,
                    plaintextBytes: envelopeBytes,
                    wake: wake
                ).messageId
            }
            guard let record = groups[groupIdHex] else { return }
            applyLocally(record, opMessageId)
        }
        if localOnly { persistMessageLogBestEffort() }
        persistSnapshotBestEffort()
        state = state.with(threads: buildThreadList())
        // ADR-0052 decision 3 — our own delete drops the message's shared pin
        // record, as a received one does. Only spawns a `Task`.
        dropSharedPinsOfDeletedMessages(groupIdHex: groupIdHex)
    }

    /// Phase 3a9 — publish raw plaintext bytes as an MLS
    /// application message. Same MLS-wrap + DS-publish path
    /// as ``sendMessage`` but skips the chat-envelope
    /// composer and the timeline append — call signaling
    /// rides this channel out-of-band.
    ///
    /// - Parameter wake: Which push wake the publish asks for. `nil` (the
    ///   default) asks for none, which is the ADR-0042 rule for a control
    ///   envelope and what every caller of this method wants: a typing
    ///   signal, a read receipt, a routing-secret handshake and a group-call
    ///   announcement must not wake anybody's device. ``DsWake/Kind/call`` is
    ///   the single exception, asked for by the 1x1 call publisher for an
    ///   offer and an end only (``CallSignalingWake``), because those two are
    ///   the whole of `docs/ux/calls.md` §2.1 — the ring starting and the ring
    ///   stopping on a device whose app is closed.
    ///
    /// ADR-0054 decision 1 — the wake rides the signed publish as the
    /// `x-lakko-wake` header and the DS forwards it after appending the row;
    /// this device makes no push-gw request.
    public func publishApplicationMessage(
        groupIdHex: String,
        plaintextBytes: Data,
        wake: DsWake.Kind? = nil
    ) async throws {
        // ADR-0045 — the seal and the publish are SERIALIZED against the
        // regions that move this group's epoch. See ``sealAndPublishLocked``.
        _ = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            try await sealAndPublishLocked(
                groupIdHex: groupIdHex, plaintextBytes: plaintextBytes, wake: wake)
        }
        persistSnapshotBestEffort()
    }

    /// The serialized body every application send shares: seal at the live
    /// epoch, persist fail-closed (inside `ChatClient.sendMessage`), publish.
    ///
    /// Runs ONLY under ``withCommitLock(groupIdHex:_:)``; never call it
    /// directly. Re-reads `canonicalIdentity` + `groups` for the usual reason —
    /// a predecessor may have REPLACED the record.
    ///
    /// ## Why the send takes the commit lock at all
    ///
    /// The hazard is NOT two `createApplicationMessage` calls interleaving —
    /// that UniFFI call is synchronous, so on `@MainActor` they cannot. Nor is
    /// it two same-epoch messages arriving out of order, which MLS resolves by
    /// generation. It is that a message can be SEALED at one epoch and PUBLISHED
    /// on the far side of a commit that changes it. `lakko-mls` never sets
    /// `max_past_epochs`, so it is openmls's default of **0**: a recipient keeps
    /// no past-epoch secrets, and a straddling message is not late but
    /// **permanently undecryptable for everyone**, dropped silently while the
    /// sender's own optimistic echo stays on screen.
    ///
    /// Both directions were reachable while this path held no lock:
    ///
    ///   - **sealed AHEAD.** A membership commit eager-merges N -> N+1 *inside*
    ///     its create and then parks in persist + publish, so a send in that
    ///     window seals at N+1 and can reach the DS first — a future epoch to
    ///     peers still at N.
    ///   - **sealed BEHIND.** An ADR-0045 sweep STAGES (the group stays at N),
    ///     publishes the commit, then spends a second round-trip on the journal
    ///     verdict. `MlsGroup::create_message` gates on `is_active()`, not
    ///     `is_operational()`, so a pending commit does NOT block a send: it
    ///     seals at N with the commit already on the wire, which makes the loss
    ///     a certainty rather than a race for that whole window.
    ///
    /// The lock closes the LOCAL half only, and that bound is real: a poll
    /// applying somebody ELSE's commit is a race against the DS, not against
    /// local state, so a message sealed here and published after a remote commit
    /// that was already ordered ahead of it is still lost. Closing that needs
    /// `max_past_epochs > 0` in `lakko-mls` (invariant #3 — an ADR and property
    /// tests), not a client-side lock.
    ///
    /// The DS publish is INSIDE the region, deliberately: the whole hazard is
    /// seal-then-publish-late, so leaving the publish out would fix nothing. The
    /// chain releases on the throw path as well as the success path, so a failed
    /// publish frees the queue rather than wedging it. The cost is that sends on
    /// one group now serialize — a latency cost on back-to-back sends, bounded
    /// because no caller of this lock long polls (the poll's 30 s fetch is
    /// outside it, and the sweep's verdict read is `waitSecs: 0`).
    ///
    /// `wake` (ADR-0054 decision 1) is resolved to a ``DsWake`` here, from the
    /// group's `push_gid` (``pushWake(groupIdHex:kind:)``), and rides the
    /// publish as its `x-lakko-wake` header.
    func sealAndPublishLocked(
        groupIdHex: String,
        plaintextBytes: Data,
        wake: DsWake.Kind? = nil
    ) async throws -> SendMessageResult {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let bound = binding.capture()
        var pushWake: DsWake?
        if let wake { pushWake = await self.pushWake(groupIdHex: groupIdHex, kind: wake) }
        let client = chatClient
        let group = record.group
        let result = try await Self.$sendBinding.withValue(bound) {
            try await client.sendMessage(
                identity: identity,
                group: group,
                plaintext: plaintextBytes,
                wake: pushWake
            )
        }
        // What a caller does with the result inside its region (apply an
        // edit, a deletion or a reaction to `groups[groupIdHex]`) is done to
        // the record that is there now.
        try requireCommitRegion(bound)
        return result
    }

    /// ADR-0024 — publish an ephemeral typing signal for `groupIdHex`.
    /// Composes the typing op envelope (`{ typing: <bool> }`) and ships
    /// it through the existing application send path. Renders no bubble
    /// + no timeline append; the receivers fold it into ephemeral
    /// per-leaf typing UI state. The view model owns the throttle (one
    /// `true` per ~4 s) + the single `false` on blur / send — this
    /// method is the raw publish seam.
    public func sendTyping(
        groupIdHex: String,
        isTyping: Bool
    ) async throws {
        // ADR-0073 R.7 case 1 — typing is not sent into a dead one-to-one.
        if isDeadSession(groupIdHex: groupIdHex) { return }
        let envelopeBytes: Data
        do {
            envelopeBytes = try composeTypingEnvelope(isTyping: isTyping)
        } catch {
            throw ChatError.wrapped("typing envelope compose failed", underlying: error)
        }
        try await publishApplicationMessage(
            groupIdHex: groupIdHex,
            plaintextBytes: envelopeBytes
        )
    }

    /// ADR-0024 — publish a read receipt for `groupIdHex`: the highest
    /// messageId the local member has read in this group. Monotone +
    /// throttled at the view-model layer (only when the watermark
    /// advances past our own last-published value). Renders no bubble;
    /// receivers advance a monotone per-leaf watermark and render the
    /// author's own messages as "Read" once every other member's
    /// watermark covers them.
    public func sendReadReceipt(
        groupIdHex: String,
        upTo: Int64
    ) async throws {
        // ADR-0073 R.7 case 1 — nor is a read receipt.
        if isDeadSession(groupIdHex: groupIdHex) { return }
        let envelopeBytes: Data
        do {
            envelopeBytes = try composeReadReceiptEnvelope(upTo: upTo)
        } catch {
            throw ChatError.wrapped("read-receipt envelope compose failed", underlying: error)
        }
        try await publishApplicationMessage(
            groupIdHex: groupIdHex,
            plaintextBytes: envelopeBytes
        )
    }

    /// ADR-0054 decision 1 over ADR-0042, sealed by ADR-0078 section 3 — the
    /// wake a publish to `groupIdHex` carries: the group's `push_gid`, the
    /// kind, a nonce, this device's clock, the group's unlock key and a
    /// signature by the group's wake key, sealed to push-gw's key
    /// (``PushWakeKey``), so the DS forwards a blob it cannot read. When this
    /// device holds no secret for the group yet, or push-gw's key cannot be
    /// had, the answer is `nil` and the publish carries no header rather than
    /// leaking the `group_id` (invariant #5) — recipients still receive the
    /// message on their next foreground poll.
    ///
    /// ``DsWake/Kind/call`` raises the provider urgency so a closed app rings.
    /// The delivered payload is identical either way: a group pseudonym, the
    /// subscription id and a journal pointer, nothing about the call.
    /// push-gw's wake key (``PushWakeKey``); replaceable so a test supplies
    /// a fixed key instead of a fetch.
    var pushWakeKeyProvider: @MainActor () async -> Data? = { await PushWakeKey.key() }

    func pushWake(groupIdHex: String, kind: DsWake.Kind) async -> DsWake? {
        guard let accountIdB64 = currentAccountIdB64,
              let secret = pushRouting.secret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            // No secret held yet: do NOT wake under the DS key.
            return nil
        }
        guard let gwKey = await pushWakeKeyProvider() else { return nil }
        let nowMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        do {
            let value = try LakkoCore.pushWakeHeaderValue(
                pushGwKey: gwKey,
                secret: secret,
                kind: kind == .call ? .call : .message,
                timestampMs: nowMs)
            return DsWake(headerValue: value)
        } catch {
            NSLog("push wake sealing failed; publishing without a wake")
            return nil
        }
    }

    // MARK: - ADR-0042 opaque push routing

    /// ADR-0042 — ensure this group's opaque push routing is provisioned
    /// for this device. Owner + no secret yet → mint + persist + broadcast
    /// the distribution envelope (E2EE journal) + subscribe under
    /// `push_gid`; a member holding a secret → subscribe under `push_gid`;
    /// anyone else → defer (no push under `group_id`). Idempotent +
    /// race-safe (a shared per-group in-flight guard in ``PushGroupRouting``
    /// coalesces the chat view's onAppear with the legacy-sweep).
    /// Fire-and-forget — never blocks the chat.
    public func ensurePushRouting(groupIdHex: String) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        Task { @MainActor [weak self] in
            await self?.performEnsurePushRouting(groupIdHex: groupIdHex)
        }
    }

    /// @MainActor body of ``ensurePushRouting(groupIdHex:)`` — reads the
    /// live `Group` handle + canonical identity on the actor and hands them
    /// to the ``PushGroupRouting`` funnel (same actor, so the non-`Sendable`
    /// `Group` never crosses an actor boundary).
    private func performEnsurePushRouting(groupIdHex: String) async {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        await pushRouting.ensureProvisioned(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            group: record.group,
            identity: identity,
            // A vanished `self` must THROW, not optional-chain into a silent
            // success: ADR-0042 marks the secret distributed only when this
            // closure returns without throwing, and a no-op "success" would
            // durably record an envelope that never reached the journal.
            broadcast: { [weak self] bytes in
                guard let self else {
                    throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                }
                try await self.broadcastPushRoutingControl(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, bytes: bytes)
            }
        )
    }

    /// ADR-0042 — subscribe under `push_gid` (defers when no secret is
    /// held; never subscribes under the DS `group_id`). Fire-and-forget.
    public func subscribeGroupUnderPushGid(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        Task { @MainActor [weak self] in
            await self?.pushRouting.subscribeUnderPushGidBestEffort(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
    }

    /// ADR-0042 — a push-routing-secret distribution envelope arrived on
    /// the group journal: run the owner gate, then (on accept) persist the
    /// secret (idempotent) + subscribe under the derived `push_gid`.
    /// Fire-and-forget (best-effort).
    ///
    /// `senderLeaf` is the MLS-authenticated author of the envelope. It is
    /// resolved against the LIVE `Group` (roster + ADR-0022 role table) in
    /// ``performReceivePushRoutingSecret(groupIdHex:secret:senderLeaf:)`` —
    /// a *changed* secret from a non-owner is refused (`lakko-security`
    /// MEDIUM-1).
    public func receivePushRoutingSecret(
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        senderLeaf: UInt32?
    ) {
        guard currentAccountIdB64 != nil, groups[groupIdHex] != nil else { return }
        // Received by this binding's account: filed in its routing row only.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard bound.isCurrent else { return }
            await self?.performReceivePushRoutingSecret(
                groupIdHex: groupIdHex, secret: secret, mintEpoch: mintEpoch,
                senderLeaf: senderLeaf)
        }
    }

    /// @MainActor body of
    /// ``receivePushRoutingSecret(groupIdHex:secret:senderLeaf:)`` — reads
    /// the live `Group` handle on the actor and hands it to the gate (same
    /// actor, so the non-`Sendable` `Group` never crosses an actor
    /// boundary). The group is the ONLY authority for the owner key + the
    /// leaf → signature-key mapping.
    private func performReceivePushRoutingSecret(
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        senderLeaf: UInt32?
    ) async {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        await pushRouting.receiveSecret(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            mintEpoch: mintEpoch,
            group: record.group,
            senderLeaf: senderLeaf
        )
    }

    /// ADR-0042 — re-broadcast the current secret to a group (after an
    /// add-member commit). No-op when this device holds no secret.
    /// Fire-and-forget.
    private func redistributePushRoutingSecret(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.pushRouting.redistribute(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                // Throws rather than optional-chaining into a silent success —
                // see the note in `performEnsurePushRouting`.
                broadcast: { [weak self] bytes in
                    guard let self else {
                        throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                    }
                    try await self.broadcastPushRoutingControl(
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex, bytes: bytes)
                }
            )
        }
    }

    /// ADR-0043 — someone ELSE's Remove commit just landed on the journal.
    /// A Commit surfaces from the poll loop with no membership delta
    /// attached, so the removal is INFERRED by diffing the member signature
    /// keys captured BEFORE the poll batch against the post-poll roster.
    ///
    /// Keys, not counts and not leaf indices: OpenMLS reuses a vacated leaf
    /// for the next Add, so a remove+add in one batch leaves both the count
    /// and the index set unchanged while a member really was evicted.
    ///
    /// A removal this device committed itself (a sweep) comes through here
    /// too, from the sweep's tail (ADR-0081 amendment: no direct-commit path
    /// remains). Owner-or-admin only, no-op on growth. Fire-and-forget: a rotation failure must never
    /// break the poll loop.
    private func rotatePushRoutingSecretIfMembershipShrank(
        groupIdHex: String,
        previousMemberKeys: [String]?
    ) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        let role = ownRole(groupIdHex: groupIdHex)
        let current = PushGroupRouting.memberSignatureKeys(group: record.group)
        // ADR-0045 UX §6.2.8 item 3 — captured HERE, in the same synchronous
        // region as the post-commit roster snapshot one line above, so the
        // eviction and the shrink it caused are judged on the same commit. Both
        // entry points (the poll's apply half and the committer sweep's tail)
        // funnel through this one method, so there is exactly one place the two
        // reads can be ordered together and this is it.
        //
        // Read off the live handle rather than any UI mirror, and it is worse
        // than a no-op to get wrong: a `Remove` rewrites no `GroupContext`, so
        // an evicted admin's role table still names it an admin, the diff sees
        // a member depart (us), and the rotation mints a fresh secret,
        // subscribes to a `push_gid` nobody will ever notify, retires the old
        // one, and then throws at the broadcast — retrying on every chat-open.
        let selfEvicted = readIsActive { try record.group.isActive() } == false
        // The local group ended: it is no longer "a conversation a restored
        // backup named that this device has yet to join".
        if selfEvicted {
            dropRestoredKnownGroup(groupIdHex: groupIdHex)
            // ADR-0078 section 9 — the group's members hold this account's
            // one `group` and `member` key: the registration sees the group
            // gone and rotates both, republishing in every remaining group.
            ReachCenter.shared.sync()
            // ADR-0087 section 9 — the one seam: every applied commit comes
            // through here (the poll's apply half and the sweep's tail).
            endMembershipIfOut(groupIdHex: groupIdHex)
        }
        // ADR-0060 section 3 — a departed member held this account's `group`
        // and `member` keys for this group: rotate both (whatever this
        // device's role — every member owns its own keys), after the removal
        // was applied so the new grants are sealed at the post-removal epoch.
        if !selfEvicted,
           ReachRules.membershipShrank(previous: previousMemberKeys, current: current) {
            ReachCenter.shared.rotateGroup(record.groupId)
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.pushRouting.rotateIfMembershipShrank(
                accountIdB64: accountIdB64,
                groupIdHex: groupIdHex,
                ownRole: role,
                previousMemberKeys: previousMemberKeys,
                currentMemberKeys: current,
                // ADR-0044 — the same lazy post-commit read as the direct
                // path. The poll already applied the peer's Remove commit
                // locally before this runs, so the live epoch is the
                // post-removal one.
                mintEpoch: { [weak self] in
                    guard let group = self?.groups[groupIdHex]?.group else { return nil }
                    return PushGroupRouting.readGroupEpoch(group)
                },
                selfEvicted: selfEvicted,
                broadcast: { [weak self] bytes in
                    guard let self else {
                        throw ChatError.argument("ChatRepository deallocated mid-broadcast")
                    }
                    try await self.broadcastPushRoutingControl(
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex, bytes: bytes)
                }
            )
        }
    }

    /// ADR-0045 UX §6.2.8 item 5 — retire THIS device's own push subscription
    /// for a group whose membership has ended.
    ///
    /// ADR-0043 recorded the evicted member's push-gw row as never deleted —
    /// *"nobody else can delete it and no client has 'you were removed'
    /// handling"* — so it went **inert** rather than away. This is that
    /// handling, and it gives `retireOldPushGid` its third caller: the rotator
    /// after it re-arms, every remaining member on a changed secret, and now
    /// the ex-member itself. It closes the other half of G-36 residual 2 —
    /// rotation stops the group notifying the old id; this stops the ex-member
    /// being subscribed to anything at all.
    ///
    /// **The honest bound:** it needs this device to come online and run, so it
    /// *complements* rotation and replaces nothing. A phone that is never
    /// opened again keeps its row, exactly as before.
    ///
    /// Best-effort and never throws: `retireOldPushGid` is bounded and
    /// swallows its own failures, and there is nothing here to unwind.
    /// Returns `false` only when there was no secret to name the retiring id
    /// from — which is done rather than failed, so the caller keeps its claim.
    @discardableResult
    func retireOwnPushSubscription(groupIdHex: String) async -> Bool {
        guard let accountIdB64 = currentAccountIdB64 else { return false }
        return await pushRouting.retireOwnSubscription(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// ADR-0042 — post a control envelope (a push-routing-secret
    /// distribution envelope) to the group journal as an E2EE MLS
    /// application message via the fail-closed persist-before-publish path.
    ///
    /// ADR-0045 — takes the per-group commit chain, like every other send, so a
    /// routing envelope cannot straddle an epoch change either. The nesting is
    /// one-directional and therefore deadlock-free: this is reached only from
    /// under the ADR-0044 routing chain, and no section holding the COMMIT chain
    /// ever awaits the routing chain (every such call site only SPAWNS a
    /// `Task`). The routing layer already bounds this closure, so the routing
    /// chain's hold stays bounded too.
    ///
    /// `accountIdB64` is the account whose routing row `bytes` was composed
    /// from. The routing work that calls this is queued per `(account,
    /// group)` and outlives a sign-out; a secret of one account is never
    /// sealed and sent by another that signed in meanwhile and is in the same
    /// group (the same rule as ``PushGroupRouting/accountIsBound``).
    private func broadcastPushRoutingControl(
        accountIdB64: String, groupIdHex: String, bytes: Data
    ) async throws {
        _ = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            guard currentAccountIdB64 == accountIdB64 else { throw StaleBindingError() }
            guard let identity = canonicalIdentity else {
                throw ChatError.argument("ChatRepository not bootstrapped")
            }
            guard let record = groups[groupIdHex] else {
                throw ChatError.argument("no live Group handle for \(groupIdHex)")
            }
            let bound = binding.capture()
            let client = chatClient
            let group = record.group
            let result = try await Self.$sendBinding.withValue(bound) {
                try await client.broadcastControl(
                    identity: identity,
                    group: group,
                    controlBytes: bytes
                )
            }
            try requireCommitRegion(bound)
            return result
        }
    }

    /// Pull new messages from one group's journal + apply them to
    /// the in-memory timeline.
    ///
    /// ## G-38 — split poll: fetch outside the lock, apply inside it
    ///
    /// The poll APPLIES incoming commits (`processIncomingVerifyingRoles`
    /// mutates the shared OpenMLS provider through the same `Group` handle a
    /// membership commit is mid-operation on), so it is not a read-only
    /// observer and must not run beside a local commit. `@MainActor` isolation
    /// does not provide that on its own: a commit holds its create-through-
    /// publish region across `await dsClient.publishGroup`, and isolation is
    /// RELEASED at that suspension — a fully synchronous apply then lands
    /// squarely inside the window where the group has eager-merged N -> N+1 but
    /// the commit is not yet on the wire.
    ///
    /// So the apply takes the SAME per-group chain the four commit paths take
    /// (``withCommitLock(groupIdHex:_:)``), and only the apply:
    ///
    ///   1. Sweep the outbox (once per group per session) — OUTSIDE the lock,
    ///      unchanged, and deliberately so: it publishes commits itself, and the
    ///      chain is not reentrant.
    ///   2. Read the cursor and FETCH with it — OUTSIDE the lock. `waitSecs` is
    ///      a deliberate 30 s long poll on the live chat screen; taking the
    ///      lock across it would stall every send and membership commit on this
    ///      group behind an idle connection, which is G-37's own defect at a
    ///      per-group blast radius.
    ///   3. Take the lock, then RE-READ the record and the cursor. A predecessor
    ///      may have replaced the record or advanced the cursor while this
    ///      poll was parked on the network. A moved cursor
    ///      means the batch in hand starts at a row already consumed, so it is
    ///      DROPPED without applying and without touching the cursor — applying
    ///      it would re-feed consumed rows to OpenMLS and let the stale batch's
    ///      end cursor wind the record's cursor BACKWARD.
    ///   4. Apply + advance the cursor, atomically, in one non-suspending
    ///      region.
    ///
    /// ## No in-flight guard, unlike Android
    ///
    /// Android's `runSplitPoll` additionally claims a per-group in-flight flag
    /// and returns `SKIPPED_IN_FLIGHT` to the loser. iOS has never had one and
    /// deliberately still does not: the two poll sources here are a 30 s long
    /// poll (`ChatViewModel.pollOnceLongPoll`) and the list's pull-to-refresh
    /// (`ChatListViewModel.pollNow`), so a guard would make the pull a silent
    /// no-op for up to 30 s. It is only an optimisation — it saves a redundant
    /// fetch, it does not provide correctness. Correctness comes from the
    /// cursor-moved guard in ``applyFetchedBatch(groupIdHex:after:raw:)``: two
    /// overlapping polls both fetch, the first to reach the lock applies, and
    /// the other finds the cursor moved and drops its batch whole.
    ///
    /// ## Honest scope
    ///
    /// This closes the LOCAL interleave — this device's own poll against this
    /// device's own commit. A poll applying a REMOTE commit while a local one is
    /// in flight to the DS is a genuine MLS epoch fork that no client-side lock
    /// can address (it needs a DS-side epoch gate or proposal-based commits);
    /// that is G-38 residual 1 and is deliberately untouched here.
    public func pollGroup(
        groupIdHex: String,
        waitSecs: Int = 0
    ) async throws {
        guard canonicalIdentity != nil else { return }
        guard let record = groups[groupIdHex] else { return }
        // ADR-0045 UX §6.2.8 item 1 — the poll must STOP for a group this
        // device is no longer in, and stopping is not an optimisation.
        // `unprotect_message` refuses on `!is_active()` before it looks at the
        // message at all, and the refusal is not epoch-scoped, so every fetched
        // row would raise `UseAfterEviction`, be swallowed by the per-message
        // catch in `ChatClient.applyFetchedMessages`, and advance the cursor —
        // one warning per row, forever, for a group that will never produce a
        // readable byte again. A loop that logs a refusal per row per poll is
        // indistinguishable from a bug.
        //
        // iOS is the MOST exposed of the three here rather than the least: this
        // is a LONG poll driven by the screen's `.task`, so the swallowed-
        // refusal loop is continuous rather than event-driven.
        //
        // A GATE rather than a disposal, because `pollGroup` is re-entered from
        // many call sites (the screen's loop, the Refresh button, every
        // membership action, the Welcome intake) and there is no single timer
        // to invalidate. It also stops the tail sweep and the ADR-0040 outbox
        // sweep, both of which publish commits an inactive group cannot make.
        //
        // The cursor is deliberately left where the last applied row put it
        // (§6.2.13): it costs nothing, and whether a rejoin wants it reset is
        // the rejoin path's question, not this one's.
        if membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) {
            // This journal will never be applied again, so any own-echo
            // registration still outstanding for it can never be consumed.
            // Drop them here rather than carrying them for the life of the
            // process: a device that committed its own leave registers that
            // commit's row and then stops polling one line later, which is
            // exactly how such an entry is made. Idempotent, so running it on
            // every gated poll costs a set filter.
            chatClient.forgetOwnPublished(groupIdHex: groupIdHex)
            return
        }
        // ADR-0073 R.5 "the old segment after the move" — an older segment
        // of a one-to-one is no longer polled (its journal has no sender left
        // but the superseded identity); an older segment of a group is, for
        // the DS retention window, since members who have not moved yet
        // still write there. A GATE for the same reason as the one above.
        // The continuity pass is still offered from here: an old one-to-one
        // is exactly the group whose own poll cannot resume an interrupted
        // build.
        guard journalPolled(groupIdHex: groupIdHex) else {
            continueAllSoon(force: false)
            return
        }
        // ADR-0040 (G-35) — sweep the durable pending-commit outbox ONCE per
        // group per session, BEFORE the first poll. Re-publishing a crashed
        // mid-window commit re-arms the own-echo filter for the re-published
        // commit's messageId, so the apply below drops it instead of feeding it
        // to `processIncoming` (which would raise `CannotDecryptOwnMessage`).
        // The guard-insert is synchronous + on the main actor, so the sweep runs
        // at most once even under overlapping poll tasks; an interleaving poll's
        // own-commit re-fetch is still a benign drop-and-continue in
        // `ChatClient.applyFetchedMessages`.
        //
        // G-38 — deliberately OUTSIDE the commit lock. The sweep publishes
        // commits of its own, and ``withCommitLock(groupIdHex:_:)`` is not
        // reentrant, so running it inside would deadlock against the apply's
        // own acquisition below.
        // This poll is the bound account's. After each wait below it stops
        // once that account has left: the batch it fetched, and the tail work
        // (claim, sweep, re-request, handover, re-admission), are not carried
        // into whoever signed in meanwhile, even when that account is in the
        // same group.
        let bound = binding.capture()
        if !sweptGroupsThisSession.contains(groupIdHex) {
            sweptGroupsThisSession.insert(groupIdHex)
            // Shielded from the caller's cancellation. This poll may be the
            // chat screen's `.task`, cancelled when the person leaves; the
            // re-publish below runs outside the commit chain, so a cancelled
            // caller would stop it with the entry un-acked, and the guard
            // above lets the sweep run only once per session. A commit this
            // device is durably past, and that never reached the DS, would
            // then stay unpublished until the next launch. The order of the
            // sweep and what it does are unchanged; only who may stop it is.
            await Cancellation.shielded { [self] in
                await sweepPendingCommits(record: record, groupIdHex: groupIdHex)
            }
            guard bound.isCurrent else { return }
        }
        // G-38 step 2 — the NETWORK half, outside the lock. Both values are read
        // here and re-validated under the lock; `groupId` is the record's cached
        // copy, so the fetch reads no `Group` handle at all.
        let after = record.cursor
        let groupId = record.groupId
        // ADR-0053 point 7 — an owner / admin device reads what non-members
        // knocked, next to the journal and on its own cursor, at most once per
        // `KnockPollThrottle.floorSeconds` per group (the journal is polled far
        // more often than anyone knocks). Not awaited with the long poll: a
        // knock waiting for approval must not sit behind 30 s of an idle
        // journal.
        if knockThrottle.take(groupIdHex) {
            Task { @MainActor [weak self] in await self?.pollKnocks(groupIdHex: groupIdHex) }
        }
        // ADR-0053 point 1 — the read is signed with the group's access key.
        let raw = try await chatClient.fetchGroupJournal(
            groupId: groupId,
            after: after,
            waitSecs: waitSecs,
            auth: dsAuth(groupIdHex: groupIdHex)
        )
        guard bound.isCurrent else { return }
        // G-38 steps 3+4 — the MLS half, inside the per-group commit chain.
        try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            applyFetchedBatch(groupIdHex: groupIdHex, after: after, raw: raw)
        }
        guard bound.isCurrent else { return }
        // ADR-0057, live walk 2026-09-29 — the owner claims the journal on
        // open, once per session, then re-checks its registrars against the
        // roster (obligation 3). Not left to the lazy claim: an UNCLAIMED
        // journal never refuses a signed read, so a `401`-triggered claim
        // never ran and a journal whose claim the DS dropped stayed unclaimed,
        // refusing every epoch registration. Only once the poll reached the
        // journal head, so the claim names the epoch the other members are at
        // (a claim at an older epoch would lock the members who are ahead
        // out). Idempotent for the same root; a `409` is left alone (this
        // device is behind), never answered with a reset.
        if !raw.hasMore, !claimedGroupsThisSession.contains(groupIdHex) {
            await ensureOwnerClaimOnOpen(groupIdHex: groupIdHex)
            guard bound.isCurrent else { return }
            if claimOnOpen.isSettled(journalIdHex: groupIdHex) {
                claimedGroupsThisSession.insert(groupIdHex)
            }
        }
        // ADR-0045 — the sweep trigger. This is the ONLY thing that makes a
        // waiting request ever end, so it runs at the tail of every poll rather
        // than only when a proposal was seen: a verdict owed from a previous run
        // (or from a session this device already closed) has to be finished too,
        // and nothing in the batch announces one.
        //
        // Cheap when there is nothing to do: `sweepIfCommitter` short-circuits
        // synchronously — before any network — when this device is not a
        // candidate, or when nothing is pending and no verdict is owed.
        //
        // Deliberately AFTER the cursor has settled, and deliberately OUTSIDE
        // the lock above (the chain is not reentrant; the sweep takes it
        // itself). A sweep failure is not a poll failure, so it never throws
        // out of here.
        await sweepIfCommitter(groupIdHex: groupIdHex)
        guard bound.isCurrent else { return }
        // ADR-0050 2.5a — a commit applied by this poll (or merged by the sweep
        // just above) may have dropped an own request. Only schedules; does
        // nothing unless the epoch moved since the last evaluation.
        scheduleSupersededReRequest(groupIdHex: groupIdHex)
        // ADR-0060 section 6 — an own Add still waiting may need a fresh
        // committer envelope. Only schedules; does nothing while none is due.
        Task { @MainActor [weak self] in
            await self?.resendCommitterEnvelopesIfDue(groupIdHex: groupIdHex)
        }
        // ADR-0057 S2 finding 1 — a root handover this account still owes (the
        // new owner's key did not resolve yet, a send failed, or it is owed
        // from an earlier session) is retried from the poll tail, no sooner
        // than its retry interval. Only schedules.
        scheduleRootHandover(groupIdHex: groupIdHex)
        // ADR-0050 section 2.4 — the scheduled re-admission check, bounded per
        // chat. Not awaited by the poll: it reads the directory.
        Task { @MainActor [weak self] in
            await self?.readmitDevicesIfDue(groupIdHex: groupIdHex)
        }
    }

    /// G-38 — the locked half of ``pollGroup(groupIdHex:waitSecs:)``.
    ///
    /// Runs ONLY under ``withCommitLock(groupIdHex:_:)``, and is
    /// **non-suspending by construction**: every call it makes is synchronous
    /// (`ChatClient.applyFetchedMessages` is deliberately not `async`, and both
    /// push-routing follow-ups only SPAWN a detached `Task` and return). That
    /// makes the whole batch — the membership baseline, the MLS apply, the
    /// cursor advance and the snapshot — one atomic `@MainActor` region, so no
    /// other commit can observe it half-applied and no network wait can be
    /// reintroduced into it without a compile error.
    ///
    /// Re-reads `canonicalIdentity` and `groups` rather than trusting anything
    /// captured before the wait — a predecessor may have REPLACED the
    /// `GroupRecord` or evicted it entirely.
    private func applyFetchedBatch(
        groupIdHex: String,
        after: Int64,
        raw: FetchResult
    ) {
        guard let identity = canonicalIdentity else { return }
        guard let record = groups[groupIdHex] else { return }
        // ADR-0087 section 9 — a batch fetched before an end was heard.
        guard !isEnding(groupIdHex: groupIdHex) else { return }
        // G-38 — the cursor-moved guard. Another poll of this group applied a
        // batch while this one was parked on the network, so the rows in hand
        // start behind the live cursor. Drop them WITHOUT applying and WITHOUT
        // touching the cursor; the next poll re-fetches from where the record
        // actually is. Applying anyway would re-feed consumed rows to OpenMLS
        // and, worse, wind the cursor BACKWARD to this stale batch's end.
        guard record.cursor == after else { return }
        // ADR-0043 — baseline for the membership diff, captured BEFORE the
        // poll applies any commit. A Commit carries no membership delta out
        // of the poll loop, so a removal committed by SOMEONE ELSE is
        // inferred from this snapshot against the post-poll one. This
        // device's OWN removals never show up here (they were applied
        // locally before this poll, so the baseline is already
        // post-removal); those rotate directly in `removeMemberFromGroup`.
        //
        // G-38 — still captured INSIDE the lock, immediately before the apply.
        // Hoisting it out to where the fetch now runs would race the shrink diff
        // against any commit landing in the fetch window.
        let membersBeforePoll = PushGroupRouting.memberSignatureKeys(group: record.group)
        let leafKeysBeforePoll = Self.leafKeysB64(group: record.group)
        // Membership rows — the people before the batch, and where the rows it
        // files start.
        let personKeysBeforeBatch = Self.personKeysB64(of: record.group)
        let firstNewRow = record.messages.count
        // ADR-0057 — the epoch BEFORE the batch, so a batch that moved it
        // (a membership or role change) re-checks the journal's registrars.
        let epochBeforeBatch = try? record.group.epoch()
        // ADR-0057 S2 finding 1 — the owner before this batch, so a remote
        // commit that carried this account's ownership transfer is seen.
        let ownerBeforeBatch = (try? record.group.groupRoles())?.owner
        let result = chatClient.applyFetchedMessages(
            identity: identity,
            group: record.group,
            // The own-echo filter is per journal, so the apply must name the
            // journal this batch came off: a DS id is assigned per group.
            groupIdHex: groupIdHex,
            raw: raw,
            after: after
        )
        // ADR-0073 — two silent drops before anything is counted or drawn: a
        // chat row whose sender is a key an accepted reset retired (a removed
        // device still holding the old keys), and a founder's re-send of a
        // message this device already holds in an older segment (rule 6: it
        // is shown once, where it was).
        let events = silentlyFiltered(
            result.events, resendOrigins: result.resendOrigins,
            eventSenders: result.eventSenders, groupIdHex: groupIdHex)
        noteAppliedForUnread(events, groupIdHex: groupIdHex)
        matchDisplayCopies(events, groupIdHex: groupIdHex, ownAuthorKey: AuthorIdentity.own(group: record.group))
        record.consumeEvents(
            events,
            ownAuthorKey: AuthorIdentity.own(group: record.group),
            callSignalingSink: { [weak self] messageId, payload, createdAt, senderLeaf, senderDeviceKey, senderKey in
                self?.routeCallSignaling(
                    groupIdHex: groupIdHex, messageId: messageId, payload: payload,
                    createdAt: createdAt, senderLeaf: senderLeaf,
                    senderDeviceKey: senderDeviceKey, senderKey: senderKey)
            },
            readReceiptSink: { [weak self] sender, upTo in
                self?.recordReadReceipt(groupIdHex: groupIdHex, sender: sender, upTo: upTo)
            },
            chatStatusSink: { [weak self] status in
                self?.chatStatusSink?(groupIdHex, status)
            },
            pushRoutingSecretSink: { [weak self] secret, mintEpoch, senderLeaf in
                // ADR-0042 — owner-gate, then persist the secret + subscribe
                // under `push_gid`. ADR-0044 — plus the recency order.
                // Fire-and-forget (it only spawns a `Task`), so the routing
                // chain it eventually takes is never awaited while the commit
                // chain is held — the two locks are never nested.
                self?.receivePushRoutingSecret(
                    groupIdHex: groupIdHex, secret: secret, mintEpoch: mintEpoch,
                    senderLeaf: senderLeaf)
            },
            // ADR-0045 — WHO (the MLS-authenticated sender leaf, resolved to a
            // signature KEY here at receipt while the roster is authenticated in
            // the epoch the beacon was sealed at) and HOW OLD (the DS row's
            // server-assigned stamp). Synchronous, so the apply region stays
            // non-suspending.
            committerBeaconSink: { [weak self] senderLeaf, createdAt in
                self?.recordCommitterBeacon(
                    groupIdHex: groupIdHex, senderLeaf: senderLeaf, createdAt: createdAt)
            },
            membershipProposalSink: { [weak self] record, createdAt in
                self?.recordPendingRequestObservation(
                    groupIdHex: groupIdHex, record: record, createdAt: createdAt)
            },
            profileSink: { [weak self] claim, senderKey in
                guard let self else { return }
                self.receivedProfileSink(claim, senderKey, self.currentAccountIdB64)
                self.keepProfileGrants(claim, senderKey: senderKey, groupIdHex: groupIdHex)
            },
            // ADR-0051 — role-gate against the live group, order, store,
            // re-title. Fire-and-forget (it only spawns a `Task`), so the name
            // chain it takes is never awaited while the commit chain is held.
            groupNameSink: { [weak self] name, updatedAtMs, senderLeaf in
                self?.receiveGroupName(
                    groupIdHex: groupIdHex, name: name, updatedAtMs: updatedAtMs,
                    senderLeaf: senderLeaf)
            },
            // ADR-0048 §2 trigger 4 — only SCHEDULES a throttled republish, so
            // the lock held here is never awaited against itself.
            profileRequestSink: { [weak self] senderKey in
                self?.answerProfileRequest(groupIdHex: groupIdHex, senderKey: senderKey)
            },
            groupContactRequestSink: { [weak self] envelope, senderKey in
                self?.onGroupContactRequest?(groupIdHex, envelope, senderKey)
            },
            // ADR-0052 decision 3 — standing check against the live group, the
            // per-message order, store. Fire-and-forget (it only spawns a
            // `Task`), so the pin chain it takes is never awaited while the
            // commit chain is held.
            pinSink: { [weak self] op, senderLeaf in
                self?.receivePin(groupIdHex: groupIdHex, op: op, senderLeaf: senderLeaf)
            },
            // ADR-0057 obligation 4 — synchronous gate + store.
            registrarSeedSink: { [weak self] plaintext, senderLeaf in
                self?.receiveRegistrarSeed(
                    groupIdHex: groupIdHex, plaintext: plaintext, senderLeaf: senderLeaf)
            },
            scopedDiscoverySink: { [weak self] envelope, senderKey in
                self?.receiveScopedDiscovery(
                    groupIdHex: groupIdHex, envelope: envelope, senderKey: senderKey)
            },
            // ADR-0059 section 6 — filed per member under the hub's own chain.
            // Fire-and-forget (it only spawns a `Task`), so that chain is never
            // awaited while the commit chain is held.
            sharedChannelsSink: { [weak self] list, senderKey in
                self?.sharedChannels.receive(groupIdHex: groupIdHex, list: list, senderKey: senderKey)
            },
            // ADR-0073 R.6 — a successor claim: judged synchronously against
            // the live group (the sender and the admin set are those of the
            // roster right after the apply) and this device's own accepted
            // resets. Never drawn.
            groupClosedSink: { [weak self] messageId, successorGroupId, senderLeaf in
                self?.receiveGroupClosed(
                    groupIdHex: groupIdHex, messageId: messageId,
                    successorGroupId: successorGroupId, senderLeaf: senderLeaf)
            },
            // ADR-0087 — fire-and-forget (it only spawns a `Task`), so the
            // name chain it takes is never awaited under the commit chain.
            groupSettingSink: { [weak self] setting, senderLeaf in
                self?.receiveGroupSetting(groupIdHex: groupIdHex, setting: setting, senderLeaf: senderLeaf)
            },
            reactionToOwnSink: { [weak self] messageId in
                self?.noteUnseenReaction(groupIdHex: groupIdHex, messageId: messageId)
            }
        )
        // Membership rows — before the log is written below.
        noteMembershipChange(
            groupIdHex: groupIdHex, record: record, events: events,
            personKeysBefore: personKeysBeforeBatch, firstNewRow: firstNewRow)
        // THE ORDERING RULE — persist first, advance the cursor second, and hold
        // the cursor when the write does not land. See `Chat/DurableCursor.swift`
        // for why the cursor may lag the durable MLS state but must never lead
        // it: since ADR-0045 a proposal applied here but not persisted, with the
        // cursor already past it, makes the commit that carries it by reference
        // permanently unappliable and strands this device at epoch N forever.
        //
        // The persist is the FAIL-CLOSED `persistSnapshotDurableOrThrow`, not
        // the swallowing `persistSnapshotBestEffort` this used to call.
        applyDurableCursorBatch(
            gate: cursorGate,
            touchedMlsState: result.touchedMlsState,
            // Strong `self`: `commit`'s closures are non-escaping, so this is no
            // cycle — and `[weak self]` plus optional chaining would silently
            // resolve to "the write succeeded" when it never ran at all, which
            // is the one direction this gate must never fail in.
            persist: { try self.persistSnapshotDurableOrThrow() },
            advance: { record.advanceCursor(to: result.cursor) }
        )
        // The plaintext timeline is written either way — held or advanced — and
        // deliberately AFTER the gate so it records whatever cursor the gate
        // left. It is not MLS state: a stale timeline costs a re-render, never a
        // strand. Skipped for a batch that produced nothing, as before, so an
        // idle long poll still writes no file.
        if !result.events.isEmpty { persistMessageLogBestEffort() }
        state = state.with(threads: buildThreadList())
        // ADR-0057 S2 finding 1 — the ownership moved away from this account in
        // this batch: hand the journal root over (only schedules).
        noteOwnerTransition(
            groupIdHex: groupIdHex, before: ownerBeforeBatch,
            after: (try? record.group.groupRoles())?.owner)
        // ADR-0057 — no heal-on-apply: a member never registers an epoch it did
        // not commit. When the batch moved the epoch, the OWNER re-checks the
        // registrars against the new roster (a join, a promotion, a removal).
        // Only spawns a `Task`, so the apply region stays non-suspending.
        if let epochBeforeBatch, let epochAfter = try? record.group.epoch(),
           epochAfter != epochBeforeBatch {
            reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
            // The roster may have changed: the notification extension's
            // person-name mirror follows it (it writes only on a change).
            mirrorCallPeerNames()
        }
        // ADR-0052 decision 3 — a delete that landed in this batch takes its
        // shared pin record with it. Only spawns a `Task` on the pin chain.
        if !result.events.isEmpty { dropSharedPinsOfDeletedMessages(groupIdHex: groupIdHex) }
        // ADR-0043 — did someone else's Remove commit just land? Then the
        // evicted member still holds this group's push_routing_secret and
        // push-gw is still waking it. Driven off an ACTUAL before/after
        // member-set comparison, never off a "a poll happened" tick.
        // Fire-and-forget, same non-nesting argument as the sink above.
        rotatePushRoutingSecretIfMembershipShrank(
            groupIdHex: groupIdHex, previousMemberKeys: membersBeforePoll)
        // ADR-0050 — remember the leaves that left, so the scheduled
        // re-admission never puts back a device someone removed.
        if let accountIdB64 = currentAccountIdB64 {
            DeviceReadmissionMemory.recordRemoved(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                before: leafKeysBeforePoll, after: Self.leafKeysB64(group: record.group))
        }
        // ADR-0048 §2 — and did someone else's Add land? Same before/after
        // roster comparison, other direction. Only schedules.
        noteProfileGrowthIfMembershipGrew(
            groupIdHex: groupIdHex, previousMemberKeys: membersBeforePoll)
        // ADR-0073 R.6 / R.7 — the continuity pass is offered from the tail
        // of every applied batch (an own claim waits for the journal to be
        // read up to it; a kept claim is judged again). Only schedules.
        // Forced only when the journal moved AND a reset was ever accepted
        // here (an own claim waits for exactly this cursor); otherwise the
        // pass keeps its interval.
        continueAllSoon(force: result.cursor > after && !supersededKeyMap().isEmpty)
    }

    /// The app-level ``CallHost``'s inbound 1x1 call-signaling feed. Claimed
    /// once, by the composition root (``CallHost/attach(to:)``) — never by a
    /// chat view model, which is what used to make an incoming call ring only
    /// while that chat was on screen. `nil` when no host is attached; the row is
    /// then dropped.
    ///
    /// Only ever handed a body ``CallSignalingRoute`` allows through (C-11):
    /// offer / answer / candidate, and `CallEnd`, which the host drops unless it
    /// names the active call on the group that call lives in (C-12). Group-control
    /// bodies never reach it — they fold into ``groupCalls`` instead.
    ///
    /// Called synchronously, in journal order, from inside the non-suspending
    /// apply region of every receive path; the handler must not suspend.
    var callSignalingSink: ((InboundCallRow) -> Void)?

    /// Every `CallEnd` a group's journal delivers, for the app-level
    /// ``GroupCallHost`` (`docs/ux/calls.md` section 4.1.2): the call it
    /// names, its reason, the row's MLS sender resolved to the person at
    /// receipt (C-68), and who this device recorded as that call's starter —
    /// read BEFORE the row closes the record. The host takes the device out of
    /// a call only when ``EndForEveryone/endsTheCallForMe(_:)`` says so; for
    /// the record itself an end is what it always was.
    ///
    /// Called synchronously, in journal order, like ``callSignalingSink``; the
    /// handler must not suspend. `nil` when no host is attached.
    var groupCallEndSink: ((ReceivedGroupCallEnd) -> Void)?

    /// Per-chat ongoing group call, keyed by hex group id, as observed on the
    /// group journal (`GroupCallStart` / `GroupCallJoin` / `GroupCallLeave` /
    /// `CallEnd`).
    ///
    /// Held HERE rather than on a view model so it is fed by every receive
    /// path — the open chat's long poll and the foreground receive of every
    /// other group alike. Persisted per account (``groupCallStore``) and
    /// reloaded at bootstrap: the DS poll resumes from its cursor, so a record
    /// held only in memory was lost on relaunch and the chat then offered to
    /// start a second call beside a running one. The participant heartbeat
    /// (``GroupCallHost``) keeps a reloaded record honest: a call whose
    /// participants stopped renewing their joins stops being live on its own.
    @Published private(set) var groupCalls: [String: OngoingGroupCall] = [:]

    /// Classify one decrypted call-signaling payload and deliver it.
    ///
    /// Group-control bodies update ``groupCalls``; everything the 1x1 path may
    /// see goes to ``callSignalingSink``. The split is ``CallSignalingRoute``,
    /// so the C-11 rule lives in one tested place rather than in each sink.
    ///
    /// `senderLeaf` is the MLS sender leaf of the journal row that carried the
    /// payload. A group-control body naming a different leaf is dropped
    /// (``GroupCallSignal/speaksFor(senderLeaf:)``); it never reaches the
    /// record. `senderDeviceKey` / `senderKey` are that leaf resolved at receipt
    /// to its own signature key and its person key; a join or leave whose
    /// device did not resolve is dropped by the reducer (C-68: a leaf index is
    /// never a participant's identity).
    ///
    /// This is the ONE funnel every receive path reaches (the poll's apply and
    /// the ADR-0045 sweep's verdict read both call it, and the foreground
    /// receive of non-open groups is the same poll), so the leaf check cannot be
    /// skipped by arriving another way.
    func routeCallSignaling(
        groupIdHex: String,
        messageId: Int64,
        payload: Data,
        createdAt: String,
        senderLeaf: UInt32?,
        senderDeviceKey: MemberIdentity?,
        senderKey: MemberIdentity?
    ) {
        let body: FfiCallSignalingBody
        do {
            body = try parseCallSignaling(plaintext: payload)
        } catch {
            NSLog("parseCallSignaling failed: %@", error.localizedDescription)
            return
        }
        // ADR-0083 section 4: a one-to-one share state is accepted only when
        // its signer is the row's MLS sender leaf and it verifies against this
        // conversation's group; anything else is dropped here, before routing.
        var verifiedScreenShare: VerifiedCallScreenShare?
        if case .screenShare(let callId, let state) = body {
            guard let group = groups[groupIdHex]?.group,
                let verified = Self.verifiedOneToOneScreenShare(
                    group: group, callId: callId, state: state, senderDeviceKey: senderDeviceKey)
            else {
                NSLog("screen-share state dropped: not verified against the MLS sender")
                return
            }
            verifiedScreenShare = verified
        }
        let route = CallSignalingRoute.of(body)
        // Read before the record is closed below: who started the call this
        // end names. The sender is the row's MLS sender, never a field of the
        // body; the fallback to the device key is the one the starter was
        // recorded with.
        var receivedEnd: ReceivedGroupCallEnd?
        if case .end(let callId, let reason) = body {
            let record = groupCalls[groupIdHex]
            receivedEnd = ReceivedGroupCallEnd(
                groupIdHex: groupIdHex,
                callId: callId,
                end: EndForEveryone.ReceivedEnd(
                    reason: reason,
                    senderPersonKey: senderKey ?? senderDeviceKey,
                    starterPersonKey: record?.callId == callId ? record?.initiatorPersonKey : nil))
        }
        if let signal = route.groupSignal {
            // Printed for EVERY group-control row that reaches this device, before
            // any of the four gates below can drop it. Its absence therefore means
            // the row never arrived (a poll that did not run, or a cursor already
            // past it) rather than a row that arrived and was refused — two
            // different bugs that were previously indistinguishable, because a
            // dropped row and an unfetched row both printed nothing.
            NSLog(
                "group-call row received kind=%@ leafOk=%@ rowMs=%@",
                GroupCallSignalLog.kind(signal),
                signal.speaksFor(senderLeaf: senderLeaf) ? "true" : "false",
                Self.parseDsTimestampMs(createdAt).map { "\($0)" } ?? "unparseable")
        }
        if let signal = route.groupSignal, !signal.speaksFor(senderLeaf: senderLeaf) {
            NSLog("group-call signal dropped: claimed leaf does not match the MLS sender")
        } else if let signal = route.groupSignal {
            let rowMs = Self.parseDsTimestampMs(createdAt)
            if case .start = signal, rowMs == nil {
                // The start time is bounded by the server's row stamp; with no
                // readable stamp there is nothing to bound it by, so the
                // announce is dropped rather than trusted on the sender's word
                // or on a local clock.
                NSLog("group-call start dropped: unparseable DS created_at")
            } else {
                applyGroupCallSignal(
                    groupIdHex: groupIdHex,
                    signal: signal,
                    participant: senderDeviceKey.map {
                        GroupCallParticipant(deviceKey: $0, personKey: senderKey ?? $0)
                    },
                    atMs: rowMs ?? Self.nowMs()
                )
            }
        }
        if let receivedEnd { groupCallEndSink?(receivedEnd) }
        if route.forwardToOneToOne, case .offer = body,
            Self.isOwnAccountSender(
                senderKey: senderKey,
                ownKey: groups[groupIdHex].flatMap { AuthorIdentity.own(group: $0.group) }) {
            // ADR-0050 — this account's other device placed this call from
            // its own leaf in the one-to-one. It is that device's outgoing
            // call, never an incoming one here: ringing it rang the caller's
            // own second phone.
            NSLog("call offer from this account's other device: not rung")
        } else if route.forwardToOneToOne {
            callSignalingSink?(
                InboundCallRow(
                    groupIdHex: groupIdHex, messageId: messageId, createdAt: createdAt, body: body,
                    verifiedScreenShare: verifiedScreenShare))
        }
    }

    /// Did this account send the row? Only when both keys are known: an
    /// unresolved sender or own key counts as someone else, so a resolution
    /// failure never silences a real incoming call.
    nonisolated static func isOwnAccountSender(
        senderKey: MemberIdentity?, ownKey: MemberIdentity?
    ) -> Bool {
        guard let senderKey, let ownKey else { return false }
        return senderKey == ownKey
    }

    /// `verifyCallScreenShare` for a one-to-one call (ADR-0083 section 4): the
    /// fixed peer id `p2p`, and the row's MLS sender leaf (base64, resolved at
    /// receipt) as the key the state must be signed by. `nil` on any refusal,
    /// and when the sender did not resolve.
    static func verifiedOneToOneScreenShare(
        group: Group, callId: Data, state: Data, senderDeviceKey: MemberIdentity?
    ) -> VerifiedCallScreenShare? {
        guard let senderDeviceKey, let signer = Data(base64Encoded: senderDeviceKey),
            signer.count == 32
        else { return nil }
        return try? verifyCallScreenShare(
            group: group, callId: callId, peerId: callScreenShareOneToOnePeerId(),
            peerSignerKey: signer, state: state)
    }

    /// Fold one group-call signal from `participant` into ``groupCalls``, and
    /// persist the per-account records when the chat's record changed.
    func applyGroupCallSignal(
        groupIdHex: String, signal: GroupCallSignal, participant: GroupCallParticipant?, atMs: Int64
    ) {
        let before = groupCalls[groupIdHex]
        let next = GroupCallReducer.apply(
            signal, participant: participant, to: before, rowCreatedAtMs: atMs)
        // What the reducer did with it. The two silent drops this makes visible
        // are the ones that turn a live call into no banner at all:
        //   * C-12 — a Join for a call id this device does not track is dropped,
        //     so a device that MISSED THE START can never be brought into the
        //     record by any number of later heartbeats;
        //   * C-68 — a Join whose sender leaf did not resolve to a device key is
        //     dropped, because a leaf index is not an identity.
        // Either one leaves `joined` empty, and an empty `joined` is never live.
        // The stored record's OWN call id is printed, not merely whether one
        // exists. The first version of this line said `hadRecord=true` and
        // stopped there, and a live run read that as "the announce for the
        // incoming call had been applied" — when what it actually meant was
        // "some record is here", and the record was a different, dead call.
        // A log that answers a narrower question than the reader thinks is
        // worse than no log.
        NSLog(
            "group-call fold kind=%@ participant=%@ record=%@ joined=%d -> %@ joined=%d",
            GroupCallSignalLog.kind(signal),
            participant == nil ? "unresolved" : "resolved",
            before.map { GroupCallSignalLog.shortId($0.callId) } ?? "none",
            before?.joined.count ?? 0,
            next.map { GroupCallSignalLog.shortId($0.callId) } ?? "none",
            next?.joined.count ?? 0)
        guard next != groupCalls[groupIdHex] else { return }
        groupCalls[groupIdHex] = next
        if let accountIdB64 = currentAccountIdB64 {
            groupCallStore.save(accountIdB64: accountIdB64, records: groupCalls)
        }
    }

    /// This device's OWN group-call signal, which MLS never echoes back to its
    /// sender: folded under this device's own device and person keys, resolved
    /// through the roster exactly like a received row's sender.
    func applyOwnGroupCallSignal(groupIdHex: String, signal: GroupCallSignal, atMs: Int64) {
        let device = ownDeviceKeyB64(groupIdHex: groupIdHex)
        applyGroupCallSignal(
            groupIdHex: groupIdHex, signal: signal,
            participant: device.map {
                GroupCallParticipant(
                    deviceKey: $0, personKey: ownSignatureKeyB64(groupIdHex: groupIdHex) ?? $0)
            },
            atMs: atMs)
    }

    /// This device's own leaf signature key in `groupIdHex` (base64) — the
    /// DEVICE, not the person. What the group-call record keys this device on.
    func ownDeviceKeyB64(groupIdHex: String) -> MemberIdentity? {
        guard let record = groups[groupIdHex],
              let leaf = try? record.group.ownLeafIndex() else { return nil }
        return AuthorIdentity.resolveDevice(group: record.group, leafIndex: leaf)
    }

    /// ADR-0024 — closure the chat view model attaches to receive
    /// inbound typing / read-receipt status events for a group (by
    /// hex id). These render no bubble + never touch the timeline;
    /// the view model folds them into ephemeral typing state +
    /// a monotone per-leaf read watermark. `nil` when no chat
    /// surface is attached; the per-record poll just drops them.
    public var chatStatusSink: ((String, ChatStatusEvent) -> Void)?

    /// ADR-0024 — the read watermarks this device holds for `groupIdHex`
    /// (sender signature key -> highest messageId read), loaded from the
    /// durable store when the mirror has no entry yet. Empty with no signed-in
    /// account. Deliberately does not write the published mirror: a view
    /// model calls this from its initializer, which can run inside a view
    /// update.
    public func readWatermarks(groupIdHex: String) -> PresenceOps.ReadWatermarkMap {
        if let cached = readWatermarksByGroup[groupIdHex] { return cached }
        guard let accountIdB64 = currentAccountIdB64 else { return [:] }
        return readWatermarkStore.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// ADR-0024 — fold one inbound read receipt into the durable store and the
    /// published mirror, advance-only. Called from the receive path for every
    /// group, whichever chat is open.
    func recordReadReceipt(groupIdHex: String, sender: MemberIdentity, upTo: Int64) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        if let next = readWatermarkStore.merge(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, sender: sender, upTo: upTo)
        {
            readWatermarksByGroup[groupIdHex] = next
        } else if readWatermarksByGroup[groupIdHex] == nil {
            readWatermarksByGroup[groupIdHex] = readWatermarkStore.load(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
    }

    // MARK: - Foreground receive support

    /// How many chat screens currently have each group open (a count, not a
    /// flag: SwiftUI can briefly hold an outgoing and an incoming screen for the
    /// same chat during a transition).
    ///
    /// An open chat long-polls itself, so ``ForegroundReceiveScheduler`` skips
    /// it, and what it applies is being read, so it does not count as unread.
    var openChats: [String: Int] = [:]

    /// The log's slots for groups not in ``groups``: a pairing history bundle
    /// lands before the Welcomes do. ``persistMessageLogBestEffort()`` writes
    /// them back, and a join takes its slot
    /// (``joinGroupFromWelcome(welcomeBytes:)``).
    var waitingLogSlots = WaitingLogSlots()

    /// The ids behind ``ChatThread/unread``, per group (``UnreadCounting``).
    var unreadMessageIds: [String: Set<Int64>] = [:]

    /// A chat screen started showing (and long-polling) `groupIdHex`. Clears
    /// the group's unread count.
    func chatScreenOpened(groupIdHex: String) {
        openChats[groupIdHex, default: 0] += 1
        let viewed = groups[groupIdHex]?.markAllViewed() ?? false
        if viewed { persistMessageLogBestEffort() }
        if unreadMessageIds.removeValue(forKey: groupIdHex) != nil || viewed {
            state = state.with(threads: buildThreadList())
        }
    }

    /// Chat folders' Mark all as read (`docs/ux/chat-folders.md` 2.2): the
    /// same local effect opening the chat has on the badge, without opening
    /// it. The cross-device half is the read cursor the caller broadcasts.
    func clearUnread(groupIdHex: String) {
        if unreadMessageIds.removeValue(forKey: groupIdHex) != nil {
            state = state.with(threads: buildThreadList())
        }
    }

    /// The matching close for ``chatScreenOpened(groupIdHex:)``.
    func chatScreenClosed(groupIdHex: String) {
        // What arrived while the chat was open was read there (ADR-0063
        // section 5: the chat marks everything it holds).
        if groups[groupIdHex]?.markAllViewed() == true {
            persistMessageLogBestEffort()
            state = state.with(threads: buildThreadList())
        }
        guard let count = openChats[groupIdHex] else { return }
        if count <= 1 {
            openChats.removeValue(forKey: groupIdHex)
        } else {
            openChats[groupIdHex] = count - 1
        }
    }

    func isChatOpen(groupIdHex: String) -> Bool {
        if openChats[groupIdHex] != nil { return true }
        // ADR-0073 R.5 — the screen is open on the conversation's current
        // group; a row applied in an older segment of it (a member who has
        // not moved yet) is read on that same screen.
        guard let plan = conversationPlan(ofGroup: groupIdHex), plan.heldSegments.count > 1 else {
            return false
        }
        return plan.heldSegments.contains { openChats[$0] != nil }
    }

    /// ADR-0063 section 5 — file `ids` the feed kept on screen as seen in
    /// `groupIdHex`, and move the group's watermark as far as the seen set is
    /// contiguous. `cursorThrough` is the account's cross-device read cursor
    /// for the group, which counts as seen too.
    ///
    /// Returns the new watermark when it moved, else `nil`; the caller sends
    /// the read receipt and the read cursor at it. Never past a message the
    /// person did not see.
    func recordFeedSeen(groupIdHex: String, ids: [Int64], cursorThrough: Int64?) -> Int64? {
        guard let record = groups[groupIdHex] else { return nil }
        let through = [record.lastViewedMessageId, cursorThrough].compactMap { $0 }.max()
        let result = FeedSeen.markSeen(
            rows: record.seenRows,
            through: through,
            feedSeen: record.feedSeen,
            ids: ids,
            contiguous: { contiguousSeen(ids: $0, seen: $1) })
        record.lastViewedMessageId = result.lastViewedMessageId
        record.feedSeen = result.feedSeen
        // The local unread count (used before any read cursor exists) drops
        // the messages newly seen.
        if var unread = unreadMessageIds[groupIdHex] {
            unread.subtract(ids)
            unreadMessageIds[groupIdHex] = unread.isEmpty ? nil : unread
        }
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
        return result.advancedTo
    }

    /// Every group the foreground receive may poll: loaded, and still a member
    /// of. Sorted, so the round robin visits them in a stable order.
    func receivableGroupIds() -> [String] {
        groups.keys
            .filter { !membershipEnded(membershipStateNow(groupIdHex: $0)) }
            .sorted()
    }

    /// The group's journal cursor, or `nil` for an unknown group. The scheduler
    /// compares it across a poll to tell whether the poll consumed rows.
    func journalCursor(groupIdHex: String) -> Int64? {
        groups[groupIdHex]?.cursor
    }

    /// Count what a receive path just applied towards the chat list's unread
    /// badge — unless the chat is open, in which case it is being read.
    private func noteAppliedForUnread(_ events: [IncomingChatEvent], groupIdHex: String) {
        // A row the timeline already holds is not new: a journal read again
        // after a relaunch (the cursor may lag the durable state) re-applies
        // rows that were read before it, and counted them as unread.
        let held = Set(groups[groupIdHex]?.messages.map(\.messageId) ?? [])
        let fresh = held.isEmpty ? events : events.filter { event in
            switch event {
            case .application(let messageId, _, _, _, _, _, _), .unsupported(let messageId, _, _, _):
                return !held.contains(messageId)
            default:
                return true
            }
        }
        let next = UnreadCounting.next(
            current: unreadMessageIds[groupIdHex] ?? [],
            applied: fresh,
            chatOpen: isChatOpen(groupIdHex: groupIdHex),
            syncedThrough: groups[groupIdHex]?.syncedThroughMessageId
        )
        unreadMessageIds[groupIdHex] = next.isEmpty ? nil : next
    }

    /// Pull new messages for every active group. Per-group
    /// failures are swallowed so one bad apple doesn't block the
    /// others.
    public func pollAllGroups(waitSecs: Int = 0) async {
        resumeOwedEnds()
        let groupIds = Array(groups.keys)
        for groupIdHex in groupIds {
            do {
                try await pollGroup(groupIdHex: groupIdHex, waitSecs: waitSecs)
            } catch {
                // best-effort
                continue
            }
        }
    }

    /// ``pollAllGroups(waitSecs:)`` for the feed's pull to refresh
    /// (`docs/ux/feed.md` section 5): the same best-effort pass, reporting
    /// how many groups it polled and how many answered, for the feed's
    /// "Couldn't check for new messages." rule (``FeedRefreshState``).
    func pollAllGroupsReporting() async -> FeedCheckTally {
        resumeOwedEnds()
        let groupIds = Array(groups.keys)
        var tally = FeedCheckTally()
        for groupIdHex in groupIds {
            tally.tried += 1
            do {
                try await pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
                tally.answered += 1
            } catch {
                continue
            }
        }
        return tally
    }

    // MARK: - ADR-0050 device fan-outs

    /// How long ``admitDevice(deviceKey:)`` waits for the new device's
    /// KeyPackage to appear in the directory, and how often it looks. The fresh
    /// device publishes it after it has landed a session.
    static let admitDeviceWaitMs: Int64 = 180_000
    static let admitDevicePollMs: UInt64 = 3_000

    /// What a device fan-out did across the chats.
    public struct DeviceFanOut: Equatable {
        /// A proposal was published or a commit landed.
        public var offered = 0
        /// Nothing to do: already admitted / not present, or the membership ended.
        public var skipped = 0
        /// The chat threw; logged, the rest continued.
        public var failed = 0
    }

    /// Base64 leaf signature keys of `group`'s roster, or `nil` on a torn read.
    nonisolated static func leafKeysB64(group: Group) -> [String]? {
        guard let roster = try? group.members() else { return nil }
        return roster.map { base64Encode($0.signaturePublicKey) }
    }

    /// This account's OTHER devices' KeyPackages, so a group this device
    /// creates includes the person's other devices from the first epoch.
    /// Best-effort: a directory failure yields an empty list, and the
    /// re-admission arm adds the missing devices later.
    ///
    /// ADR-0074 section 2.3 — only devices in the account's DeviceGroup: a
    /// removed device that put a directory row back is not added.
    func ownOtherDevices(identity: Identity) async -> [DeviceKeyPackage] {
        guard let accountId = currentAccountId else { return [] }
        let ownKey = identity.signaturePublicKey()
        do {
            let list = try await discovery.fetchKeyPackages(accountId: accountId) ?? []
            let roster = ownDeviceGroupRosterB64()
            return list.filter {
                $0.deviceId != ownKey && DeviceReadmission.ownDeviceAdmissible(
                    deviceKeyB64: base64Encode($0.deviceId), ownDeviceGroupRoster: roster)
            }
        } catch {
            NSLog("ADR-0050: own devices lookup failed; adding this device only: \(error)")
            return []
        }
    }

    /// ADR-0050 section 2.8 / 2.4 — admit ONE device (by its published
    /// KeyPackage) to a group this device is in. Self-service on the core side:
    /// a member may add a leaf of its own account or of an account already in
    /// the group. `nil` when a leaf with that key is already in the roster.
    ///
    /// ADR-0074 section 2.3 — a device of THIS account is admitted only when
    /// its key is a leaf of the account's DeviceGroup; otherwise `nil`, like
    /// a device already in the roster. Every own-device add path comes
    /// through here (the pairing fan-out, the re-admission arm).
    func admitDeviceToGroup(groupIdHex: String, keyPackage: Data) async throws -> MembershipOutcome? {
        let deviceKey = try keyPackageSignatureKey(keyPackage: keyPackage)
        return try await withCommitLock(groupIdHex: groupIdHex) { [self] in
            // Checked under the lock: a poll may have applied the very commit
            // that admitted this device while we waited.
            guard let record = groups[groupIdHex] else { return nil }
            if let own = AuthorIdentity.own(group: record.group),
               let account = try? keyPackageAccountKey(keyPackage: keyPackage),
               base64Encode(account) == own,
               !DeviceReadmission.ownDeviceAdmissible(
                deviceKeyB64: base64Encode(deviceKey), ownDeviceGroupRoster: ownDeviceGroupRosterB64()) {
                NSLog("ADR-0074: a device of this account that is not in its DeviceGroup was not admitted")
                return nil
            }
            // ADR-0074 part 3 — a device of this account whose binding names
            // another of the account's keys than the one its leaves here are
            // bound to (it minted before the pairing bundle reached it) is
            // not admitted: to every gate it would be another account.
            if let own = AuthorIdentity.own(group: record.group).flatMap({ Data(base64Encoded: $0) }),
               let account = try? keyPackageAccountKey(keyPackage: keyPackage),
               let accountIdB64 = currentAccountIdB64,
               OwnAccountKeys.namesWrongOwnKey(
                keyPackageAccountKey: account, ownKeyInConversation: own,
                currentAccountKey: accountIdentity?.signaturePublicKey(),
                predecessorKeys: OwnAccountKeys.predecessorPublicKeys(accountIdB64: accountIdB64)) {
                NSLog("ADR-0074: a device of this account bound under another of its keys was not admitted")
                return nil
            }
            if (try? record.group.members())?.contains(where: { $0.signaturePublicKey == deviceKey }) ?? false {
                return nil
            }
            if usesProposalPath(groupIdHex: groupIdHex) {
                let outcome = try await proposeAddMemberLocked(
                    groupIdHex: groupIdHex, keyPackageBytes: keyPackage)
                // ADR-0050 2.5a — a device of THIS account is a self-service
                // request whose intent survives a superseding commit. A
                // contact's device (the 2.4 re-admission arm) is re-checked by
                // that arm on its own schedule, so it records nothing here.
                if let own = AuthorIdentity.own(group: record.group),
                   let account = try? keyPackageAccountKey(keyPackage: keyPackage),
                   base64Encode(account) == own {
                    recordMembershipIntent(
                        groupIdHex: groupIdHex, .ownDeviceAdd(deviceKeyB64: base64Encode(deviceKey)))
                }
                return outcome
            }
            // ADR-0081 amendment (2026-10-19) — no direct-commit path: a group
            // with no role table is refused.
            try requireRoleTable(groupIdHex: groupIdHex, op: "add")
            return nil
        }
    }

    /// ADR-0050 section 2.10 — retire ONE device's leaf from a group, the
    /// account's other devices untouched (unlike ``removeMemberFromGroup``,
    /// which removes the person). `nil` when no leaf holds that key.
    func retireDeviceFromGroup(groupIdHex: String, deviceKey: Data) async throws -> MembershipOutcome? {
        guard let record = groups[groupIdHex],
              let leaf = (try? record.group.members())?
                .first(where: { $0.signaturePublicKey == deviceKey })?.leafIndex
        else { return nil }
        let bound = binding.capture()
        let outcome = try await removeOneLeafFromGroup(groupIdHex: groupIdHex, leafIndex: leaf)
        try bound.require()
        // ADR-0050 2.5a — retiring is always a device of this account.
        if case .proposed = outcome {
            recordMembershipIntent(
                groupIdHex: groupIdHex, .ownDeviceRemove(deviceKeyB64: base64Encode(deviceKey)))
        }
        return outcome
    }

    /// Poll the directory until `deviceKey`'s KeyPackage is there, bounded.
    private func awaitDeviceKeyPackage(accountId: Data, deviceKey: Data) async throws -> Data {
        let deadline = Self.nowMs() + Self.admitDeviceWaitMs
        while true {
            if let hit = (try? await discovery.fetchKeyPackages(accountId: accountId))?
                .first(where: { $0.deviceId == deviceKey }) {
                return hit.keyPackage
            }
            if Self.nowMs() >= deadline {
                throw ChatError.argument(
                    "the new device has not published a KeyPackage yet; it will be admitted to your chats later")
            }
            try await Task.sleep(nanoseconds: Self.admitDevicePollMs * 1_000_000)
        }
    }

    /// ADR-0050 section 2.8 — admit a device this device just added to the
    /// DeviceGroup (QR pairing) to EVERY chat this device is in: wait, bounded,
    /// for its conversation KeyPackage, then per chat a self-service Add
    /// proposal (swept here when this device is the acting committer; a
    /// table-less group is refused). A device whose KeyPackage never
    /// appears within the wait is admitted later by the re-admission arm.
    @discardableResult
    public func admitDevice(deviceKey: Data) async throws -> DeviceFanOut {
        guard let accountId = currentAccountId else {
            throw ChatError.argument("admitDevice: no session")
        }
        // The device is this binding's account's own: it is offered to that
        // account's chats only, not to those of an account that signed in
        // while the directory was polled (up to ``admitDeviceWaitMs``).
        let bound = binding.capture()
        let keyPackage = try await awaitDeviceKeyPackage(accountId: accountId, deviceKey: deviceKey)
        try bound.require()
        var tally = DeviceFanOut()
        for groupIdHex in Array(groups.keys).sorted() {
            try bound.require()
            if membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) {
                tally.skipped += 1
                continue
            }
            do {
                guard let outcome = try await admitDeviceToGroup(
                    groupIdHex: groupIdHex, keyPackage: keyPackage) else {
                    tally.skipped += 1
                    continue
                }
                tally.offered += 1
                if case .proposed = outcome {
                    await sweepIfCommitter(groupIdHex: groupIdHex)
                }
            } catch {
                tally.failed += 1
                NSLog("ADR-0050 admit device: chat \(groupIdHex) skipped: \(error)")
            }
        }
        return tally
    }

    /// ADR-0050 section 2.10 — retire a device just removed from the
    /// DeviceGroup from EVERY chat this device is in (its one leaf per chat).
    /// The directory row is NOT deleted here any more:
    /// ADR-0074 section 2.3 deletes it first, with the account statement
    /// (``DeviceRemovalRunner``), because that delete is what closes the
    /// directory.
    @discardableResult
    public func retireDevice(deviceKey: Data) async -> DeviceFanOut {
        var tally = DeviceFanOut()
        // As in ``admitDevice(deviceKey:)``: this binding's chats only.
        let bound = binding.capture()
        for groupIdHex in Array(groups.keys).sorted() {
            guard bound.isCurrent else { return tally }
            if membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) {
                tally.skipped += 1
                continue
            }
            do {
                guard let outcome = try await retireDeviceFromGroup(
                    groupIdHex: groupIdHex, deviceKey: deviceKey) else {
                    tally.skipped += 1
                    continue
                }
                tally.offered += 1
                if case .proposed = outcome {
                    await sweepIfCommitter(groupIdHex: groupIdHex)
                }
            } catch {
                tally.failed += 1
                NSLog("ADR-0050 retire device: chat \(groupIdHex) skipped: \(error)")
            }
        }
        return tally
    }

    /// ADR-0074 section 2.3 — the leaf keys (base64) of the account's own
    /// DeviceGroup, or `nil` when they cannot be read. Wired by the
    /// composition root from the DeviceGroup sync runtime; the default reads
    /// nothing, which admits no own device.
    var ownDeviceGroupRosterProvider: @MainActor () -> [Data]? = { nil }

    private func ownDeviceGroupRosterB64() -> Set<String>? {
        ownDeviceGroupRosterProvider().map { Set($0.map { base64Encode($0) }) }
    }

    /// ADR-0050 section 2.10 — delete THIS device's own directory KeyPackage
    /// with this session's bearer token (Forget this device). Best-effort;
    /// logged on failure. The row is tombstoned for 30 days (ADR-0074), which
    /// is why the wipe that follows also deletes this device's identity seed:
    /// the next setup mints a new device key.
    public func deleteDirectoryKeyPackage(deviceKey: Data) async {
        guard let token = currentSessionToken, !token.isEmpty else { return }
        do {
            try await DiscoveryClient(sessionToken: token).deleteKeyPackage(deviceId: deviceKey)
        } catch {
            NSLog("ADR-0050: directory KeyPackage not deleted: \(error)")
        }
    }

    /// ADR-0050 — this device's own conversation leaf key, or `nil` before
    /// bootstrap. `forgetDevice` deletes its directory row by it.
    public var ownDeviceLeafKey: Data? {
        canonicalIdentity?.signaturePublicKey()
    }

    /// ADR-0050 section 2.4 — the accounts, other than this one, the
    /// re-admission check can look up: `(personKey, accountId)` for every
    /// accepted contact whose account id is known. Wired by the composition
    /// root from the contacts store.
    /// The identity gate the scheduled re-admission arm runs each account
    /// through (ADR-0073 R.2). Injectable for tests.
    var identityGateClient: () -> ContactsClient = { ContactsClient() }

    var readmissionContactsProvider: @MainActor () -> [DeviceReadmission.LookupContact] = { [] }

    /// The pin and verified-binding stores the arm reads its standing from.
    /// Injectable for tests.
    var readmissionPins: () -> PeerPinStore = { PeerPinStore() }
    var readmissionBindings: () -> VerifiedBindingStore = { VerifiedBindingStore() }

    /// ADR-0073 R.7 case 2 — one pass of the repair of `groupIdHex` for a
    /// member whose reset this device accepted: where this account is owner
    /// or admin of a role-table group the old identity did not own, propose
    /// the removal of the old identity's devices and the addition of the new
    /// identity's (each checked to be bound to the re-pinned key), and, as
    /// the owner, move an admin role from the old key to the new one. Every
    /// step is a proposal through the ADR-0045 path; several proposers
    /// compose in one sweep. Nothing is shown and nothing is surfaced on
    /// failure: the next pass (the arm's interval) tries again.
    ///
    /// A one-to-one, an owned group and a group with no role table are not
    /// repaired here (R.7 cases 1, 3 and 4).
    func repairResetMember(
        groupIdHex: String, supersededKeyB64: String, newKeyB64: String, accountId: Data
    ) async {
        guard let record = groups[groupIdHex], let newKey = base64Decode(newKeyB64) else { return }
        let roles = groupRoles(groupIdHex: groupIdHex)
        guard ResetRepair.resetCase(
            isOneToOne: isOneToOne(groupIdHex: groupIdHex),
            ownerKeyB64: roles.map { base64Encode($0.owner) },
            supersededKeyB64: supersededKeyB64) == .repair
        else { return }
        let ownRole: ResetRepair.OwnRole? = {
            switch self.ownRole(groupIdHex: groupIdHex) {
            case .owner?: return .owner
            case .admin?: return .admin
            case .member?: return .member
            case nil: return nil
            }
        }()
        let leaves = try? record.group.members()
        let roster = leaves?.map {
            ResetRepair.PersonLeaf(leafIndex: $0.leafIndex, personKeyB64: base64Encode(PersonKey.of($0)))
        }
        let pendingRemoves: [UInt32] = pendingProposals(groupIdHex: groupIdHex).compactMap {
            if case .remove(let leaf) = $0.kind { return leaf }
            return nil
        }
        let plan = ResetRepair.repairPlan(
            ownRole: ownRole, roster: roster, supersededKeyB64: supersededKeyB64,
            pendingRemoveLeaves: pendingRemoves, removeInFlight: false,
            membershipEnded: membershipEnded(membershipStateNow(groupIdHex: groupIdHex)))
        if let leaf = plan.removeLeaf {
            do {
                // One old leaf per pass (web `repairPlan`), through the
                // single-leaf call: the person-wide remove would also record
                // a membership intent, and a silent repair leaves no row in
                // the pending-requests list.
                _ = try await removeOneLeafFromGroup(groupIdHex: groupIdHex, leafIndex: leaf)
            } catch {
                NSLog("ADR-0073 repair: removal not proposed in \(groupIdHex): \(error)")
            }
        }
        if plan.mayAdd, ResetRepair.mayAddResetIdentity(ownRole) {
            // R.8 "no KeyPackage yet": an empty list proposes nothing and the
            // next pass asks again.
            // The arm ran the gate for this account on this pass.
            let devices = (try? await expectedPersonDevices(
                accountId: accountId, expectedKey: newKey, gateAlreadyRan: true)) ?? []
            let present = Set((try? groups[groupIdHex]?.group.members())?.map { $0.signaturePublicKey } ?? [])
            let removed = currentAccountIdB64.map {
                DeviceReadmissionMemory.removed(accountIdB64: $0, groupIdHex: groupIdHex)
            } ?? []
            noteAddedLeafAccounts(keyPackages: devices.map(\.keyPackage))
            for device in devices
            where !present.contains(device.deviceId) && !removed.contains(base64Encode(device.deviceId)) {
                do {
                    _ = try await admitDeviceToGroup(groupIdHex: groupIdHex, keyPackage: device.keyPackage)
                } catch {
                    NSLog("ADR-0073 repair: add not proposed in \(groupIdHex): \(error)")
                }
            }
        }
        // The owner moves an admin role from the old key to the new one, one
        // role change per pass (C-65), the new key first.
        if let table = groupRoles(groupIdHex: groupIdHex),
           let rosterNow = try? groups[groupIdHex]?.group.members(),
           let step = ResetRepair.adminRestoreStep(
            ownRole: ownRole,
            adminKeysB64: table.admins.map { base64Encode($0) },
            supersededKeyB64: supersededKeyB64,
            newKeyB64: newKeyB64,
            rosterPersonKeysB64: PersonKey.people(rosterNow).map { base64Encode($0) },
            roleChangePending: hasPendingRoleChange(groupIdHex: groupIdHex)) {
            do {
                switch step {
                case .promote(let keyB64):
                    if let key = base64Decode(keyB64) {
                        _ = try await setMemberAdmin(
                            groupIdHex: groupIdHex, memberSignaturePubkey: key, makeAdmin: true)
                    }
                case .demote(let keyB64):
                    if let key = base64Decode(keyB64) {
                        _ = try await setMemberAdmin(
                            groupIdHex: groupIdHex, memberSignaturePubkey: key, makeAdmin: false)
                    }
                }
            } catch {
                NSLog("ADR-0073 repair: role change not proposed in \(groupIdHex): \(error)")
            }
        }
        await sweepIfCommitter(groupIdHex: groupIdHex)
    }

    /// ADR-0050 section 2.4 — re-check the device lists of the accounts in
    /// `groupIdHex` against its roster: propose an Add for a listed device with
    /// no leaf, and — for this account only — a Remove for a leaf whose
    /// directory row is gone. Each chat is checked at most once per
    /// ``DeviceReadmission/intervalMs``. Runs at the tail of the foreground
    /// poll.
    func readmitDevicesIfDue(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64,
              let accountId = currentAccountId,
              let record = groups[groupIdHex],
              !membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
              let ownPerson = AuthorIdentity.own(group: record.group),
              DeviceReadmissionMemory.takeSlot(groupIdHex: groupIdHex, nowMs: Self.nowMs()),
              let leaves = try? record.group.members()
        else { return }
        let roster = leaves.map {
            DeviceReadmission.RosterLeafKeys(
                leafKeyB64: base64Encode($0.signaturePublicKey),
                personKeyB64: base64Encode(PersonKey.of($0)))
        }
        // ADR-0073 R.2 — the wider lookup: every roster person whose account
        // id this device knows, from a contact row (under the person's current
        // key or one a reset retired) or from the member's own profile. The
        // id from a profile is a CLAIM; the gate below judges the roster key
        // against the directory through the verified binding, and
        // `lookupStanding` decides whether the id is followed to devices.
        let people = roster.map { $0.personKeyB64 }
        let candidates = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64: people,
            ownPersonKeyB64: ownPerson,
            contacts: readmissionContactsProvider(),
            profileAccountIdOf: { ProfileDirectory.shared.accountId(signatureKeyB64: $0) })
        // The roster, the lookups and the plan are this binding's.
        let bound = binding.capture()
        var accounts: [DeviceReadmission.AccountDevices] = []
        do {
            let list = try await discovery.fetchKeyPackages(accountId: accountId) ?? []
            accounts.append(DeviceReadmission.AccountDevices(
                personKeyB64: ownPerson,
                devices: list.map {
                    DeviceReadmission.DirectoryDevice(
                        deviceKeyB64: base64Encode($0.deviceId), keyPackage: $0.keyPackage)
                }))
        } catch {
            accounts.append(DeviceReadmission.AccountDevices(personKeyB64: ownPerson, devices: nil))
        }
        var resetRepairs: [(supersededKeyB64: String, newKeyB64: String, accountId: Data)] = []
        for candidate in candidates {
            // One of the acceptance rule's triggers. A refusal, an
            // inconclusive check and a network failure all end the same way
            // here: nothing is proposed for this person on this pass.
            guard (try? await identityGateClient().checkContactIdentity(
                accountId: candidate.accountId, heldKeyB64: candidate.rosterKeyB64)) != nil
            else { continue }
            let accountIdB64 = base64Encode(candidate.accountId)
            let pin = readmissionPins().readPin(accountIdB64: accountIdB64)
            let standing = DeviceReadmission.lookupStanding(
                rosterKeyB64: candidate.rosterKeyB64,
                pin: pin,
                provenPredecessorsB64: pin.map {
                    provenPredecessorsProvider().predecessors(of: $0.pinnedSignaturePubkeyB64)
                } ?? [],
                binding: pin == nil ? readmissionBindings().read(accountIdB64: accountIdB64) : nil)
            switch standing {
            case .skip:
                continue
            case .resetIdentity:
                if let pin {
                    resetRepairs.append((candidate.rosterKeyB64, pin.pinnedSignaturePubkeyB64, candidate.accountId))
                }
            case .samePerson:
                // The ordinary ADR-0050 arm: a listed device of this person
                // with no leaf. Only devices bound to the roster's key (or
                // its proven chain) are theirs.
                guard let rosterKey = base64Decode(candidate.rosterKeyB64) else { continue }
                do {
                    // The gate for this account ran a few lines up.
                    let list = try await expectedPersonDevices(
                        accountId: candidate.accountId, expectedKey: rosterKey, gateAlreadyRan: true)
                    accounts.append(DeviceReadmission.AccountDevices(
                        personKeyB64: candidate.rosterKeyB64,
                        devices: list.map {
                            DeviceReadmission.DirectoryDevice(
                                deviceKeyB64: base64Encode($0.deviceId), keyPackage: $0.keyPackage)
                        }))
                } catch {
                    accounts.append(DeviceReadmission.AccountDevices(
                        personKeyB64: candidate.rosterKeyB64, devices: nil))
                }
            }
        }
        // Nothing is proposed into the chats of an account that signed in
        // while the directory was read.
        guard bound.isCurrent else { return }
        // ADR-0073 R.7 case 2 — the silent repair of a role-table group for
        // each member whose reset this device accepted.
        for repair in resetRepairs {
            await repairResetMember(
                groupIdHex: groupIdHex, supersededKeyB64: repair.supersededKeyB64,
                newKeyB64: repair.newKeyB64, accountId: repair.accountId)
            guard bound.isCurrent else { return }
        }
        let plan = DeviceReadmission.plan(
            roster: roster,
            ownPersonKeyB64: ownPerson,
            accounts: accounts,
            removed: DeviceReadmissionMemory.removed(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
            ownDeviceGroupRoster: ownDeviceGroupRosterB64())
        guard !plan.admit.isEmpty || !plan.retire.isEmpty else { return }
        var proposed = 0
        for device in plan.admit {
            guard bound.isCurrent else { return }
            do {
                if try await admitDeviceToGroup(groupIdHex: groupIdHex, keyPackage: device.keyPackage) != nil {
                    proposed += 1
                }
            } catch {
                NSLog("ADR-0050 re-admission: add not proposed: \(error)")
            }
        }
        for leafKeyB64 in plan.retire {
            guard bound.isCurrent else { return }
            guard let key = base64Decode(leafKeyB64) else { continue }
            do {
                if try await retireDeviceFromGroup(groupIdHex: groupIdHex, deviceKey: key) != nil {
                    proposed += 1
                }
            } catch {
                NSLog("ADR-0050 re-admission: retire not proposed: \(error)")
            }
        }
        guard bound.isCurrent else { return }
        if proposed > 0 {
            NSLog("ADR-0050 re-admission: \(plan.admit.count) to admit, \(plan.retire.count) to retire in \(groupIdHex)")
            await sweepIfCommitter(groupIdHex: groupIdHex)
        }
    }

    /// ADR-0050 section 2.9 — the hex ids of every group this device holds,
    /// for the pairing state bundle.
    public var knownGroupIdHexes: [String] {
        Array(groups.keys).sorted()
    }

    /// ADR-0073 R.5 — one entry per CONVERSATION, the group it currently is.
    /// What a screen that lists or looks up chats reads: an older segment is
    /// history, never a second chat with the same person.
    public var conversationHeadIdHexes: [String] {
        conversationPlans().map(\.headHex).sorted()
    }

    // MARK: - KeyPackage publication

    /// Publish a fresh KeyPackage minted on the long-lived
    /// ``canonicalIdentity``, so a Welcome sent to it can actually be
    /// opened on this device.
    ///
    /// Order is mint → durable snapshot persist → upload, and a failed
    /// persist aborts the upload (fail closed: never publish a KeyPackage
    /// whose private keys might not survive a relaunch). Requests coalesce
    /// into at most one pass per burst — see ``KeyPackageRepublisher``.
    ///
    /// Called after every chat bootstrap (each session start, including the
    /// silent relaunch restore) and after every Welcome join attempt. A
    /// no-op until the repository is bootstrapped with a session token.
    /// Returns the driver task so a caller can await completion; production
    /// callers fire and forget.
    @discardableResult
    public func republishKeyPackage() -> Task<Void, Never> {
        keyPackageRepublisher.request()
    }

    /// ADR-0073 section 2.3 step 5 — after a key reset, publish this device's
    /// KeyPackage under a device binding signed by the NEW account key.
    ///
    /// The repository may still hold the identity it built for the SAME
    /// account before the reset, whose stored binding names the OLD account
    /// key; ``bootstrap(secrets:)`` would keep it (same account, early
    /// return). So the in-memory state is dropped and rebuilt from the new
    /// secrets first: ``installDeviceIdentity(accountIdB64:account:)`` then
    /// re-mints the binding (the stored one no longer verifies against the
    /// new account key) and persists it. Then one republish pass runs, and
    /// its outcome is read back: anything but an upload throws, so the
    /// reset's publish record keeps the step owed and retries it.
    public func republishKeyPackageAfterKeyReset(secrets: IdentitySecrets) async throws {
        clear()
        bootstrap(secrets: secrets)
        let before = keyPackageRepublisher.outcomes.count
        await republishKeyPackage().value
        guard keyPackageRepublisher.outcomes.count > before,
              keyPackageRepublisher.outcomes.last == .uploaded else {
            throw KeyResetPublishFailed(
                step: KeyResetPublish.Step.keyPackage.rawValue,
                underlying: DiscoveryClientError.argument(
                    "keypackage republish did not upload: \(String(describing: keyPackageRepublisher.outcomes.last))"))
        }
    }

    // MARK: - Internals

    /// Record a group this device now holds. Every path that founds or joins
    /// a group ends here. Internal rather than private so a test can drive
    /// the join bookkeeping without a Delivery Service.
    func persistKnownGroupBestEffort(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // Section 10 of the UX spec: arriving counts as activity. A launch
        // reloads groups without coming here, so this time survives it.
        groupArrivalStore.remember(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, atMs: Self.nowMs())
        // The device holds the group itself now, so it no longer needs to be
        // carried as "a conversation a restored backup named".
        dropRestoredKnownGroup(groupIdHex: groupIdHex)
        // ADR-0060 — a new group needs its `member` (and, where the setting
        // allows, `group`) reach key registered before a co-member writes.
        ReachCenter.shared.sync()
    }

    /// Drop `groupIdHex` from the conversations a restored backup named
    /// (``RestoredKnownGroupsStore``): this device joined the group, the user
    /// deleted the chat, or the local group ended. No-op when the id is not
    /// in the set.
    func dropRestoredKnownGroup(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        RestoredKnownGroupsStore.remove(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, defaults: restoredGroupsDefaults)
    }

    /// Durably persist the canonical Identity's MLS provider snapshot and
    /// RE-THROW on any failure — the fail-closed counterpart of
    /// ``persistSnapshotBestEffort()``.
    ///
    /// Wired into ``ChatClient/ratchetPersister`` and invoked BEFORE the
    /// DS publish on every application send. A swallowed persist failure
    /// here would re-open the AEAD nonce-reuse-on-crash window:
    /// `createApplicationMessage` has already advanced the sender ratchet
    /// to generation N+1, and the OpenMLS provider is in-memory, so that
    /// N+1 state lives ONLY in this snapshot. If it does not durably land,
    /// the gen-N ciphertext must NOT be published — the caller aborts, and
    /// after a reload the ratchet is safe to re-consume generation N (only
    /// one gen-N ciphertext can ever exist on the wire).
    ///
    /// The plaintext message-log is NOT persisted here: its absence cannot
    /// cause nonce reuse (it is not MLS state), and it is written
    /// best-effort AFTER the send completes so it captures the just-sent
    /// row. When there is no canonical identity / account in scope (tests,
    /// pre-bootstrap) there is nothing durable to persist and no
    /// reload-from-snapshot path that could re-consume a generation, so
    /// this resolves as a no-op (mirror of the web no-op-when-`onPersist`-
    /// unset posture).
    /// The joiner's state before its join request is published
    /// (``requestToJoinFromLink(capabilityWire:nowMs:blobId:)``): the MLS
    /// snapshot holding the proposal's KeyPackage keys, durably, for the
    /// account and identity the request was started with. Throws when the
    /// account's session has ended (``AccountStoreGate``) or the write fails;
    /// the caller then publishes nothing.
    func persistJoinRequestOrThrow(identity: Identity, accountIdB64: String) throws {
        let blob = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(accountIdB64: accountIdB64, blob: blob)
    }

    func persistSnapshotDurableOrThrow() throws {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else { return }
        let blob = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(
            accountIdB64: accountIdB64,
            blob: blob
        )
    }

    /// A founder's state, written before its Welcome is published
    /// (``startGroupWith(recipientAccountId:recipientSignaturePubkey:kind:recipientPlan:)``):
    /// the MLS snapshot holding the new group, durably, then the group's row
    /// in the known groups, so the next start of the account loads it.
    /// Throws when either is refused or fails, or when no account is bound
    /// (``StaleBindingError``); the caller then publishes nothing. Returns
    /// the account the row was written under.
    func persistFoundingOrThrow(groupIdHex: String) throws -> String {
        guard canonicalIdentity != nil, let accountIdB64 = currentAccountIdB64 else {
            throw StaleBindingError()
        }
        try persistSnapshotDurableOrThrow()
        try knownGroupsStore.addOrEnd(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        return accountIdB64
    }


    // MARK: - ADR-0045 proposal-based membership + the committer sweep

    /// ADR-0045 — does this group carry a role table, and so a committer?
    ///
    /// A group with **no role table** has no owner, no rank 0 and no committer,
    /// and every peer's receipt gate drops proposals in it, so a membership
    /// operation on one is refused (``requireRoleTable(groupIdHex:op:)``).
    /// `Group::create` has seeded a role table since ADR-0022, and since the
    /// ADR-0081 amendment of 2026-10-19 `Group.joinFromWelcome` refuses a
    /// Welcome without one, so no client reaches such a group. A torn read is
    /// "no role table", which refuses.
    func usesProposalPath(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex] else { return false }
        return Self.usesProposalPath { try record.group.groupRoles() }
    }

    /// ADR-0081 amendment (2026-10-19) — a membership operation on a group
    /// with no role table is refused, fail closed, with the core's typed
    /// `NoRoleTable` refusal. No client creates such a group and
    /// `Group.joinFromWelcome` refuses its Welcome, so this is unreachable
    /// today; it replaces the direct-commit path that used to serve it.
    func requireRoleTable(groupIdHex: String, op: String) throws {
        guard let record = groups[groupIdHex] else { return }
        try Self.requireRoleTable(op: op) { try record.group.groupRoles() }
    }

    /// The rule of ``requireRoleTable(groupIdHex:op:)`` over an injected read,
    /// so the refusal is testable without a group no client can make.
    nonisolated static func requireRoleTable(op: String, _ roleTable: () throws -> RoleTable?) throws {
        guard usesProposalPath(roleTable) else {
            throw FfiError.MembershipNoRoleTable(
                message: "\(op) refused: this group carries no role table, so it has no committer")
        }
    }

    /// The rule of ``usesProposalPath(groupIdHex:)``, over an injected read so
    /// both of its answers are directly testable — including the table-less case
    /// no `Group.create` can produce.
    nonisolated static func usesProposalPath(_ roleTable: () throws -> RoleTable?) -> Bool {
        do { return try roleTable() != nil } catch { return false }
    }

    /// ADR-0045 — is a role-change proposal already waiting to be swept?
    ///
    /// A torn read is treated as "no", which lets the request through to the
    /// core's own authorisation rather than refusing on a transient store
    /// failure — the core is the enforcement either way, and this check exists
    /// only to convert a silent loss into a clear message.
    ///
    /// The loss it prevents is real rather than theoretical. A role change is a
    /// read-modify-write over the role table; on the direct path the per-group
    /// lock sufficed, because the first toggle's commit updated the table and
    /// the second read it. A PROPOSAL changes nothing until it is swept, so two
    /// toggles taken a moment apart both read the SAME committed table and each
    /// proposes a table carrying only its own change. RFC 9420 permits at most
    /// one `GroupContextExtensions` proposal per commit, so the sweep keeps one
    /// and discards the other — the first promotion would vanish with no error
    /// anywhere. Serializing cannot fix that: there is nothing for the second
    /// read to observe. Nor can the client recompute from "the committed table
    /// as amended by our own pending proposal", because `ProposalKind.roleChange`
    /// carries no payload.
    func hasPendingRoleChange(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex] else { return false }
        guard let pending = try? record.group.pendingProposals() else { return false }
        return pending.contains { if case .roleChange = $0.kind { return true } else { return false } }
    }

    /// ADR-0045 — publish an Add **proposal** instead of committing one.
    ///
    /// The epoch does not advance and there is no Welcome yet — the Welcome is
    /// produced by whichever device sweeps — so there is nothing to roll back
    /// and no ADR-0040 outbox entry to write.
    private func proposeAddMemberLocked(
        groupIdHex: String,
        keyPackageBytes: Data
    ) async throws -> MembershipOutcome {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let proposalBytes: Data
        do {
            proposalBytes = try record.group.proposeAddMemberFromWire(
                identity: identity,
                keyPackage: keyPackageBytes
            )
        } catch {
            throw ChatError.wrapped("proposeAddMemberFromWire failed", underlying: error)
        }
        return try await publishProposal(
            record: record, groupIdHex: groupIdHex, proposalBytes: proposalBytes)
    }

    /// ADR-0045 — publish a Remove **proposal**.
    ///
    /// Naming this device's OWN leaf is how a member leaves, and needs no role;
    /// any other leaf needs owner or admin, and the owner's leaf is refused for
    /// everyone. The core runs that rule locally before anything is published,
    /// so an unauthorised request never reaches the wire and the caller gets a
    /// synchronous, specific error — but the local check is a courtesy: the
    /// enforcing copies are every peer's receipt gate and commit gate, which do
    /// not trust us.
    ///
    /// The member STAYS in the roster until a sweep lands, and the UI must keep
    /// showing them. Dropping the row now would assert they are gone while they
    /// are still in the group and still receiving every message.
    private func proposeRemoveMemberLocked(
        groupIdHex: String,
        leafIndex: UInt32
    ) async throws -> MembershipOutcome {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — call bootstrap() first")
        }
        guard let record = groups[groupIdHex] else {
            throw ChatError.argument("no live Group handle for \(groupIdHex)")
        }
        let proposalBytes: Data
        do {
            proposalBytes = try record.group.proposeRemoveMember(
                identity: identity,
                leafIndex: leafIndex
            )
        } catch {
            throw ChatError.wrapped("proposeRemoveMember failed", underlying: error)
        }
        return try await publishProposal(
            record: record, groupIdHex: groupIdHex, proposalBytes: proposalBytes)
    }

    /// The tail every propose path shares: persist (the proposal is now in this
    /// device's own pending store, which is provider state, so a relaunch must
    /// not lose a request the peers hold), then publish.
    ///
    /// Best-effort on the persist rather than fail-closed, and the asymmetry
    /// with the commit paths is the point: a proposal advances no epoch, so a
    /// lost snapshot costs the local copy of a request the journal still holds,
    /// where a lost commit snapshot costs the device its place in the group.
    ///
    /// `transferToB64` (ADR-0045 UX §6.1.10) is supplied only by the ownership-
    /// transfer path and is filed on the `role-change` observation alone. Keying
    /// it on that one synthetic request key is exact rather than approximate:
    /// C-65 permits at most one pending role change per group and the caller has
    /// just checked there was none, so the only role change in this set is the
    /// one we published a line ago.
    private func publishProposal(
        record: GroupRecord,
        groupIdHex: String,
        proposalBytes: Data,
        transferToB64: String? = nil
    ) async throws -> MembershipOutcome {
        try persistSnapshotBeforePublish()
        let bound = binding.capture()
        let publish: PublishResult
        do {
            publish = try await dsClient.publishGroup(
                groupId: record.groupId,
                payload: proposalBytes,
                auth: dsAuth(groupIdHex: groupIdHex)
            )
        } catch {
            throw ChatError.wrapped("DS publishGroup (membership proposal) failed", underlying: error)
        }
        // The request record below, and the intent every caller files after
        // this returns, are written under the account that is bound now.
        try requireCommitRegion(bound)
        // The proposer never receives its own application-layer echo, but a
        // proposal IS re-fetched by the poll, and OpenMLS refuses to process a
        // frame this device sent. Suppress it so the poll drops it quietly
        // instead of logging a decrypt failure every batch.
        chatClient.markOwnPublishedCommit(groupIdHex: groupIdHex, messageId: publish.id)
        // Record our own request at the DS row's timestamp, so the quiet window
        // starts from the same instant on every device — and mark it `own`, the
        // UX §10 exception that lets a plain member see the status of a request
        // they made themselves.
        if let accountIdB64 = currentAccountIdB64,
           let atMs = Self.parseDsTimestampMs(publish.createdAt),
           let pending = try? record.group.pendingProposals() {
            let groupIdB64 = base64Encode(record.groupId)
            for proposal in pending where proposal.stored {
                let key = proposalKey(proposal)
                pendingRequestStore.record(
                    accountIdB64: accountIdB64,
                    groupIdB64: groupIdB64,
                    key: key,
                    atMs: atMs,
                    own: proposal.senderLeaf == (try? record.group.ownLeafIndex()),
                    transferToB64: key == "role-change" ? transferToB64 : nil
                )
            }
        }
        return .proposed(proposalMessageId: publish.id)
    }

    // MARK: - ADR-0045 the committer sweep

    /// ADR-0045 S4a/S4b — run one committer sweep for `groupIdHex`, if this
    /// device is the acting committer.
    ///
    /// The committer set is the ADR-0022 owner plus admins (and, ADR-0050
    /// 2.5a, a plain member while it holds a device admission), ordered by
    /// `ownCommitterRank()`; every leaf of a ranked account holds its rank.
    /// Which device ACTS is decided by the fold over the liveness beacons this
    /// device has observed: it acts when nobody better placed — a strictly
    /// lower rank, or the same rank with a lower signature key (another device
    /// of the same account) — has beaconed inside the freshness window, which
    /// includes the case where it has heard nothing at all. So an admin-only
    /// group no longer waits for an absent owner, and an account whose
    /// lowest-keyed device was lost still acts through its live one.
    ///
    /// Short-circuits synchronously — before touching the network — when this
    /// device is not a candidate, or when nothing is pending and no verdict is
    /// owed. That is what makes it safe to call at the tail of every poll.
    ///
    /// Runs under the same per-group commit lock the membership paths and the
    /// poll's apply half take. The whole region (stage -> publish -> verdict ->
    /// merge/abandon) is inside it, network round-trips included, because the
    /// verdict is a read-modify-write over both the group epoch and the journal
    /// cursor; a poll interleaving in the middle would not merely slow it down,
    /// it would make the verdict wrong.
    @discardableResult
    public func sweepIfCommitter(groupIdHex: String) async -> SweepOutcome {
        // An accept's owed Welcome owes no commit and no rank.
        await resolveOwedAnswers(groupIdHex: groupIdHex)
        // A group with no role table has no committer to be. Short-circuit
        // before taking the lock, so the poll tail costs nothing at all there.
        // (Its staged direct commits went with the ADR-0081 amendment of
        // 2026-10-19, so nothing can be owed on it.)
        guard usesProposalPath(groupIdHex: groupIdHex) else {
            return .notCommitter(rank: nil)
        }
        guard let record = groups[groupIdHex] else { return .notCommitter(rank: nil) }
        let before = PushGroupRouting.memberSignatureKeys(group: record.group)
        let leafKeysBefore = Self.leafKeysB64(group: record.group)
        // `before` and `leafKeysBefore` are this binding's roster; what
        // follows the sweep compares them with `groups[groupIdHex]` as it is
        // then.
        let bound = binding.capture()
        let outcome: SweepOutcome
        do {
            outcome = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                guard let record = groups[groupIdHex], let identity = canonicalIdentity else {
                    return .notCommitter(rank: nil)
                }
                _ = identity
                // ADR-0057 S2 finding 1 — a merged sweep (or a remote commit its
                // verdict applied) that carried this account's ownership
                // transfer hands the journal root over. Read on every exit,
                // a thrown verdict included: rows below it may have applied.
                let ownerBeforeSweep = (try? record.group.groupRoles())?.owner
                defer {
                    noteOwnerTransition(
                        groupIdHex: groupIdHex, before: ownerBeforeSweep,
                        after: roleTableOwner(groupIdHex: groupIdHex))
                }
                let outcome = try await runCommitterSweep(sweepPorts(record: record, groupIdHex: groupIdHex))
                // ADR-0057 obligation 3 — a sweep that landed changed the
                // roster: the owner appoints the new members and dismisses the
                // departed. (No heal: a lost sweep's registration is replaced
                // by the winner's, or refused before its commit went out.)
                if case .committed = outcome {
                    reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
                }
                return outcome
            }
        } catch {
            // A verdict this device could not read leaves the sweep staged for
            // the next attempt (which runs at the tail of every poll). Never
            // surfaced as a user-facing error: nothing the user did failed, and
            // the request is still live.
            NSLog("ADR-0045 sweep did not complete: \(error)")
            return .unresolved
        }
        // ADR-0050 2.5a — this sweep (or a remote commit its verdict applied)
        // may have moved the epoch; a sweep called outside the poll (after a
        // user action) is otherwise not followed by an evaluation until the
        // next poll. Only schedules.
        guard bound.isCurrent else { return .unresolved }
        scheduleSupersededReRequest(groupIdHex: groupIdHex)
        guard case .committed(_, _, let addedCount) = outcome else { return outcome }
        if let accountIdB64 = currentAccountIdB64, let current = groups[groupIdHex] {
            DeviceReadmissionMemory.recordRemoved(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                before: leafKeysBefore, after: Self.leafKeysB64(group: current.group))
        }

        state = state.with(threads: buildThreadList())
        // ADR-0043 — the swept commit may have removed somebody, who still holds
        // this group's `push_routing_secret`. Driven off an ACTUAL before/after
        // member-set comparison, never off "a sweep happened". Fire-and-forget:
        // it only spawns a `Task`, so the routing chain it eventually takes is
        // never awaited while the commit chain is held.
        rotatePushRoutingSecretIfMembershipShrank(
            groupIdHex: groupIdHex, previousMemberKeys: before)
        // ADR-0042 — a swept ADD needs the current routing secret, exactly as
        // the direct add path re-broadcasts it. No-op when this device holds
        // none.
        if addedCount > 0 { redistributePushRoutingSecret(groupIdHex: groupIdHex) }
        // ADR-0048 §2 — a swept Add is growth too.
        noteProfileGrowthIfMembershipGrew(
            groupIdHex: groupIdHex, previousMemberKeys: before, ownAdd: addedCount > 0)
        return outcome
    }

    /// Adapt a live `GroupRecord` + this repository's DS / outbox wiring onto
    /// the ``SweepPorts`` the ADR-0045 sweep orchestration runs over.
    ///
    /// The adapter is deliberately thin and does no deciding: every rule lives
    /// in `CommitterSweep.swift`, where it is host-testable without an MLS
    /// provider, a filesystem or a network.
    private func sweepPorts(record: GroupRecord, groupIdHex: String) -> SweepPorts {
        let groupIdB64 = base64Encode(record.groupId)
        // A sweep waits on the network between its ports (the journal read,
        // the registration, the publish, the verdict read), and each port
        // reads the bound identity, the account and its stores when it is
        // called, beside `record`, which is the starting account's. Once that
        // account has left, a port answers as it does for a repository that
        // is not bootstrapped: it throws, or does nothing. The sweep then
        // stops where a crash would have stopped it, and the account's next
        // session resolves what it owes.
        let bound = binding.capture()
        return SweepPorts(
            group: SweepGroupPort(
                groupId: record.groupId,
                epoch: { try record.group.epoch() },
                ownCommitterRank: { [weak self] in
                    guard bound.isCurrent, let identity = self?.canonicalIdentity else { return nil }
                    return try? record.group.ownCommitterRank(identity: identity)
                },
                pendingProposalCount: { ((try? record.group.pendingProposals()) ?? []).count },
                hasStagedSweep: { (try? record.group.hasStagedSweep()) ?? false },
                stagePendingProposals: { [weak self] in
                    guard bound.isCurrent, let identity = self?.canonicalIdentity else { return nil }
                    guard let staged = try record.group.stagePendingProposals(identity: identity)
                    else { return nil }
                    // `swept` reports what the commit REALLY carries, read back
                    // off the staged commit by the core rather than predicted by
                    // the caller — which matters here because it is what
                    // addresses the Welcome.
                    var added: [Data] = []
                    var removedLeaves: Set<UInt32> = []
                    for proposal in staged.swept {
                        switch proposal.kind {
                        case .add(let signatureKey):
                            added.append(signatureKey)
                        case .remove(let targetLeaf):
                            removedLeaves.insert(targetLeaf)
                        case .leave:
                            if let leaf = proposal.senderLeaf { removedLeaves.insert(leaf) }
                        default:
                            break
                        }
                    }
                    // ADR-0057 — the accounts this sweep evicts, from the roster
                    // BEFORE the merge, for the registration's revoke list.
                    record.stagedEvictedAccountKeys = self?.evictedAccountKeys(
                        group: record.group, removedLeaves: removedLeaves) ?? []
                    return StagedSweepArtifacts(
                        commit: staged.commit,
                        welcome: staged.welcome,
                        addedSignatureKeys: added
                    )
                },
                mergeStagedSweep: { [weak self] in
                    guard bound.isCurrent, let identity = self?.canonicalIdentity else {
                        throw ChatError.argument("ChatRepository not bootstrapped — cannot merge")
                    }
                    // Membership rows — this device never receives its own
                    // commit back, so it writes the row itself (the roster
                    // before the merge against the roster after it).
                    let personKeysBeforeMerge = Self.personKeysB64(of: record.group)
                    _ = try record.group.mergeStagedSweep(identity: identity)
                    self?.noteOwnCommitMembershipChange(
                        groupIdHex: groupIdHex, record: record,
                        personKeysBefore: personKeysBeforeMerge,
                        commitMessageId: record.ownPublishedCommitMessageId)
                },
                abandonStagedSweep: { [weak self] in
                    guard bound.isCurrent, let identity = self?.canonicalIdentity else {
                        throw ChatError.argument("ChatRepository not bootstrapped — cannot abandon")
                    }
                    _ = try record.group.abandonStagedSweep(identity: identity)
                },
                // ADR-0045 — apply AND surface. The verdict consumes every row the
                // DS ordered below our own commit and the shared cursor then
                // advances past them, so a row applied here is a row the poll will
                // never fetch again: discarding what it decrypted loses an ordinary
                // chat message from the timeline permanently and silently. Routed
                // through the SAME `ChatClient.toEvent` + ``GroupRecord/consumeEvents``
                // the poll uses, so every envelope kind is dispatched by one ladder.
                applyIncoming: { [weak self] row in
                    guard let self, bound.isCurrent, let identity = self.canonicalIdentity else {
                        throw ChatError.argument("ChatRepository not bootstrapped — cannot apply")
                    }
                    // The MLS apply is what the verdict reads, so it is
                    // deliberately OUTSIDE the surfacing below: a throw here must
                    // reach `adjudicate`, which skips the row exactly as the poll
                    // loop does, and a skipped row must surface nothing.
                    // Membership rows — the people before this row's apply.
                    let personKeysBeforeRow = Self.personKeysB64(of: record.group)
                    let firstNewRow = record.messages.count
                    let processed = try record.group.processIncomingVerifyingRoles(
                        identity: identity, bytes: row.payload)
                    let event = ChatClient.toEvent(
                        messageId: row.id,
                        createdAt: row.createdAt,
                        processed: processed,
                        // ADR-0023 — the SECOND receive path, and it must
                        // resolve identically to the poll: the roster is read
                        // here, one line after the apply, in the epoch this row
                        // decrypted at. These rows are gone from the poll's
                        // future forever, so an author resolved wrongly here is
                        // resolved wrongly for good. It passes the GROUP, not a
                        // resolver of its own — the two paths are then left
                        // nothing to disagree about.
                        group: record.group
                    )
                    // ADR-0073 — the same two silent drops the poll applies
                    // (a superseded sender, a re-send of a message already
                    // held in an older segment): the two receive paths must
                    // not disagree.
                    var origins: [Int64: ResendMarker.Origin] = [:]
                    if let origin = ChatClient.resendOrigin(of: processed) { origins[row.id] = origin }
                    let events = self.silentlyFiltered(
                        [event], resendOrigins: origins,
                        eventSenders: [ChatClient.applicationSender(of: processed, group: record.group)],
                        groupIdHex: groupIdHex)
                    self.noteAppliedForUnread(events, groupIdHex: groupIdHex)
                    self.matchDisplayCopies(
                        events, groupIdHex: groupIdHex, ownAuthorKey: AuthorIdentity.own(group: record.group))
                    record.consumeEvents(
                        events,
                        ownAuthorKey: AuthorIdentity.own(group: record.group),
                        callSignalingSink: { [weak self] messageId, payload, createdAt, senderLeaf, senderDeviceKey, senderKey in
                            self?.routeCallSignaling(
                                groupIdHex: groupIdHex, messageId: messageId, payload: payload,
                                createdAt: createdAt, senderLeaf: senderLeaf,
                    senderDeviceKey: senderDeviceKey, senderKey: senderKey)
                        },
                        readReceiptSink: { [weak self] sender, upTo in
                            self?.recordReadReceipt(
                                groupIdHex: groupIdHex, sender: sender, upTo: upTo)
                        },
                        chatStatusSink: { [weak self] status in
                            self?.chatStatusSink?(groupIdHex, status)
                        },
                        pushRoutingSecretSink: { [weak self] secret, mintEpoch, senderLeaf in
                            self?.receivePushRoutingSecret(
                                groupIdHex: groupIdHex, secret: secret, mintEpoch: mintEpoch,
                                senderLeaf: senderLeaf)
                        },
                        committerBeaconSink: { [weak self] senderLeaf, createdAt in
                            self?.recordCommitterBeacon(
                                groupIdHex: groupIdHex, senderLeaf: senderLeaf,
                                createdAt: createdAt)
                        },
                        membershipProposalSink: { [weak self] proposal, createdAt in
                            self?.recordPendingRequestObservation(
                                groupIdHex: groupIdHex, record: proposal, createdAt: createdAt)
                        },
                        profileSink: { [weak self] claim, senderKey in
                            guard let self else { return }
                            self.receivedProfileSink(claim, senderKey, self.currentAccountIdB64)
                            self.keepProfileGrants(
                                claim, senderKey: senderKey, groupIdHex: groupIdHex)
                        },
                        groupNameSink: { [weak self] name, updatedAtMs, senderLeaf in
                            self?.receiveGroupName(
                                groupIdHex: groupIdHex, name: name, updatedAtMs: updatedAtMs,
                                senderLeaf: senderLeaf)
                        },
                        profileRequestSink: { [weak self] senderKey in
                            self?.answerProfileRequest(groupIdHex: groupIdHex, senderKey: senderKey)
                        },
                        groupContactRequestSink: { [weak self] envelope, senderKey in
                            self?.onGroupContactRequest?(groupIdHex, envelope, senderKey)
                        },
                        pinSink: { [weak self] op, senderLeaf in
                            self?.receivePin(
                                groupIdHex: groupIdHex, op: op, senderLeaf: senderLeaf)
                        },
                        registrarSeedSink: { [weak self] plaintext, senderLeaf in
                            self?.receiveRegistrarSeed(
                                groupIdHex: groupIdHex, plaintext: plaintext,
                                senderLeaf: senderLeaf)
                        },
                        scopedDiscoverySink: { [weak self] envelope, senderKey in
                            self?.receiveScopedDiscovery(
                                groupIdHex: groupIdHex, envelope: envelope, senderKey: senderKey)
                        },
                        sharedChannelsSink: { [weak self] list, senderKey in
                            self?.sharedChannels.receive(
                                groupIdHex: groupIdHex, list: list, senderKey: senderKey)
                        },
                        groupClosedSink: { [weak self] messageId, successorGroupId, senderLeaf in
                            self?.receiveGroupClosed(
                                groupIdHex: groupIdHex, messageId: messageId,
                                successorGroupId: successorGroupId, senderLeaf: senderLeaf)
                        },
                        groupSettingSink: { [weak self] setting, senderLeaf in
                            self?.receiveGroupSetting(
                                groupIdHex: groupIdHex, setting: setting, senderLeaf: senderLeaf)
                        },
                        reactionToOwnSink: { [weak self] messageId in
                            self?.noteUnseenReaction(groupIdHex: groupIdHex, messageId: messageId)
                        }
                    )
                    // Membership rows — the same rule as the poll, per row.
                    self.noteMembershipChange(
                        groupIdHex: groupIdHex, record: record, events: events,
                        personKeysBefore: personKeysBeforeRow, firstNewRow: firstNewRow)
                    // Per row, and deliberately not deferred to the end of the
                    // sweep: this is what republishes `record.messages` into the
                    // observable state the chat screen reads, and it must happen on
                    // a LOST verdict and on a thrown one too. Whether our own commit
                    // won the order says nothing about the rows below it — they were
                    // applied, the ratchet moved and the cursor advanced past them
                    // either way, so the only question left is whether the user gets
                    // to read them. Cheap because the window this loop consumes is
                    // narrow by construction.
                    self.state = self.state.with(threads: self.buildThreadList())
                    // ADR-0052 decision 3 — a delete applied here drops its
                    // shared pin record. Only spawns a `Task`.
                    self.dropSharedPinsOfDeletedMessages(groupIdHex: groupIdHex)
                }
            ),
            actingCommitter: { [weak self] ownRank in
                guard let self, bound.isCurrent else { throw StaleBindingError() }
                return try self.actingCommitterNow(record: record, ownRank: ownRank)
            },
            journal: SweepJournalPort(
                // `dsClient`, not `chatClient.fetchGroupJournal`: the sweep's
                // publishes already go through `dsClient`, and the verdict read
                // is a read-modify-write paired with them. Routing the two
                // halves of one adjudication through two different DS clients
                // would make the verdict depend on which of them a caller
                // configured.
                fetch: { [weak self] after in
                    guard let self, bound.isCurrent else { throw StaleBindingError() }
                    let raw = try await self.dsClient.fetchGroup(
                        groupId: record.groupId, after: after, waitSecs: 0,
                        auth: self.dsAuth(groupIdHex: groupIdHex))
                    // ADR-0045 — `createdAt` carried so an applied row can become
                    // a timeline event.
                    return raw.messages.map {
                        SweepJournalRow(id: $0.id, payload: $0.payload, createdAt: $0.createdAt)
                    }
                },
                // ADR-0081 decision 1 — the staged commit and the registration
                // of the epoch it moves to (`dsStagedAccessSigner`) in one
                // request, proven under the epoch the group still sits at.
                commitStaged: { [weak self] bytes in
                    guard let self, bound.isCurrent, let identity = self.canonicalIdentity else {
                        throw ChatError.argument("ChatRepository not bootstrapped — cannot commit")
                    }
                    guard let signerN1 = try record.group.dsStagedAccessSigner(identity: identity) else {
                        throw ChatError.argument("no staged sweep to commit")
                    }
                    let proof = try record.group.dsAccessSigner(identity: identity)
                    // The staged sweep has not merged, so the group is still at
                    // N: the owner's claim (once per session) names N.
                    await self.ensureOwnerClaimOnOpen(groupIdHex: groupIdHex)
                    try bound.require()
                    let epochN = (try? record.group.epoch()) ?? 0
                    do {
                        let answer = try await self.commitNextEpoch(
                            record: record, groupIdHex: groupIdHex, next: signerN1, proof: proof,
                            row: bytes, evictedAccountKeys: record.stagedEvictedAccountKeys)
                        try bound.require()
                        self.sweepRegistrationBackoff[groupIdHex] = nil
                        return answer.id
                    } catch {
                        if bound.isCurrent, SweepRegistrationBackoff.appliesTo(error) {
                            let next = SweepRegistrationBackoff.afterRefusal(
                                self.sweepRegistrationBackoff[groupIdHex],
                                nowMs: Self.nowMs(), epoch: epochN)
                            self.sweepRegistrationBackoff[groupIdHex] = next
                            NSLog("ADR-0057: sweep commit refused (%@); next attempt in %lld s",
                                  String(describing: error),
                                  SweepRegistrationBackoff.delayMs(failures: next.failures) / 1000)
                        }
                        throw error
                    }
                },
                publishMailbox: { [weak self] target, bytes in
                    guard let self, bound.isCurrent else { throw StaleBindingError() }
                    return try await self.publishSweepWelcome(
                        target: target, bytes: bytes, record: record, groupIdHex: groupIdHex)
                },
                // ADR-0057 obligation 5 — no root and no registrar seed: this
                // device cannot register the next epoch, so it never stages.
                canRegister: { [weak self] in
                    self?.canRegisterJournalEpochs(groupIdHex: groupIdHex) ?? false
                },
                // Live walk 2026-09-29 — a refused registration waits out its
                // backoff; an epoch change (a membership or role change) ends
                // the wait at once.
                registrationDue: { [weak self] in
                    guard let self, let backoff = self.sweepRegistrationBackoff[groupIdHex] else {
                        return true
                    }
                    let epoch = (try? record.group.epoch()) ?? backoff.epoch
                    return backoff.allows(nowMs: Self.nowMs(), epoch: epoch)
                }
            ),
            outbox: SweepOutboxPort(
                newEntryId: { newOutboxEntryId() },
                record: { [weak self] entry in
                    guard let self, bound.isCurrent else {
                        throw ChatError.argument("ChatRepository deallocated — sweep aborted")
                    }
                    var next = NewCommitEntry(
                        id: entry.id,
                        groupId: groupIdB64,
                        // The epoch the sweep was BUILT AGAINST: a staged sweep
                        // has not merged, so unlike every direct-path entry this
                        // is N, not N+1. It is diagnostics only —
                        // `sweepVerdictPending` is what the restart sweep
                        // branches on.
                        targetEpoch: String((try? record.group.epoch()) ?? 0),
                        commitB64: entry.commitB64,
                        sweepVerdictPending: true,
                        welcomeMailboxesB64: entry.welcomeMailboxesB64
                    )
                    if let welcome = entry.welcomeEnvelopeB64 {
                        next.secondArtifactB64 = welcome
                        next.secondArtifactKind = .welcome
                    }
                    try self.persistCommitEntryThenSnapshotOrThrow(next)
                },
                findOwed: { [weak self] in
                    guard let self, bound.isCurrent, let accountIdB64 = self.currentAccountIdB64 else { return nil }
                    // A welcomes-owed entry has already reached a verdict, so it
                    // is not this finder's business — re-adjudicating it would
                    // re-publish a commit whose order was already read.
                    guard let row = self.commitOutbox.loadUnacked(
                        accountIdB64: accountIdB64, groupIdB64: groupIdB64
                    ).first(where: {
                        $0.sweepVerdictPending == true && $0.sweepWelcomesOwed != true
                    }) else { return nil }
                    return SweepDurableEntry(
                        id: row.id,
                        commitB64: row.commitB64,
                        welcomeEnvelopeB64: row.secondArtifactB64,
                        welcomeMailboxesB64: row.welcomeMailboxesB64 ?? [],
                        commitMessageId: row.commitMessageId
                    )
                },
                markWelcomesOwed: { [weak self] id, commitMessageId in
                    guard let self, bound.isCurrent, let accountIdB64 = self.currentAccountIdB64 else {
                        throw ChatError.argument("no account — cannot record the owed Welcome")
                    }
                    // Outbox bookkeeping only: the MLS snapshot was persisted one
                    // line earlier by the sweep's own `persist`, so this
                    // deliberately does NOT go through
                    // `persistCommitEntryThenSnapshotOrThrow`.
                    try self.commitOutbox.markWelcomesOwed(
                        accountIdB64: accountIdB64,
                        id: id,
                        commitMessageId: commitMessageId
                    )
                },
                findOwedWelcomes: { [weak self] in
                    guard let self, bound.isCurrent, let accountIdB64 = self.currentAccountIdB64 else { return [] }
                    return self.commitOutbox.loadUnacked(
                        accountIdB64: accountIdB64, groupIdB64: groupIdB64
                    ).filter { $0.sweepWelcomesOwed == true && $0.answerRequesterKeyB64 == nil }.map { row in
                        SweepDurableEntry(
                            id: row.id,
                            commitB64: row.commitB64,
                            welcomeEnvelopeB64: row.secondArtifactB64,
                            welcomeMailboxesB64: row.welcomeMailboxesB64 ?? [],
                            commitMessageId: row.commitMessageId
                        )
                    }
                },
                keepOwedWelcomes: { [weak self] id, commitMessageId, mailboxes in
                    guard let self, bound.isCurrent, let accountIdB64 = self.currentAccountIdB64 else {
                        throw ChatError.argument("no account — cannot keep the owed Welcome")
                    }
                    try self.commitOutbox.keepWelcomesOwed(
                        accountIdB64: accountIdB64, id: id,
                        commitMessageId: commitMessageId, mailboxesB64: mailboxes)
                },
                ack: { [weak self] id, commitMessageId, welcomeMessageId in
                    guard bound.isCurrent else { return }
                    self?.ackCommitBestEffort(
                        entryId: id,
                        commitMessageId: commitMessageId,
                        secondMessageId: welcomeMessageId
                    )
                },
                discard: { [weak self] id in
                    guard let self, bound.isCurrent, let accountIdB64 = self.currentAccountIdB64 else { return }
                    self.commitOutbox.deleteEntryBestEffort(accountIdB64: accountIdB64, id: id)
                }
            ),
            cursor: SweepCursorRef(
                read: { record.cursor },
                advance: { record.advanceCursor(to: $0) }
            ),
            // ADR-0060 section 6 — each added leaf's Welcome goes to its
            // owner's ACCOUNT mailbox. Resolved here when this device can
            // (a `/g/` joiner's retained KeyPackage, a KeyPackage this device
            // fetched to propose the add); otherwise the leaf key itself is
            // recorded and resolved through the roster once the sweep merged
            // (``publishSweepWelcome``).
            welcomeMailbox: { [weak self] key in
                // ADR-0074 section 3.5a rule 1 / ADR-0078 — resolved before
                // addressing, to the verified mailbox a grant held for the
                // person names. With none known yet the leaf key is recorded
                // and resolved through the roster once the sweep merged.
                if let self, let accountKey = self.resolveAddedAccountKey(record: record, leafKey: key),
                   let target = ReachCenter.shared.writePlan(toAccountKey: accountKey)?.target {
                    return target.address
                }
                return key
            },
            // ADR-0053 point 8 — every added leaf, a `/g/` joiner included, gets
            // its Welcome in its own mailbox; no `WelcomeRendezvous` is written
            // to the group journal any more.
            // ADR-0078 — the Welcome is recorded raw; it is sealed as a
            // `.welcome` row at publish time, to the verified mailbox
            // (``publishSweepWelcome``).
            wrapWelcome: { welcome in welcome },
            markOwnPublished: { [weak self] id in
                guard bound.isCurrent else { return }
                record.ownPublishedCommitMessageId = id
                self?.chatClient.markOwnPublishedCommit(groupIdHex: groupIdHex, messageId: id)
            },
            persist: { [weak self] in
                guard bound.isCurrent else { return }
                self?.persistSnapshotBestEffort()
            },
            // The verdict apply advances the SAME cursor the poll drives, so it
            // takes the FAIL-CLOSED persist — see `Chat/DurableCursor.swift`.
            // This one IS escaping (it is stored on the ports struct), so `self`
            // is captured weakly — but a released repository THROWS rather than
            // resolving to "the write succeeded". Optional-chaining it away would
            // let the sweep advance the shared cursor over rows nothing wrote.
            persistApplied: { [weak self] in
                guard let self, bound.isCurrent else {
                    throw ChatError.argument(
                        "ADR-0045 verdict apply: repository released before the snapshot write"
                    )
                }
                try self.persistSnapshotDurableOrThrow()
            },
            // ADR-0081 — a 409 is a lost epoch, a 400 / 401 / 403 a refusal;
            // anything else (the DS not reached) leaves the sweep staged.
            classifyCommitFailure: { error in
                if (error as? DsStatementError) == .conflict { return .lost }
                return SweepRegistrationBackoff.appliesTo(error) ? .refused : .unknown
            }
        )
    }

    /// ADR-0045 S4b — the acting-committer verdict, taken fresh on every sweep
    /// attempt.
    ///
    /// **Every rank in the projection is derived HERE and now**, by
    /// `Group.committerRank` against the group's live `0xFF02` role table —
    /// never carried over from when the beacon arrived. A rank is a position in
    /// that table's canonical order, so promoting, demoting or removing any
    /// admin renumbers every admin sorting after it; a rank captured under one
    /// ordering and compared against this device's own current rank is not
    /// stale, it is meaningless.
    ///
    /// ADR-0050 2.5a item 6 — the fold also takes this device's own leaf key
    /// (the DEVICE identity the group runs on) and each sender's key: it
    /// ignores this device's own beacon and defers to a fresh beacon of the
    /// same rank with a lower key, i.e. to a lower-keyed active device of the
    /// same account. Throws when no device identity is bound (the sweep reads
    /// a throw as a stand-down).
    private func actingCommitterNow(record: GroupRecord, ownRank: UInt32) throws -> ActingCommitter {
        guard let identity = canonicalIdentity else {
            throw ChatError.argument("ChatRepository not bootstrapped — no own signature key")
        }
        guard let accountIdB64 = currentAccountIdB64 else {
            // No session to read a store against. `notACandidate` reads as a
            // stand-down in `runCommitterSweep`, which is the fail-closed side:
            // acting on a view of who else is running that this device could not
            // read is the hazard.
            return .notACandidate
        }
        return try decideActingCommitter(
            store: beaconStore,
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            ownRank: ownRank,
            ownSignatureKey: identity.signaturePublicKey(),
            rankOf: { senderKeyB64 in
                guard let key = base64Decode(senderKeyB64) else { return nil }
                return try? record.group.committerRank(signatureKey: key)
            },
            nowMs: Self.nowMs(),
            // Read off the FFI, never hand-copied — the interval and the window
            // are two halves of one budget and a local literal is the drift
            // class ADR-0044 exists about.
            freshnessWindowMs: committerBeaconFreshnessWindowMs(),
            fold: coreActingCommitterFold
        )
    }

    // MARK: - ADR-0045 the committer liveness beacon

    /// ADR-0045 S4b — record that a beacon landed, from the only two things the
    /// field-less envelope deliberately does not carry.
    ///
    /// WHO comes from the MLS-authenticated sender leaf, resolved to a signature
    /// KEY **here, at receipt**, because that is the only moment the roster is
    /// authenticated in the epoch the beacon was sealed at. The KEY is what gets
    /// persisted, never the leaf: OpenMLS reuses a vacated leaf for the next
    /// member added, so a stored leaf silently comes to name somebody else.
    ///
    /// HOW OLD comes from the DS row's server-assigned timestamp — never a local
    /// clock, and never anything the sender asserted. A row whose timestamp does
    /// not parse is DROPPED rather than stored: there is no age to measure, so
    /// it is no evidence of liveness.
    func recordCommitterBeacon(groupIdHex: String, senderLeaf: UInt32?, createdAt: String) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex],
              let senderLeaf,
              let atMs = Self.parseDsTimestampMs(createdAt),
              let roster = try? record.group.members(),
              let senderKey = roster.first(where: { $0.leafIndex == senderLeaf })?
                .signaturePublicKey
        else { return }
        beaconStore.record(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            senderKeyB64: base64Encode(senderKey),
            atMs: atMs
        )
    }

    /// ADR-0045 S4b — emit this device's own liveness beacon, if it is due.
    ///
    /// `foreground` is the one rule the core cannot check and it is load-bearing
    /// rather than cosmetic: a beacon means *able to sweep*, and on this client
    /// a sweep runs only at the tail of a poll driven by a visible `ChatScreen`.
    /// The ADR-0041 NotificationServiceExtension decrypt is
    /// read-only-throwaway and structurally cannot commit (**C-59**) — the slim
    /// FFI it links does not even export `composeCommitterBeacon` — so a beacon
    /// from a backgrounded app would be a lie that costs the group a takeover.
    ///
    /// Everything else — only-candidates-emit, first-of-session-immediate, one
    /// per interval — is the core's `shouldEmitBeacon`, called through the gate.
    ///
    /// Best-effort and silent: nobody is watching a beacon, and a failed emit
    /// only means this device does not claim the duty on this tick.
    @discardableResult
    func emitCommitterBeaconIfDue(groupIdHex: String, foreground: Bool) async -> BeaconEmitVerdict {
        // Short-circuited FIRST so a backgrounded app does no work at all — the
        // same answer `beaconEmitVerdict` gives below, reached before the
        // Ed25519 key derivation the rank read costs. The gate there stays the
        // authority.
        guard foreground else { return .notForeground }
        // ADR-0045 UX §6.2.8 item 2 — an evicted device must never beacon. A
        // beacon means *able to sweep*, and this one can never sweep again: the
        // envelope is an ordinary MLS application message and
        // `createApplicationMessage` is refused for an inactive group, so the
        // emit would throw on its own ticker every interval anyway.
        //
        // Note this is the SAME rule that keeps a backgrounded app from
        // emitting, reached from the other direction: there the device could
        // sweep and is not running; here it is running and never can. Placed
        // directly beside that check for exactly that reason.
        //
        // The ticker is invalidated too (the view model's one-shot teardown),
        // so this is belt and braces — but it is the half that is a GATE, and
        // the gate is what makes a surviving timer harmless rather than merely
        // unlikely. `.notACandidate` rather than a new verdict: this device is
        // not a candidate to sweep, which is exactly what the fold means.
        if membershipEnded(membershipStateNow(groupIdHex: groupIdHex)) {
            return .notACandidate
        }
        guard usesProposalPath(groupIdHex: groupIdHex) else { return .notACandidate }
        guard let identity = canonicalIdentity, let record = groups[groupIdHex] else {
            // This device cannot establish that it is a candidate, so it must
            // not claim to be one.
            return .notACandidate
        }
        let ownRank = try? record.group.ownCommitterRank(identity: identity)
        let now = UInt64(max(0, Self.nowMs()))
        let verdict = beaconEmitVerdict(
            ownRank: ownRank,
            foreground: foreground,
            // ADR-0050 2.5a item 8 — the same check the sweep makes before it
            // stages (``canRegisterJournalEpochs(groupIdHex:)``): a device with
            // neither the root nor a registrar seed could not complete a sweep,
            // so it must not make others defer to it. Read only for a ranked
            // device: it can derive the root signer.
            canRegister: ownRank == nil || canRegisterJournalEpochs(groupIdHex: groupIdHex),
            lastEmittedAtMs: lastBeaconAtMs[groupIdHex],
            nowMs: now,
            throttle: { rank, last, nowMs in
                shouldEmitBeacon(ownRank: rank, lastEmittedAtMs: last, nowMs: nowMs)
            }
        )
        guard verdict == .emit else { return verdict }
        // Stamped BEFORE the await, so a publish that is slow or that fails
        // cannot let the next tick fire a second beacon a few seconds later. It
        // is also this device's re-entrancy guard: a publish outlasting the tick
        // interval reads as throttled on the next pass rather than doubling up.
        lastBeaconAtMs[groupIdHex] = now
        do {
            try await publishApplicationMessage(
                groupIdHex: groupIdHex,
                plaintextBytes: composeCommitterBeacon()
            )
        } catch {
            NSLog("ADR-0045 committer beacon not emitted: \(error)")
        }
        return .emit
    }

    // MARK: - ADR-0045 read-only accessors for the members sheet

    /// This device's ADR-0045 committer rank for `groupIdHex`: `0` for the
    /// owner, `k + 1` for the k-th admin, and — ADR-0050 2.5a — a rank after
    /// the admins for a plain member ONLY while its pending store holds a
    /// device admission or its own account's self-service proposal it could
    /// sweep. Every leaf of a ranked account holds the account's rank (item 6);
    /// which device of the account acts is the beacon fold's tie-break by
    /// signature key. `nil` otherwise, and for a group with no role table. `nil`
    /// and `0` are never interchangeable, and a rank is NOT a role: gate
    /// owner-or-admin decisions on ``ownRole(groupIdHex:)``.
    public func ownCommitterRank(groupIdHex: String) -> UInt32? {
        guard let identity = canonicalIdentity, let record = groups[groupIdHex] else { return nil }
        return try? record.group.ownCommitterRank(identity: identity)
    }

    /// ADR-0045 — `true` while a staged sweep on `groupIdHex` is still awaiting
    /// its journal verdict. Such a sweep is DURABLE and freezes this group's own
    /// membership operations until it is adjudicated, so it is worth surfacing.
    /// A torn read answers `false`, which is what a caller can act on.
    public func hasStagedSweep(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex] else { return false }
        return (try? record.group.hasStagedSweep()) ?? false
    }

    /// The core's retained pending-proposal set for `groupIdHex` — the truth
    /// about *what* is pending. Empty when there is no live handle.
    public func pendingProposals(groupIdHex: String) -> [ProposalRecord] {
        guard let record = groups[groupIdHex] else { return [] }
        return (try? record.group.pendingProposals()) ?? []
    }

    /// The signed-in account id, base64 — the first half of a per-`(account,
    /// group)` scope key (ADR-0045 UX §7). `nil` before bootstrap.
    public var accountIdB64: String? { currentAccountIdB64 }

    /// ADR-0045 S4a — the pending membership requests for `groupIdHex`, joined
    /// from the core's live set and this device's observation store, with the
    /// store pruned against the live set in the same pass.
    public func pendingRequests(groupIdHex: String) -> [PendingRequest] {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return [] }
        let groupIdB64 = base64Encode(record.groupId)
        let observations = pendingRequestStore.load(
            accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        let (requests, liveKeys) = foldPendingRequests(
            records: (try? record.group.pendingProposals()) ?? [],
            observations: observations,
            nowMs: Self.nowMs()
        )
        // ADR-0050 2.5a — an own request that left the pending set without
        // landing is no longer detected here and listed. The superseded
        // re-request (``reRequestSupersededIfDue(groupIdHex:)``) works from the
        // durable INTENT recorded when the request was proposed, which this
        // prune does not touch, and lists a request in `Not applied` only once
        // re-requesting it stops.
        pendingRequestStore.prune(
            accountIdB64: accountIdB64, groupIdB64: groupIdB64, liveKeys: liveKeys)
        return requests
    }

    /// `docs/ux/conversation-info.md` section 2.4 — the people (account keys)
    /// with an add waiting for the sweep, so the Add members screen does not
    /// list them again: this device's own add requests whose device proposals
    /// are still pending, and any pending add whose leaf key is the person's.
    public func pendingAddPersonKeys(groupIdHex: String) -> Set<Data> {
        guard let record = groups[groupIdHex],
              let pending = try? record.group.pendingProposals() else { return [] }
        let leafKeys: Set<String> = Set(pending.compactMap {
            guard $0.stored, case .add(let key) = $0.kind else { return nil }
            return base64Encode(key)
        })
        guard !leafKeys.isEmpty else { return [] }
        let intents = currentAccountIdB64.map {
            pendingRequestStore.intents(accountIdB64: $0, groupIdB64: base64Encode(record.groupId))
        } ?? []
        return AddMembersRules.pendingAddPersonKeys(pendingAddLeafKeysB64: leafKeys, intents: intents)
    }

    /// ADR-0045 S7 residual 1 / ADR-0050 2.5a — the `Not applied` rows the
    /// user has not dismissed, recorded by the superseded re-request when it
    /// stops.
    public func unappliedAdds(groupIdHex: String) -> [UnappliedAdd] {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return [] }
        return unappliedRequestStore.load(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId))
    }

    /// Dismiss one `Not applied` row by its ``UnappliedAdd/rowKey``. It stays
    /// dismissed.
    public func dismissUnapplied(groupIdHex: String, rowKey: String) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        unappliedRequestStore.dismiss(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            addSignatureKeyB64: rowKey)
    }

    // MARK: - ADR-0050 2.5a — the superseded re-request

    /// Record what this device asked for, at the epoch it asked at. Called by
    /// every propose path once the proposal is published — the user's and the
    /// re-request's alike, so a re-request refreshes the epoch of its own slot.
    func recordMembershipIntent(groupIdHex: String, _ request: MembershipIntent.Request) {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex],
              let epoch = try? record.group.epoch() else { return }
        pendingRequestStore.recordIntent(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            MembershipIntent(request: request, epoch: epoch))
    }

    /// The group as the superseded verdict reads it, in one pass. `nil` on a
    /// torn read of the epoch, the role table or the pending set — the caller
    /// then does nothing, which is the only safe answer to "I could not see".
    private func supersededSnapshot(groupIdHex: String) -> SupersededSnapshot? {
        guard let record = groups[groupIdHex] else { return nil }
        do {
            let epoch = try record.group.epoch()
            let table = try record.group.groupRoles()
            let pending = try record.group.pendingProposals()
            let leaves = (try? record.group.members())?.map {
                SupersededLeaf(
                    leafIndex: $0.leafIndex,
                    leafKeyB64: base64Encode($0.signaturePublicKey),
                    personKeyB64: base64Encode(PersonKey.of($0)))
            }
            return SupersededSnapshot(
                epoch: epoch,
                leaves: leaves,
                roleTable: table.map {
                    RetryRoleTable(
                        ownerKeyB64: base64Encode($0.owner),
                        adminKeysB64: $0.admins.map { base64Encode($0) })
                },
                pendingKeys: pending.map { proposalKey($0) },
                membershipEnded: membershipEnded(membershipStateNow(groupIdHex: groupIdHex)),
                retainedJoinKeys: retainedExternalJoinKeys(groupIdHex: groupIdHex)?
                    .map { base64Encode($0) })
        } catch {
            return nil
        }
    }

    /// Fire-and-forget form of ``reRequestSupersededIfDue(groupIdHex:)``.
    func scheduleSupersededReRequest(groupIdHex: String) {
        Task { @MainActor [weak self] in
            await self?.reRequestSupersededIfDue(groupIdHex: groupIdHex)
        }
    }

    /// ADR-0050 2.5a point 4 — after an observed merged commit, re-request
    /// every own request that commit superseded.
    ///
    /// Runs at most once at a time per group, and only when the group's epoch
    /// differs from the epoch last evaluated: no commit, no attempt. The loop
    /// re-reads the epoch after each pass, so a commit that lands DURING a pass
    /// (this device's own sweep of a re-request included) is evaluated rather
    /// than lost to the in-flight guard; every further pass needs a further
    /// commit, so the loop cannot spin.
    func reRequestSupersededIfDue(groupIdHex: String) async {
        guard !supersededRunning.contains(groupIdHex) else { return }
        supersededRunning.insert(groupIdHex)
        let bound = binding.capture()
        defer { if bound.isCurrent { supersededRunning.remove(groupIdHex) } }
        while bound.isCurrent,
              let record = groups[groupIdHex],
              let epoch = try? record.group.epoch(),
              supersededTrigger.take(groupIdHex: groupIdHex, epoch: epoch) {
            await evaluateSupersededIntents(groupIdHex: groupIdHex)
        }
    }

    private func evaluateSupersededIntents(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let intents = pendingRequestStore.intents(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        if intents.isEmpty { return }
        var reRequested = false
        // The intents are this binding's account's: none is re-requested in,
        // or settled against, the group of an account that signed in while
        // an earlier one was out.
        let bound = binding.capture()
        for intent in intents {
            guard bound.isCurrent else { return }
            // Re-read per intent: an earlier re-request in this pass changed
            // the pending set.
            guard let snapshot = supersededSnapshot(groupIdHex: groupIdHex) else { return }
            switch supersededResolution(supersededVerdict(intent, snapshot)) {
            case .keep:
                continue
            case .forget:
                pendingRequestStore.removeIntent(
                    accountIdB64: accountIdB64, groupIdB64: groupIdB64, slot: intent.slot)
            case .forgetAndList:
                stopReRequest(intent, groupIdB64: groupIdB64, list: true)
            case .reRequest:
                do {
                    let reRequest = try await performReRequest(intent, groupIdHex: groupIdHex)
                    try bound.require()
                    switch reRequest {
                    case .proposed:
                        reRequested = true
                    case .subjectGone:
                        stopReRequest(intent, groupIdB64: groupIdB64, list: false)
                    case .nothingProposed:
                        // The propose path found the effect already true under
                        // its lock; the next evaluation clears the intent.
                        break
                    }
                } catch {
                    guard bound.isCurrent else { return }
                    let resolution = reRequestFailureResolution(error) {
                        membershipRefusalDisposition(reason: $0)
                    }
                    guard resolution == .forgetAndList else {
                        NSLog("ADR-0050 2.5a: re-request kept for the next commit: \(error)")
                        continue
                    }
                    // A refusal that raced the effect landing is not a failure:
                    // re-judge on fresh state before listing.
                    let fresh = supersededSnapshot(groupIdHex: groupIdHex)
                        .map { supersededVerdict(intent, $0) }
                    let silent = fresh == .landed || fresh == .subjectGone
                    stopReRequest(intent, groupIdB64: groupIdB64, list: !silent)
                }
            }
        }
        guard bound.isCurrent else { return }
        if reRequested {
            // The same step every user path takes after proposing: this device
            // may be the acting committer.
            await sweepIfCommitter(groupIdHex: groupIdHex)
        }
    }

    private func stopReRequest(_ intent: MembershipIntent, groupIdB64: String, list: Bool) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        pendingRequestStore.removeIntent(
            accountIdB64: accountIdB64, groupIdB64: groupIdB64, slot: intent.slot)
        if list {
            unappliedRequestStore.record(
                accountIdB64: accountIdB64, groupIdB64: groupIdB64,
                items: [unappliedRow(for: intent)])
        }
    }

    private enum ReRequestOutcome {
        case proposed
        case nothingProposed
        case subjectGone
    }

    /// Re-derive one request from its intent against the current epoch and
    /// publish it through the SAME propose function the user's action uses
    /// (same commit lock, same ADR-0040 / ADR-0057 steps). Each of those
    /// records the intent again at the new epoch.
    private func performReRequest(
        _ intent: MembershipIntent, groupIdHex: String
    ) async throws -> ReRequestOutcome {
        func outcome(_ result: MembershipOutcome?) -> ReRequestOutcome {
            if case .proposed? = result { return .proposed }
            return .nothingProposed
        }
        func leafOf(person: String) -> UInt32? {
            guard let record = groups[groupIdHex] else { return nil }
            return (try? record.group.members())?
                .first { base64Encode(PersonKey.of($0)) == person }?.leafIndex
        }
        switch intent.request {
        case .add(let person, let account, _):
            guard let key = base64Decode(person), let id = base64Decode(account) else {
                return .subjectGone
            }
            // Fresh KeyPackages: they are single-use, and the dropped proposal
            // already named the old ones.
            return outcome(try await addMemberToGroup(
                groupIdHex: groupIdHex, recipientAccountId: id, recipientSignaturePubkey: key))
        case .approveJoin(let joiner):
            guard let ref = base64Decode(joiner) else { return .subjectGone }
            _ = try await approveExternalJoin(groupIdHex: groupIdHex, proposalRef: ref)
            return .proposed
        case .remove(let person), .leave(let person):
            // Re-resolved by key: the leaf the dropped proposal named may since
            // belong to somebody else.
            guard let leaf = leafOf(person: person) else { return .subjectGone }
            return outcome(try await removeMemberFromGroup(groupIdHex: groupIdHex, leafIndex: leaf))
        case .setAdmin(let person, let makeAdmin):
            guard let key = base64Decode(person) else { return .subjectGone }
            return outcome(try await setMemberAdmin(
                groupIdHex: groupIdHex, memberSignaturePubkey: key, makeAdmin: makeAdmin))
        case .transferOwnership(let to, let outgoing):
            guard let key = base64Decode(to) else { return .subjectGone }
            return outcome(try await transferGroupOwnership(
                groupIdHex: groupIdHex, newOwnerSignaturePubkey: key, outgoing: outgoing))
        case .ownDeviceAdd(let device):
            guard let accountId = currentAccountId, let key = base64Decode(device) else {
                return .subjectGone
            }
            // The device's current KeyPackage; none in the directory means the
            // device is gone, which ends the request.
            guard let keyPackage = try await discovery.fetchKeyPackages(accountId: accountId)?
                .first(where: { $0.deviceId == key })?.keyPackage
            else { return .subjectGone }
            return outcome(try await admitDeviceToGroup(groupIdHex: groupIdHex, keyPackage: keyPackage))
        case .ownDeviceRemove(let device):
            guard let key = base64Decode(device) else { return .subjectGone }
            return outcome(try await retireDeviceFromGroup(groupIdHex: groupIdHex, deviceKey: key))
        }
    }

    /// ADR-0045 S4a — record that a peer's membership proposal landed, stamped
    /// with the DS row's time so the quiet window means the same thing on every
    /// device. A record the receipt gate REFUSED (`stored == false`) is recorded
    /// nowhere and rendered nowhere: no sweep will ever commit it, so it is not
    /// a pending request.
    func recordPendingRequestObservation(
        groupIdHex: String,
        record proposal: ProposalRecord,
        createdAt: String
    ) {
        guard proposal.stored else { return }
        guard let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex],
              let atMs = Self.parseDsTimestampMs(createdAt) else { return }
        pendingRequestStore.record(
            accountIdB64: accountIdB64,
            groupIdB64: base64Encode(record.groupId),
            key: proposalKey(proposal),
            atMs: atMs,
            own: false
        )
    }

    /// Milliseconds since the Unix epoch, as one seam so the beacon age and the
    /// quiet window read the same clock.
    nonisolated static func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }

    /// Parse a DS row's server-assigned `created_at` into milliseconds since the
    /// Unix epoch. `nil` when it does not parse — the caller must DROP the row
    /// rather than substitute a local clock, which is the whole point of reading
    /// the server's stamp.
    ///
    /// The parse itself moved to ``DsTimestamp``, a Foundation-only file the
    /// notification-service extension also compiles: the extension has to read
    /// the same age for the same ring decision, and a second copy of this
    /// could disagree with the app about how old a call is.
    nonisolated static func parseDsTimestampMs(_ raw: String) -> Int64? {
        DsTimestamp.parseMs(raw)
    }

    // MARK: - ADR-0040 (G-35) per-group commit serialization

    /// Run `body` as the sole in-flight MLS-mutating section for one group.
    ///
    /// Five sections take this chain: the four group-membership create-commit
    /// paths, and — since G-38 — the APPLY half of
    /// ``pollGroup(groupIdHex:waitSecs:)`` (``applyFetchedBatch(groupIdHex:after:raw:)``).
    /// The poll belongs here because it is not a read-only observer: it feeds
    /// incoming commits to `processIncomingVerifyingRoles`, which mutates the
    /// shared OpenMLS provider through the very `Group` handle an in-flight
    /// commit is mid-operation on. Only the poll's apply is inside; its DS fetch
    /// stays outside (see ``pollGroup(groupIdHex:waitSecs:)``).
    ///
    /// ## Why actor isolation is not enough
    ///
    /// Every create-commit entry point in `lakko-mls` (`setGroupRoles` /
    /// `addMemberFromWire` / `removeMember` / `approveExternalJoin`)
    /// EAGER-MERGES epoch N -> N+1 into the shared in-memory OpenMLS provider
    /// *inside* the create call, before the commit has been published. The
    /// region from that merge to the completed publish is therefore a critical
    /// section: a second commit started inside it reads the group at N+1 and
    /// publishes commit(N+1 -> N+2) alongside a still-in-flight
    /// commit(N -> N+1). The DS assigns journal order by arrival, not by which
    /// call started first, so the two can land inverted — peers at N drop the
    /// N+1 commit as wrong-epoch, apply the N one, and settle at N+1 while this
    /// device sits at N+2. That is precisely the "device-ahead" outcome
    /// ADR-0040 exists to prevent, and the pending-commit outbox does NOT
    /// recover it: both publishes succeeded, so both entries are acked and the
    /// restart sweep has nothing to re-publish.
    ///
    /// `ChatRepository` is `@MainActor`, which makes each individual statement
    /// atomic but releases isolation at every `await` — and the region above
    /// contains one (`dsClient.publishGroup`). Actor isolation prevents data
    /// races; it does not prevent reentrancy. This chain does.
    ///
    /// ## Contract for `body`
    ///
    /// `body` runs AFTER an unbounded wait, so it must RE-READ
    /// ``canonicalIdentity`` and ``groups`` rather than close over values
    /// captured before the wait: a predecessor may have REPLACED the
    /// `GroupRecord` (or evicted it entirely). Every `…Locked` method below
    /// re-derives both from `self` for exactly this reason.
    ///
    /// ## Deadlock audit
    ///
    /// The chain is not reentrant, so `body` must not call another
    /// commit-locked entry point. It does not: the locked sections reach only
    /// `Group` (synchronous UniFFI), ``persistCommitEntryThenSnapshotOrThrow(_:)``,
    /// ``ackCommitBestEffort(entryId:commitMessageId:secondMessageId:)``,
    /// `DsClient.publishGroup` / `publishMailbox`, and
    /// `ChatClient.markOwnPublishedCommit` — none of which call back in.
    /// ``redistributePushRoutingSecret(groupIdHex:)`` is reached from the add
    /// path but only SPAWNS a detached `Task` and returns, so the routing lock
    /// it eventually takes is never awaited while this one is held. The
    /// ADR-0043 rotation half of ``removeMemberFromGroup(groupIdHex:leafIndex:)``
    /// is deliberately left OUTSIDE this lock (it publishes an application
    /// message, not a commit), so the two chains are never nested at all.
    ///
    /// G-38 re-audit, for the poll section: ``applyFetchedBatch(groupIdHex:after:raw:)``
    /// is NON-SUSPENDING, so it cannot await anything at all, let alone this
    /// lock. Its two follow-ups that do eventually take the ADR-0044 routing
    /// chain — ``receivePushRoutingSecret(groupIdHex:secret:mintEpoch:senderLeaf:)``
    /// and ``rotatePushRoutingSecretIfMembershipShrank(groupIdHex:previousMemberKeys:)``
    /// — only SPAWN a `Task` and return, so neither is awaited under this lock.
    /// The two entry points on the poll path that DO publish are both outside
    /// it: the DS fetch (by design, so a 30 s long poll blocks nothing) and
    /// ``sweepPendingCommits(record:)``, which re-publishes commits and would
    /// deadlock against the apply's acquisition if it were moved inside.
    ///
    /// ## Why the publish is not bounded by a timeout
    ///
    /// The ADR-0044 review required bounding network I/O taken under the
    /// push-routing lock, because there the work queued behind a wedged row is
    /// a rotation that must proceed. Here the inverse holds: the work queued
    /// behind is another COMMIT, and releasing the lock while a publish's fate
    /// is unknown re-opens the exact device-ahead hazard the lock closes. The
    /// publish is bounded anyway by `URLSession`'s own request timeout, which
    /// surfaces as a thrown publish failure with the outbox entry left un-acked
    /// — the state the restart sweep is built to recover.
    ///
    /// ## A region belongs to the account that queued it
    ///
    /// The chain is not cleared on sign-out (see ``wipeInMemoryState()``), and
    /// a region re-reads `canonicalIdentity` and `groups` when its turn comes.
    /// That stops it once its account has signed out. It does not stop it
    /// when ANOTHER account has signed in meanwhile and is in the same group:
    /// the re-read then finds that account's identity and record, and the
    /// region (a send, a membership change, a sweep) would run as it. So a
    /// region whose binding moved while it waited is not run at all.
    func withCommitLock<T>(
        groupIdHex: String,
        _ body: @escaping @MainActor () async throws -> T
    ) async throws -> T {
        let bound = binding.capture()
        return try await commitLock.run(groupIdHex: groupIdHex) {
            try bound.require()
            return try await body()
        }
    }

    /// The check a region on the commit chain repeats after every wait inside
    /// it. ``withCommitLock(groupIdHex:_:)`` checks the binding once, when the
    /// region's turn comes; a region that then waits on the network (a
    /// registration, a publish) can be outlived by its account in the middle.
    /// What follows such a wait re-reads `canonicalIdentity`, `groups` and
    /// `currentAccountIdB64`, so it would record, acknowledge or apply under
    /// whoever is signed in by then.
    ///
    /// `bound` is captured at the top of the region's helper, before its
    /// first wait. A region stopped here is in the state a crash at the same
    /// point leaves, which the outbox sweep and the owed sweep verdict of the
    /// account's next session already resolve (ADR-0040, ADR-0045).
    private func requireCommitRegion(_ bound: BindingEpoch.Captured) throws {
        try bound.require()
    }

    /// Observe every moment a group's MLS chain DRAINS: the last queued
    /// region for that group has settled and nothing is queued behind it. See
    /// ``GroupMlsLock`` for why only a drain is reported. The app-level group
    /// call host uses it to rotate a live call's SFrame key after an epoch
    /// advance applied by ANY receive path (G-18 / ADR-0029).
    @discardableResult
    func observeGroupMlsIdle(_ observer: @escaping @MainActor (_ groupIdHex: String) -> Void) -> UUID {
        commitLock.observeIdle(observer)
    }

    func removeGroupMlsIdleObserver(_ token: UUID) {
        commitLock.removeIdleObserver(token)
    }

    // MARK: - ADR-0053 DS journal access

    /// The current-epoch access signer of a live group, recorded in
    /// ``dsSigners`` so the epoch before it stays available for a retry.
    /// `nil` before bootstrap or when the exporter refuses (an inactive group).
    private func currentDsSigner(record: GroupRecord, groupIdHex: String) -> DsProofSigner? {
        guard let identity = canonicalIdentity,
              let signer = try? record.group.dsAccessSigner(identity: identity) else { return nil }
        dsSigners.observe(journal: groupIdHex, signer: signer)
        return signer
    }

    /// ADR-0053 client contract points 1 and 5 — the auth for a request to
    /// this group's journal: the current-epoch signer, the previous one when
    /// this device still holds it, and a claim-on-open as the last retry.
    /// `nil` when the group is unknown here (the request goes unsigned, which
    /// only an unclaimed journal accepts).
    func dsAuth(groupIdHex: String) -> DsAuth? {
        guard let record = groups[groupIdHex],
              let signer = currentDsSigner(record: record, groupIdHex: groupIdHex) else { return nil }
        let epoch = signer.epoch()
        // The request this signs is the bound account's. Its retries and its
        // repair note are not run against the journal state of an account
        // that signed in while the request was out.
        let bound = binding.capture()
        return DsAuth(
            signer: signer,
            previous: dsSigners.previousSigner(journal: groupIdHex, before: epoch),
            reclaim: { [weak self] in
                guard bound.isCurrent else { return nil }
                return await self?.claimGroupJournal(groupIdHex: groupIdHex)
            },
            // Second security pass, finding 1 — refused at this device's own
            // epoch after every retry: an owner or admin schedules the repair
            // check (a member's note is dropped there).
            onRefusedAtHead: { [weak self] in
                Task { @MainActor in
                    guard bound.isCurrent else { return }
                    self?.journalRepair.note(groupIdHex: groupIdHex, epoch: epoch, reason: .refusedAtHead)
                }
            }
        )
    }

    /// ADR-0057 second pass, finding 1 — poll `groupIdHex` to its journal
    /// head, applying everything: one page per poll, until the cursor stops
    /// moving (at most ``JournalRepair/maxPolls``). Throws the DS refusal.
    func pollToHead(groupIdHex: String) async throws {
        for _ in 0..<JournalRepair.maxPolls {
            guard let before = groups[groupIdHex]?.cursor else { return }
            try await pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
            guard let after = groups[groupIdHex]?.cursor, after != before else { return }
        }
    }

    // MARK: - ADR-0057 journal ownership

    /// Is this account the group's ADR-0022 owner, i.e. does it hold the
    /// journal's root? A group with no role table has no owner.
    private func isJournalOwner(record: GroupRecord) -> Bool {
        guard let identity = canonicalIdentity else { return false }
        return (try? record.group.ownRole(identity: identity)) == .owner
    }

    /// The root signer of a group's journal, derived from the ACCOUNT identity.
    func journalRootSigner(groupId: Data) throws -> DsStatementSignerProtocol {
        guard let accountIdentity else {
            throw ChatError.argument("no account identity — cannot derive the journal root")
        }
        return try dsJournalRootSigner(accountIdentity: accountIdentity, journalId: groupId)
    }

    /// The key this device registers the group's epochs with (obligation 5):
    /// the root for the owner, the stored registrar seed otherwise, `nil` when
    /// this device holds neither — it must not commit then.
    private func journalAuthority(record: GroupRecord, groupIdHex: String) -> JournalAuthority? {
        JournalAuthority.resolve(
            isOwner: isJournalOwner(record: record),
            rootHeld: currentAccountIdB64.map {
                registrarSeeds.isRootHeld(accountIdB64: $0, groupIdHex: groupIdHex)
            } ?? false,
            rootSigner: { try self.journalRootSigner(groupId: record.groupId) },
            registrar: {
                guard let accountIdB64 = self.currentAccountIdB64,
                      let held = self.registrarSeeds.get(
                          accountIdB64: accountIdB64, groupIdHex: groupIdHex),
                      let signer = try? dsStatementSigner(seed: held.seed) else { return nil }
                return (signer, held.rank)
            })
    }

    /// Whether this device can register the group's next epoch, i.e. whether
    /// it may stage a sweep at all.
    func canRegisterJournalEpochs(groupIdHex: String) -> Bool {
        guard let record = groups[groupIdHex] else { return false }
        return journalAuthority(record: record, groupIdHex: groupIdHex) != nil
    }

    /// The registrar public key the owner appointed for `accountKey` on this
    /// journal — what a removal's registration revokes. Root only.
    private func registrarPublicKey(groupId: Data, accountKey: Data) -> Data? {
        guard let accountIdentity,
              let rootSeed = try? dsJournalRootSeed(
                  accountIdentitySeed: accountIdentity.ed25519Seed(), journalId: groupId),
              let seed = try? dsJournalRegistrarSeed(rootSeed: rootSeed, memberAccountKey: accountKey),
              let signer = try? dsStatementSigner(seed: seed) else { return nil }
        return signer.publicKey()
    }

    /// ADR-0057 obligation 2 — the owner's lazy claim, run as the last retry
    /// of a request its journal refused with `401`: claim the journal with the
    /// root at the current epoch (it was unclaimed). A journal claimed already
    /// (`409`) is left alone — nothing is registered, because a root
    /// registration replaces every epoch at and above it and a lagging owner
    /// device would cut the others off. A member never claims. Returns the
    /// current signer to retry with only when the claim landed; `nil`
    /// otherwise (this device waits for the committer's registration).
    @discardableResult
    func claimGroupJournal(groupIdHex: String) async -> DsRequestSigner? {
        let (outcome, signer) = await claimJournalAsOwner(groupIdHex: groupIdHex)
        switch outcome {
        case .claimed:
            reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
            return signer
        case .alreadyClaimed, .notOwner, .failed:
            return nil
        }
    }

    /// The owner's claim of the group journal at the group's CURRENT epoch,
    /// shared by the lazy claim and the claim on open. A non-owner sends
    /// nothing. A landed claim sets the root-held marker and clears the sweep
    /// registration backoff (the refusal it backed off from is gone).
    private func claimJournalAsOwner(
        groupIdHex: String
    ) async -> (JournalOwnership.LazyClaimOutcome, DsProofSigner?) {
        guard let record = groups[groupIdHex] else { return (.failed, nil) }
        guard isJournalOwner(record: record) else { return (.notOwner, nil) }
        guard let signer = currentDsSigner(record: record, groupIdHex: groupIdHex) else {
            return (.failed, nil)
        }
        let bound = binding.capture()
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: true,
            root: { try self.journalRootSigner(groupId: record.groupId) },
            journalId: record.groupId,
            epoch: signer.epoch(),
            epochKey: signer.verifyingKey(),
            ds: dsClient)
        // The claim was made with this binding's root. The mark below must
        // not tell an account that signed in meanwhile that IT holds the
        // root of this journal.
        guard bound.isCurrent else { return (.failed, nil) }
        if outcome == .claimed {
            if let accountIdB64 = currentAccountIdB64 {
                registrarSeeds.markRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            }
            sweepRegistrationBackoff[groupIdHex] = nil
            NSLog("ADR-0057: the owner claimed the group journal on open")
        }
        return (outcome, signer)
    }

    /// Live walk 2026-09-29 — the owner's claim on open, at most once per
    /// group per session (``JournalClaimOnOpen``), followed by the registrar
    /// reconcile whatever the claim came to (a non-owner's reconcile returns at
    /// once; an owner whose journal was claimed already probes for the root).
    /// Awaited before every epoch registration of the group, with the group
    /// still at the epoch it is registering FROM, so the claim never names an
    /// epoch no member reached.
    func ensureOwnerClaimOnOpen(groupIdHex: String) async {
        await claimOnOpen.ensure(journalIdHex: groupIdHex) { [weak self] in
            guard let self else { return .failed }
            let (outcome, _) = await self.claimJournalAsOwner(groupIdHex: groupIdHex)
            self.reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
            return outcome
        }
    }

    /// ADR-0081 decision 1 over ADR-0057 obligation 5 — register the epoch a
    /// commit moves to with this device's root or registrar key AND append the
    /// commit row, in one request (`POST .../commit`) proven by `proof`, the
    /// key of the epoch the commit leaves.
    /// `evictedAccountKeys` are the accounts the commit removes; their
    /// registrar keys are revoked in the same statement — derived by the root,
    /// read from the owner's seed envelopes by an admin (strictly lower ranks
    /// only, see ``JournalOwnership/revokeList(authority:evictedAccountKeys:registrarPublicKey:knownRegistrar:equalOrHigherAccounts:)``).
    /// Throws: a device with no authority, or a refused commit, aborts it
    /// (``DsStatementError/conflict`` when another commit owns the epoch).
    @discardableResult
    private func commitNextEpoch(
        record: GroupRecord,
        groupIdHex: String,
        next: DsGroupSigner,
        proof: DsRequestSigner,
        row: Data,
        evictedAccountKeys: [Data] = []
    ) async throws -> DsCommitAnswer {
        guard let authority = journalAuthority(record: record, groupIdHex: groupIdHex) else {
            throw ChatError.argument(
                "this device holds neither the journal root nor a registrar seed — cannot commit")
        }
        // ADR-0057 amended 2026-10-01 — an ADMIN that commits a removal
        // revokes the removed accounts' registrar keys too, from the keys the
        // owner's seed envelopes carried in the clear; the live role table
        // keeps an admin from listing the owner or another admin (the DS would
        // refuse the whole registration).
        let known = currentAccountIdB64.map {
            registrarSeeds.knownRegistrars(accountIdB64: $0, groupIdHex: groupIdHex)
        } ?? [:]
        let bound = binding.capture()
        var equalOrHigher: Set<Data> = []
        if case .registrar = authority, let roles = try? record.group.groupRoles() {
            equalOrHigher.insert(roles.owner)
            equalOrHigher.formUnion(roles.admins)
        }
        let revoke = JournalOwnership.revokeList(
            authority: authority,
            evictedAccountKeys: evictedAccountKeys,
            registrarPublicKey: { self.registrarPublicKey(groupId: record.groupId, accountKey: $0) },
            knownRegistrar: { known[$0] },
            equalOrHigherAccounts: equalOrHigher)
        let answer = try await JournalOwnership.commitEpoch(
            authority: authority,
            journalId: record.groupId,
            epoch: next.epoch(),
            epochKey: next.verifyingKey(),
            revoke: revoke,
            row: row,
            proof: proof,
            ds: dsClient,
            // Second security pass, finding 1 — a 409: the commit is
            // abandoned by the throw (never a Replace here); an owner or admin
            // schedules the repair check at the epoch it is at.
            onConflict: { [weak self] current in
                Task { @MainActor in
                    guard bound.isCurrent else { return }
                    self?.journalRepair.note(
                        groupIdHex: groupIdHex, epoch: current, reason: .appendConflict)
                }
            })
        // The signer memory is the bound session's (``wipeInMemoryState()``).
        try bound.require()
        dsSigners.observe(journal: groupIdHex, signer: next)
        return answer
    }

    /// The accounts the leaves in `removedLeaves` belong to, when every leaf of
    /// the account is removed — read from the roster BEFORE the commit.
    private func evictedAccountKeys(group: Group, removedLeaves: Set<UInt32>) -> [Data] {
        guard let members = try? group.members() else { return [] }
        return JournalOwnership.evictedAccountKeys(
            members: members.map { (leaf: $0.leafIndex, accountKey: $0.accountKey) },
            removedLeaves: removedLeaves)
    }

    /// ADR-0057 obligation 3, second security pass finding 2 — the owner
    /// converges the journal's registrar set to the roster: a `registrars-set`
    /// with the WHOLE desired set (admin for ADR-0022 admins, member for every
    /// other member account, never the owner itself) whenever it differs from
    /// the set this session last sent, so once per group per session and on
    /// every roster or role change; the DS drops every registrar not listed,
    /// whatever any device-local record says. Then every member whose seed
    /// envelope is owed gets it sealed to its account X25519 key; the local
    /// record only tracks which envelopes were delivered.
    ///
    /// Owner devices only, and only once this account is known to hold the
    /// root (finding 4): an owner without the marker probes first, at most
    /// every ``rootProbeRetryInterval``. Fire-and-forget, one run per group at
    /// a time. A member whose X25519 key cannot be resolved yet is left
    /// pending and retried on the next reconcile.
    func reconcileJournalRegistrarsSoon(groupIdHex: String) {
        guard !registrarReconcileInflight.contains(groupIdHex) else {
            registrarReconcileRerun.insert(groupIdHex)
            return
        }
        registrarReconcileInflight.insert(groupIdHex)
        // The in-flight and re-run marks are cleared with the session
        // (``wipeInMemoryState()``). A run that outlived it touches neither:
        // they are by then the next account's.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self else { return }
            repeat {
                guard bound.isCurrent else { return }
                self.registrarReconcileRerun.remove(groupIdHex)
                await self.reconcileJournalRegistrars(groupIdHex: groupIdHex)
                await self.reconcileKnockSeeds(groupIdHex: groupIdHex)
            } while bound.isCurrent && self.registrarReconcileRerun.contains(groupIdHex)
            if bound.isCurrent { self.registrarReconcileInflight.remove(groupIdHex) }
        }
    }

    /// How often an owner without the "root held" marker probes. Chosen, not
    /// measured; the web client's interval.
    private static let rootProbeRetryInterval: TimeInterval = 60

    /// Finding 4 — an owner that does not know it holds the root probes (a
    /// `root` statement naming its own root, signed by it); a 204 sets the
    /// marker. Returns whether the marker is set afterwards.
    private func probeRootIfUnheld(accountIdB64: String, groupIdHex: String, groupId: Data) async -> Bool {
        if registrarSeeds.isRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex) { return true }
        if let wait = rootProbeNotBefore[groupIdHex], Date() < wait { return false }
        rootProbeNotBefore[groupIdHex] = Date().addingTimeInterval(Self.rootProbeRetryInterval)
        guard let root = try? journalRootSigner(groupId: groupId) else { return false }
        do {
            if try await JournalOwnership.probeRoot(root: root, journalId: groupId, ds: dsClient) == .root {
                registrarSeeds.markRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                rootProbeNotBefore[groupIdHex] = nil
                NSLog("ADR-0057: root probe answered 204; this account holds the root")
                return true
            }
            NSLog("ADR-0057: root probe answered 401; still using the registrar seed")
        } catch {
            NSLog("ADR-0057: root probe failed; retried later")
        }
        return false
    }

    /// The registrar set the live roster and role table call for, when this
    /// account is the owner of an active group; `nil` otherwise.
    private func desiredRegistrars(record: GroupRecord) -> [Data: DsRegistrarRank]? {
        guard isJournalOwner(record: record),
              (try? record.group.isActive()) == true,
              let roles = try? record.group.groupRoles(),
              let members = try? record.group.members() else { return nil }
        return JournalOwnership.desiredRanks(
            memberAccountKeys: members.map(\.accountKey), owner: roles.owner, admins: roles.admins)
    }

    private func reconcileJournalRegistrars(groupIdHex: String) async {
        guard let record = groups[groupIdHex], let accountIdB64 = currentAccountIdB64,
              isJournalOwner(record: record) else { return }
        let groupId = record.groupId
        // The reconcile reads the roster, the authority and the account
        // identity again at every step, across its own waits. All of them
        // answer "nothing to do" once this binding's account has left, so no
        // step runs with the next account's root or record.
        let bound = binding.capture()
        guard await probeRootIfUnheld(accountIdB64: accountIdB64, groupIdHex: groupIdHex, groupId: groupId),
              bound.isCurrent else { return }
        do {
            _ = try await JournalOwnership.reconcileRegistrars(.init(
                journalId: groupId,
                ds: dsClient,
                desired: { [weak self] in
                    guard let self, bound.isCurrent, let current = self.groups[groupIdHex] else { return nil }
                    return self.desiredRegistrars(record: current)
                },
                authority: { [weak self] in
                    guard let self, bound.isCurrent, let current = self.groups[groupIdHex] else { return nil }
                    return self.journalAuthority(record: current, groupIdHex: groupIdHex)
                },
                registrarPublicKey: { [weak self] in
                    self?.registrarPublicKey(groupId: groupId, accountKey: $0)
                },
                lastSentSet: { [weak self] in self?.registrarSetsSent[groupIdHex] },
                noteSentSet: { [weak self] in self?.registrarSetsSent[groupIdHex] = $0 },
                // ADR-0057 amended 2026-10-01 — deliveries are recorded per
                // DEVICE, so a member's device that joined after its account's
                // envelope was posted is owed one at this reconcile.
                memberLeaves: { [weak self] in
                    guard let self, let current = self.groups[groupIdHex],
                          let members = try? current.group.members() else { return [:] }
                    var out: [Data: [Data]] = [:]
                    for member in members {
                        out[member.accountKey, default: []].append(member.signaturePublicKey)
                    }
                    return out
                },
                loadDelivered: { [registrarSeeds] in
                    registrarSeeds.delivered(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                },
                saveDelivered: { [registrarSeeds] in
                    registrarSeeds.saveDelivered(accountIdB64: accountIdB64, groupIdHex: groupIdHex, $0)
                },
                sendEnvelope: { [weak self] accountKey, rank in
                    guard let self, bound.isCurrent, let accountIdentity = self.accountIdentity,
                          let x25519 = await self.memberX25519PublicKey(accountKey: accountKey),
                          bound.isCurrent else {
                        return false
                    }
                    var rootSeed = try dsJournalRootSeed(
                        accountIdentitySeed: accountIdentity.ed25519Seed(), journalId: groupId)
                    defer { RootHandover.wipe(&rootSeed) }
                    var seed = try dsJournalRegistrarSeed(rootSeed: rootSeed, memberAccountKey: accountKey)
                    defer { RootHandover.wipe(&seed) }
                    let envelope = try composeRegistrarSeedEnvelope(
                        journalId: groupId, memberAccountKey: accountKey,
                        memberX25519PublicKey: x25519, rank: rank, registrarSeed: seed)
                    try await self.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
                    return true
                }))
        } catch {
            NSLog("ADR-0057: registrar reconcile failed, retried on the next change: %@",
                  String(describing: error))
        }
    }

    /// A member's account X25519 key, from its identity bundle: the account id
    /// comes from a contact record or the member's own profile (a claim), and
    /// the bundle is accepted only when it names the same account signature
    /// key the roster does — so a wrong claim finds no key rather than a
    /// stranger's. Cached per session.
    func memberX25519PublicKey(accountKey: Data) async -> Data? {
        if let cached = memberX25519Cache[accountKey] { return cached }
        let keyB64 = accountKey.base64EncodedString()
        var accountId: Data? = memberAccountIdHints[accountKey]
            ?? ProfileDirectory.shared.accountId(signatureKeyB64: keyB64)
        if accountId == nil {
            accountId = AcceptedContactsStore.load()
                .first(where: { $0.request.fromPubkey == accountKey })?.request.fromAccountId
        }
        guard let accountId, accountId.count == 16,
              let bundle = try? await discovery.identityFetch(accountId: accountId),
              Data(base64Encoded: bundle.signaturePubkey) == accountKey,
              let x25519 = Data(base64Encoded: bundle.x25519Pubkey), x25519.count == 32
        else { return nil }
        memberX25519Cache[accountKey] = x25519
        return x25519
    }

    /// ADR-0057 obligation 4 — a registrar-seed envelope arrived in the group.
    /// Every device of the addressed account opens and stores it (the seed is
    /// sealed to the ACCOUNT key every device holds), whichever device of the
    /// account read it first: nothing here is per device (ADR-0050 2.5a item
    /// 8). One dispatch (``RootHandover/receiveSeedEnvelope(rank:envelopeJournal:envelopeAddressee:groupId:ownAccountKey:senderAccountKey:owner:open:storeRegistrarSeed:takeOver:onDrop:)``)
    /// so a rank-root envelope has no path into the registrar-seed store:
    ///
    ///   * rank member / admin: kept only when it is addressed to this account
    ///     AND its MLS sender resolves, at receipt, to the group's owner
    ///     through the live role table (C-68); then opened with the account
    ///     identity and stored (a newer one replaces the older).
    ///   * rank root (ADR-0057 S2 finding 1, a root handover after an
    ///     ownership transfer): kept only when it names this journal, is
    ///     addressed to this account AND the live role table names this
    ///     account owner; no sender check (the DS refuses a seed that is not
    ///     the current root). The opened old root seed goes to a DETACHED
    ///     takeover (this region holds the group's MLS lock, which the
    ///     takeover's reconcile publishes under) and is never persisted.
    ///
    /// Everything else is dropped without a trace in the timeline.
    /// Synchronous: the apply region stays non-suspending.
    func receiveRegistrarSeed(groupIdHex: String, plaintext: Data, senderLeaf: UInt32?) {
        // ADR-0074 Stage A — a seed envelope is addressed and sealed to the
        // account key the group knows this person by.
        guard let record = groups[groupIdHex], let accountIdB64 = currentAccountIdB64,
              let accountIdentity = conversationAccountIdentity,
              let decoded = try? decodeRegistrarSeedEnvelope(plaintext: plaintext) else { return }
        let owner = (try? record.group.groupRoles())?.owner
        let senderAccountKey = senderLeaf.flatMap { leaf in
            (try? record.group.members())?.first(where: { $0.leafIndex == leaf })?.accountKey
        }
        // ADR-0057 amended 2026-10-01 — every member, addressee or not, records
        // account -> registrar public key (+ rank) from an envelope of this
        // journal whose MLS sender resolves to the owner: what this device
        // revokes when it commits that account's removal as an admin. Never a
        // root handover (its key is the root's, not a registrar's).
        if let registrarRank = Self.registrarRank(of: decoded.rank),
           decoded.journalId == record.groupId,
           let owner, let senderAccountKey, senderAccountKey == owner {
            registrarSeeds.noteRegistrar(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                accountKey: decoded.memberAccountKey,
                publicKey: decoded.registrarPublicKey, rank: registrarRank)
        }
        _ = RootHandover.receiveSeedEnvelope(
            rank: decoded.rank,
            envelopeJournal: decoded.journalId,
            envelopeAddressee: decoded.memberAccountKey,
            groupId: record.groupId,
            ownAccountKey: accountIdentity.signaturePublicKey(),
            senderAccountKey: senderAccountKey,
            owner: owner,
            open: { try openRegistrarSeedEnvelope(plaintext: plaintext, accountIdentity: accountIdentity) },
            storeRegistrarSeed: { [registrarSeeds] seed, rank in
                registrarSeeds.put(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, seed: seed, rank: rank)
            },
            takeOver: { [weak self, registrarSeeds] oldRootSeed in
                // Finding 4 — kept sealed under the device key until the
                // takeover lands or answers 401, so a failure or a restart
                // retries it.
                registrarSeeds.putPendingRootSeed(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, seed: oldRootSeed)
                self?.rootTakeoverNotBefore[groupIdHex] = nil
                self?.scheduleRootTakeover(groupIdHex: groupIdHex)
            },
            onDrop: { reason in NSLog("ADR-0057: %@", reason) })
    }

    /// The registrar rank a seed envelope appoints, `nil` for a root handover.
    nonisolated static func registrarRank(of rank: DsSeedEnvelopeRank) -> DsRegistrarRank? {
        switch rank {
        case .member: return .member
        case .admin: return .admin
        case .root: return nil
        }
    }

    // MARK: - ADR-0057 S2 finding 1 — the journal root moves with an ownership transfer

    /// The retry floor for a handover (and a takeover) whose last attempt did
    /// not settle. Chosen, not measured; the same interval Android waits
    /// between appointment attempts.
    private static let rootHandoverRetryInterval: TimeInterval = 60

    /// The role table's owner now, `nil` for a table-less group or a torn read.
    func roleTableOwner(groupIdHex: String) -> Data? {
        guard let record = groups[groupIdHex] else { return nil }
        return (try? record.group.groupRoles())?.owner
    }

    /// Report the role-table owner read `before` and `after` one commit apply
    /// or sweep merge. Called from inside the group's MLS lock, so it only
    /// records and schedules; it never awaits the network. When the owner
    /// moved away from this account: record the handover as owed (it survives
    /// a restart), forget which seed envelopes were delivered, drop the "root
    /// held" marker (the root is about to move) and schedule the send. Every
    /// other change is ignored.
    private func noteOwnerTransition(groupIdHex: String, before: Data?, after: Data?) {
        guard RootHandover.ownerMovedAwayFrom(
            own: conversationAccountIdentity?.signaturePublicKey(), before: before, after: after) != nil,
              let accountIdB64 = currentAccountIdB64 else { return }
        registrarSeeds.markHandoverOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        registrarSeeds.forgetDelivered(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        registrarSeeds.clearRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        registrarSetsSent[groupIdHex] = nil
        rootHandoverNotBefore[groupIdHex] = nil
        rootHandoverLastSentAt[groupIdHex] = nil
        NSLog("ADR-0057: ownership moved away; root handover owed")
        scheduleRootHandover(groupIdHex: groupIdHex)
    }

    /// Schedule the root handover for `groupIdHex` when it is owed, not
    /// already running and not waiting out a retry interval; and resume a
    /// kept takeover (a received root seed) the same way. Spawned, never
    /// awaited: the send publishes into the group under its MLS lock. Called
    /// at the poll tail, so on the first poll of a session and on group
    /// activity.
    func scheduleRootHandover(groupIdHex: String) {
        scheduleRootTakeover(groupIdHex: groupIdHex)
        guard let accountIdB64 = currentAccountIdB64,
              registrarSeeds.owedHandovers(accountIdB64: accountIdB64)
                  .contains(groupIdHex.lowercased()) else { return }
        if let wait = rootHandoverNotBefore[groupIdHex], Date() < wait { return }
        if rootHandoverInFlight.contains(groupIdHex) {
            rootHandoverRerun.insert(groupIdHex)
            return
        }
        rootHandoverInFlight.insert(groupIdHex)
        // As in ``reconcileJournalRegistrarsSoon(groupIdHex:)``.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self else { return }
            repeat {
                guard bound.isCurrent else { return }
                self.rootHandoverRerun.remove(groupIdHex)
                await self.rootHandoverOnce(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            } while bound.isCurrent && self.rootHandoverRerun.contains(groupIdHex)
            if bound.isCurrent { self.rootHandoverInFlight.remove(groupIdHex) }
        }
    }

    /// One step of an owed handover (sending side, finding 4): probe with this
    /// account's own root; `401` settles it, `204` (re-)sends the envelope at
    /// most every ``RootHandover/resendInterval``.
    private func rootHandoverOnce(accountIdB64: String, groupIdHex: String) async {
        guard registrarSeeds.owedHandovers(accountIdB64: accountIdB64)
                  .contains(groupIdHex.lowercased()),
              let record = groups[groupIdHex] else { return }
        if (try? record.group.isActive()) == false {
            // Evicted: there is no group left to post the seed into.
            registrarSeeds.clearHandoverOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            return
        }
        let groupId = record.groupId
        let ds = dsClient
        // The root seed handed over is derived from the bound account
        // identity when it is sent, after the probe and the key lookup have
        // waited. Once this binding's account has left, nothing is probed
        // with, or derived from, the next account's identity.
        let bound = binding.capture()
        let outcome = await RootHandover.runOwed(
            probe: { [weak self] in
                guard let self, bound.isCurrent else { throw StaleBindingError() }
                // The root is derived from the account identity whatever the
                // role table says: this is the OUTGOING owner's own root.
                let root = try self.journalRootSigner(groupId: groupId)
                return try await JournalOwnership.probeRoot(root: root, journalId: groupId, ds: ds)
            },
            lastSentAt: rootHandoverLastSentAt[groupIdHex],
            now: Date(),
            send: { [weak self] in
                guard let self, bound.isCurrent else { return .cancelled }
                return await self.sendRootHandoverEnvelope(
                    groupIdHex: groupIdHex, groupId: groupId, bound: bound)
            })
        guard bound.isCurrent else { return }
        if outcome == .sent { rootHandoverLastSentAt[groupIdHex] = Date() }
        if outcome.settlesObligation {
            registrarSeeds.clearHandoverOwed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            rootHandoverNotBefore[groupIdHex] = nil
            rootHandoverLastSentAt[groupIdHex] = nil
        } else {
            rootHandoverNotBefore[groupIdHex] = Date().addingTimeInterval(Self.rootHandoverRetryInterval)
        }
        NSLog("ADR-0057: owed root handover %@", String(describing: outcome))
    }

    /// Seal this account's root seed to the live owner and post it.
    private func sendRootHandoverEnvelope(
        groupIdHex: String, groupId: Data, bound: BindingEpoch.Captured
    ) async -> RootHandover.SendOutcome {
        await RootHandover.send(
            ownAccountKey: { [weak self] in
                bound.isCurrent ? self?.conversationAccountIdentity?.signaturePublicKey() : nil
            },
            liveOwner: { [weak self] in
                bound.isCurrent ? self?.roleTableOwner(groupIdHex: groupIdHex) : nil
            },
            ownerX25519: { [weak self] accountKey in
                await self?.memberX25519PublicKey(accountKey: accountKey)
            },
            rootSeed: { [weak self] in
                try bound.require()
                guard let account = self?.accountIdentity else {
                    throw ChatError.argument("no account identity — cannot derive the journal root")
                }
                var accountSeed = account.ed25519Seed()
                defer { RootHandover.wipe(&accountSeed) }
                return try dsJournalRootSeed(accountIdentitySeed: accountSeed, journalId: groupId)
            },
            compose: { newOwner, x25519, rootSeed in
                try composeRootHandoverEnvelope(
                    journalId: groupId, newOwnerAccountKey: newOwner,
                    newOwnerX25519PublicKey: x25519, rootSeed: rootSeed)
            },
            sendEnvelope: { [weak self] envelope in
                guard let self, bound.isCurrent else { throw StaleBindingError() }
                try await self.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
            })
    }

    /// Receiving side: run the new owner's takeover with the kept OLD root
    /// seed (``RegistrarSeedStore/pendingRootSeed(accountIdB64:groupIdHex:)``),
    /// detached from the locked receive region, when one is kept, none is
    /// running and a failed one is not waiting out its retry interval.
    private func scheduleRootTakeover(groupIdHex: String) {
        guard let accountIdB64 = currentAccountIdB64,
              groups[groupIdHex] != nil,
              !rootTakeoverInFlight.contains(groupIdHex),
              registrarSeeds.pendingRootSeed(accountIdB64: accountIdB64, groupIdHex: groupIdHex) != nil
        else { return }
        if let wait = rootTakeoverNotBefore[groupIdHex], Date() < wait { return }
        rootTakeoverInFlight.insert(groupIdHex)
        // As in ``reconcileJournalRegistrarsSoon(groupIdHex:)``.
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self, bound.isCurrent else { return }
            await self.runRootTakeover(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            if bound.isCurrent { self.rootTakeoverInFlight.remove(groupIdHex) }
        }
    }

    private func runRootTakeover(accountIdB64: String, groupIdHex: String) async {
        guard let record = groups[groupIdHex],
              var oldRootSeed = registrarSeeds.pendingRootSeed(
                  accountIdB64: accountIdB64, groupIdHex: groupIdHex) else { return }
        defer { RootHandover.wipe(&oldRootSeed) }
        let groupId = record.groupId
        let ds = dsClient
        // The takeover transfers the journal to THIS account's root, read
        // from the bound account identity at each step. Once this binding's
        // account has left it has no root to offer and is no longer the
        // owner here: the journal is never transferred to the root of an
        // account that signed in meanwhile.
        let bound = binding.capture()
        let outcome = await RootHandover.takeOver(
            journalId: groupId,
            oldRootSeed: oldRootSeed,
            ownRootPublicKey: { [weak self] in
                guard bound.isCurrent else { return nil }
                return (try? self?.journalRootSigner(groupId: groupId))?.publicKey()
            },
            stillOwner: { [weak self] in
                guard let self, bound.isCurrent, let current = self.groups[groupIdHex] else { return false }
                return self.isJournalOwner(record: current)
            },
            oldRootTransfer: { seed, newRoot in
                try dsStatementSigner(seed: seed).rootTransfer(journalId: groupId, newRoot: newRoot)
            },
            probeOwnRoot: { [weak self] in
                guard let self, bound.isCurrent else { throw StaleBindingError() }
                return try await JournalOwnership.probeRoot(
                    root: try self.journalRootSigner(groupId: groupId), journalId: groupId, ds: ds)
            },
            markRootHeld: { [registrarSeeds] in
                registrarSeeds.markRootHeld(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            },
            newRoot: { [weak self] in
                guard bound.isCurrent else { return nil }
                return try? self?.journalRootSigner(groupId: groupId)
            },
            desiredRegistrarEntries: { [weak self] in
                guard let self, bound.isCurrent, let current = self.groups[groupIdHex],
                      let desired = self.desiredRegistrars(record: current) else { return nil }
                return JournalOwnership.registrarSetEntries(
                    desired: desired,
                    registrarPublicKey: { self.registrarPublicKey(groupId: groupId, accountKey: $0) })
            },
            ds: ds,
            resetAppointments: { [weak self] in
                self?.registrarSeeds.forgetDelivered(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                self?.registrarSeeds.remove(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                self?.registrarSetsSent[groupIdHex] = nil
            },
            reconcile: { [weak self] in
                guard bound.isCurrent else { return }
                self?.reconcileJournalRegistrarsSoon(groupIdHex: groupIdHex)
            },
            log: { line in NSLog("ADR-0057: %@", line) })
        guard bound.isCurrent else { return }
        if outcome.settlesSeed {
            registrarSeeds.removePendingRootSeed(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            rootTakeoverNotBefore[groupIdHex] = nil
        } else {
            rootTakeoverNotBefore[groupIdHex] = Date().addingTimeInterval(Self.rootHandoverRetryInterval)
        }
        NSLog("ADR-0057: root takeover %@", String(describing: outcome))
    }

    // MARK: - ADR-0060 Welcome delivery

    /// The account keys of leaves this device fetched to propose or add, by
    /// leaf signature key (b64): what resolves an added leaf to its owner's
    /// mailbox before the add is merged.
    private var addedLeafAccounts: [String: Data] = [:]

    /// Remember the owner of every leaf in `keyPackages` (ADR-0050 binding).
    func noteAddedLeafAccounts(keyPackages: [Data]) {
        for kp in keyPackages {
            guard let leaf = try? keyPackageSignatureKey(keyPackage: kp),
                  let account = try? keyPackageAccountKey(keyPackage: kp) else { continue }
            addedLeafAccounts[base64Encode(leaf)] = account
        }
    }

    /// ADR-0060 section 6 — post a committer-delegation envelope for the
    /// person `addedAccountKey` in `groupIdHex`: one delegation per committer
    /// candidate (the owner, then the admins), signed with the grant this
    /// device holds for them. Best effort; with no grant held there is nothing
    /// to delegate and the committer must hold its own.
    ///
    /// ADR-0060 section 6 — with no grant held and `openMailbox` given (a
    /// person found in Everyone), the envelope carries NO delegation and
    /// names that mailbox, so whichever candidate commits may write the
    /// Welcome there without a proof.
    ///
    /// A published envelope is noted on the Add's intent
    /// (``MembershipIntent/lastEnvelope``), which is what
    /// ``resendCommitterEnvelopesIfDue(groupIdHex:)`` reads. One at a time per
    /// (group, person).
    func sendCommitterDelegations(
        groupIdHex: String, addedAccountKey: Data, openMailbox: MailboxTarget? = nil
    ) async {
        let personKeyB64 = base64Encode(addedAccountKey)
        let inFlightKey = groupIdHex + "|" + personKeyB64
        guard let record = groups[groupIdHex],
              let candidates = committerCandidates(record: record),
              committerEnvelopesInFlight.insert(inFlightKey).inserted else { return }
        let bound = binding.capture()
        defer { if bound.isCurrent { committerEnvelopesInFlight.remove(inFlightKey) } }
        let reach = committerEnvelopeReach()
        let signed = reach.composeDelegationEnvelope(
            addedAccountKey: addedAccountKey, candidateAccountKeys: candidates,
            groupIdHex: groupIdHex)
        guard let envelope = CommitterDelegationChoice.envelope(
            signed: signed?.wire,
            open: openMailbox.flatMap { reach.composeOpenEnvelope(target: $0) })
        else { return }
        do {
            try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
        } catch {
            NSLog("ADR-0060: committer delegations not sent: \(error)")
            return
        }
        guard bound.isCurrent, let accountIdB64 = currentAccountIdB64 else { return }
        pendingRequestStore.recordCommitterEnvelope(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId),
            personKeyB64: personKeyB64,
            SentCommitterEnvelope(
                sentAtMs: committerEnvelopeClock(), signed: signed != nil,
                coveredB64: (signed?.covered ?? candidates).map(base64Encode)))
    }

    /// The committer candidates of a group as an envelope names them: the
    /// owner, then the admins. `nil` for a group with no role table. The one
    /// list the first send, a re-send and the due rule all read.
    private func committerCandidates(record: GroupRecord) -> [Data]? {
        guard let table = try? record.group.groupRoles() else { return nil }
        return committerEnvelopeReach().delegationCandidates([table.owner] + table.admins)
    }

    /// ADR-0060 section 6, "An add a contact proposed" — a delegation lives
    /// less than a day and a proposal has no deadline, so the proposing device
    /// re-sends its committer envelope while its Add waits: at the tail of a
    /// foreground poll, for each Add this device proposed that is still
    /// pending (``ownAddStillWaiting(_:_:)``), when the core's rule says one
    /// is due. Fresh delegations signed with the grant held for the person,
    /// or the recorded open binding with no delegation, composed by the same
    /// function as the first send. With neither, nothing is signed and
    /// nothing goes out: all a poll costs then is the two lookups that found
    /// nothing.
    ///
    /// Who stops it is the intent: when the Add lands or is dropped the
    /// superseded evaluation forgets or replaces it, and the record with it.
    func resendCommitterEnvelopesIfDue(groupIdHex: String) async {
        guard committerEnvelopeForeground(),
              let accountIdB64 = currentAccountIdB64,
              let record = groups[groupIdHex] else { return }
        let intents = pendingRequestStore.intents(
            accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId))
        let bound = binding.capture()
        for intent in intents {
            guard case .add(let personB64, _, _) = intent.request,
                  let person = base64Decode(personB64) else { continue }
            // Re-read per intent: the send before this one was a wait.
            guard bound.isCurrent,
                  let current = groups[groupIdHex],
                  let snapshot = supersededSnapshot(groupIdHex: groupIdHex),
                  let candidates = committerCandidates(record: current) else { return }
            guard ownAddStillWaiting(intent, snapshot),
                  committerEnvelopeIsDue(
                      intent.lastEnvelope, candidates: candidates, nowMs: committerEnvelopeClock())
            else { continue }
            await sendCommitterDelegations(
                groupIdHex: groupIdHex, addedAccountKey: person,
                openMailbox: committerEnvelopeReach().openWelcomeTarget(
                    ofPerson: person, groupIdHex: groupIdHex))
        }
    }

    /// Keep the `reply` grant of a `/g/` join attribution for this group,
    /// filed under the joiner's account key (its pending KeyPackage's binding).
    /// What one sealed knock row came to.
    enum SealedKnockStep: Equatable { case keep, drop, stop }

    /// ADR-0087 section 5 — open one sealed knock row and keep its KeyPackage
    /// as a request to join; the joiner's reply grant is kept for its account
    /// so the sweep can write the Welcome into its mailbox.
    private func openSealedKnock(
        record: GroupRecord, groupIdHex: String, identity: Identity, row: Data, createdAt: String,
        rowId: Int64 = 0
    ) -> SealedKnockStep {
        guard let version = knockRowVersion(row: row),
              var seed = knockSeed(record: record, groupIdHex: groupIdHex, version: version) else { return .drop }
        defer { RootHandover.wipe(&seed) }
        guard let request = try? openKnockRow(knockSeed: seed, row: row) else { return .drop }
        let linkCap: GroupInviteCapability? = {
            guard let capability = request.capability,
                  let cap = try? GroupInviteCapability.decodeAndVerify(wire: capability),
                  cap.groupId() == record.groupId else { return nil }
            return cap
        }()
        let byLink = linkCap != nil
        // A public group (ADR-0087 section 1) also takes a row with no link;
        // a private one only a row carrying a capability of this group.
        guard byLink || groupHandle(groupIdHex: groupIdHex)?.isPublic == true else { return .drop }
        // UX section 8 — a request rejected here is not read back: the knock
        // row stays on the DS after a Reject.
        if rowId > 0, let accountIdB64 = currentAccountIdB64,
           let joiner = try? keyPackageSignatureKey(keyPackage: request.keyPackage),
           let through = groupSettingsStore.rejected(accountIdB64: accountIdB64, groupIdHex: groupIdHex)[
               base64Encode(joiner)],
           rowId <= through {
            return .drop
        }
        guard let kept = try? record.group.retainJoinRequest(identity: identity, keyPackage: request.keyPackage)
        else { return .drop }
        switch kept.outcome {
        case .retained, .alreadyPending:
            // UX section 8 — `Asked 2 hours ago`, or from a link `Invited by
            // Anna · 2 hours ago`: when the request reached the Delivery
            // Service and whose link it carried.
            if let accountIdB64 = currentAccountIdB64 {
                groupSettingsStore.keepRequestMeta(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                    joinerKeyB64: base64Encode(kept.joinerSignatureKey),
                    askedAtMs: Self.parseDsTimestampMs(createdAt) ?? Self.nowMs(),
                    inviterKey: linkCap?.issuerSignaturePubkey(),
                    knockRowId: rowId > 0 ? rowId : nil)
            }
            // UX section 8 — the name the request carries, shown unverified.
            if let accountIdB64 = currentAccountIdB64,
               let name = request.profile?.displayName?.trimmingCharacters(in: .whitespacesAndNewlines),
               !name.isEmpty {
                groupSettingsStore.keepRequestName(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                    joinerKeyB64: base64Encode(kept.joinerSignatureKey), name: name)
            }
            let account = ((try? keyPackageAccountKey(keyPackage: request.keyPackage)) ?? nil)
                ?? kept.joinerSignatureKey
            ReachCenter.shared.keep(
                request.replyGrant, carrier: .request,
                fromAccountKeyB64: base64Encode(account), scopeHex: nil)
            return .keep
        case .queueFull:
            return .stop
        case .alreadyMember, .notAdmissible:
            return .drop
        }
    }

    /// The knock seed of `version` for this group: derived from the journal
    /// root on the owner's devices, else the one the owner handed this admin.
    private func knockSeed(record: GroupRecord, groupIdHex: String, version: UInt16) -> Data? {
        if ownsGroup(groupIdHex: groupIdHex) {
            return knockSeedForOwner(groupIdHex: groupIdHex, groupId: record.groupId, version: version)
        }
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return groupSettingsStore.knockSeeds(accountIdB64: accountIdB64, groupIdHex: groupIdHex)[version]
    }

    private func keepJoinAttributionGrant(record: GroupRecord, payload: Data) {
        guard let attr = try? parseGroupJoinAttribution(bytes: payload),
              attr.groupId == record.groupId,
              let account = try? record.group.pendingExternalJoinAccountKey(
                  joinerSignatureKey: attr.joinerSignatureKey) else { return }
        ReachCenter.shared.keep(
            attr.replyGrant, carrier: .request,
            fromAccountKeyB64: base64Encode(account), scopeHex: nil)
    }

    /// ADR-0060 sections 3 and 6 — keep the reach grants a profile carried,
    /// filed under the sender resolved at receipt (C-68) and scoped to this
    /// conversation. A `contact` grant is kept only from the other participant
    /// of a one-to-one conversation.
    func keepProfileGrants(_ claim: ProfileClaim, senderKey: MemberIdentity?, groupIdHex: String) {
        guard !claim.grants.isEmpty, let senderKey,
              senderKey != groups[groupIdHex].flatMap({ AuthorIdentity.own(group: $0.group) })
        else { return }
        let oneToOne = conversationKind(groupIdHex: groupIdHex) == .chat
            && (memberCount(groupIdHex: groupIdHex) ?? 0) <= 2
        let carrier: ReachRules.Carrier = oneToOne
            ? .oneToOneProfile(fromOtherParticipant: true)
            : .groupProfile
        for grant in claim.grants {
            ReachCenter.shared.keep(
                grant, carrier: carrier, fromAccountKeyB64: senderKey, scopeHex: groupIdHex)
        }
    }

    /// ADR-0060 — a "who can find me" service envelope received in a
    /// conversation. A committer delegation is kept for the sweep, from any
    /// member; an
    /// introduction offer / bundle is kept only from the other participant of
    /// a one-to-one conversation (``IntroductionBook``); a setting or a reach
    /// state belongs on the owner's DeviceGroup and is dropped here.
    func receiveScopedDiscovery(
        groupIdHex: String, envelope: ScopedDiscoveryEnvelope, senderKey: MemberIdentity?
    ) {
        guard let senderKey else { return }
        switch envelope {
        case .committerDelegation(let delegation):
            // Whoever sent it, this account included: a device never receives
            // its own group messages, so one from this account came from
            // another of its devices, which proposed the Add this device may
            // sweep and may be the only way to the added person it has.
            committerEnvelopeReach().keepDelegation(
                delegation, fromAccountKeyB64: senderKey, groupIdHex: groupIdHex)
        case .introductionOffer(let record):
            guard isOneToOneWithOther(groupIdHex: groupIdHex, senderKey: senderKey) else { return }
            // A new or changed offer is relayed to this account's other
            // contacts now, not at the next profile change: until it is,
            // they cannot find this contact as a contact of a contact.
            if IntroductionBook.shared.keepOffer(record, fromAccountKeyB64: senderKey) {
                requestIntroductions()
            }
        case .introductions(let bundle):
            guard isOneToOneWithOther(groupIdHex: groupIdHex, senderKey: senderKey) else { return }
            IntroductionBook.shared.keepBundle(bundle, fromAccountKeyB64: senderKey)
        case .visibility, .reachState:
            return
        }
    }

    /// Whether `groupIdHex` is a one-to-one conversation and `senderKey` its
    /// other participant.
    private func isOneToOneWithOther(groupIdHex: String, senderKey: MemberIdentity) -> Bool {
        guard conversationKind(groupIdHex: groupIdHex) == .chat,
              (memberCount(groupIdHex: groupIdHex) ?? 0) <= 2,
              let record = groups[groupIdHex] else { return false }
        return senderKey != AuthorIdentity.own(group: record.group)
    }

    /// An added leaf's account key, when this device can tell before the
    /// merge: a pending `/g/` joiner's retained KeyPackage, or a KeyPackage it
    /// fetched itself.
    private func resolveAddedAccountKey(record: GroupRecord, leafKey: Data) -> Data? {
        if let account = try? record.group.pendingExternalJoinAccountKey(joinerSignatureKey: leafKey) {
            return account
        }
        return addedLeafAccounts[base64Encode(leafKey)]
    }

    /// Welcome writes already made, by `SHA-256(bytes) | mailbox`: two leaves
    /// of one person that resolved only after the merge map to one mailbox,
    /// and the person gets the Welcome once.
    private var sweepWelcomeWrites: Set<String> = []

    /// Publish one sweep Welcome. `target` is an account-mailbox pseudonym, or
    /// an added leaf's signature key that could not be resolved before the
    /// merge — now a member, resolved through the roster to its owner's
    /// account.
    ///
    /// ADR-0050 2.5a item 7 — signed with each key this device holds for that
    /// mailbox in turn (``ReachCenter/writeAuthCandidates(toMailbox:groupIdHex:)``:
    /// this account's own key for its own mailbox, else every grant held for
    /// the person, then a committer delegation), moving to the next on a
    /// `401`. With none left it writes NOTHING and throws
    /// ``SweepWelcomeUndeliverable``: the Welcome stays owed and is retried on
    /// the next poll and when a grant for that person arrives
    /// (``retryOwedWelcomes()``). An unsigned write to a closed mailbox is
    /// answered as stored and dropped, which is how a Welcome used to be lost.
    private func publishSweepWelcome(
        target: Data, bytes: Data, record: GroupRecord, groupIdHex: String
    ) async throws -> Int64 {
        // ADR-0074 section 3.5a rule 1 — a leaf resolved through the roster
        // names the key it is BOUND under; a mailbox recorded earlier may be a
        // predecessor's. Either way the write goes to the current key's.
        let memberAccountKey = (try? record.group.members())?
            .first(where: { $0.signaturePublicKey == target })?.accountKey
        func resolve() throws -> SweepWelcomeAttempt {
            let plan = memberAccountKey.flatMap {
                ReachCenter.shared.writePlan(toAccountKey: $0, groupIdHex: groupIdHex)
            } ?? ReachCenter.shared.writePlan(toMailbox: target, groupIdHex: groupIdHex)
            // ADR-0060 section 6 — a person found in Everyone, named by a
            // kept no-delegation envelope or this device's own record, is
            // written to without a proof, and only when the signed order
            // above offers nothing.
            let open = memberAccountKey.flatMap {
                ReachCenter.shared.openWelcomeTarget(ofPerson: $0, groupIdHex: groupIdHex)
            } ?? ReachCenter.shared.openWelcomeTarget(forMailbox: target, groupIdHex: groupIdHex)
            // ADR-0078 — no verified mailbox, no write: the Welcome stays
            // owed (``SweepWelcomeUndeliverable`` with no key tried).
            return SweepWelcomeAttempt.decide(plan: plan, open: open)
        }
        let digest = base64Encode(Data(SHA256.hash(data: bytes)))
        let dedupe = digest + "|" + base64Encode(try resolve().mailbox ?? target)
        guard sweepWelcomeWrites.insert(dedupe).inserted else { return 0 }
        do {
            let client = dsClient
            var delivered: Data?
            // ADR-0074 section 3.5a rule 6 — when every key held was refused,
            // the person's address is re-checked once (throttled per person:
            // an owed Welcome is retried on every poll) and the Welcome goes
            // again when the mailbox or the keys moved.
            let id = try await RefusedMailboxWrite.write(
                resolve: resolve,
                refused: { ($0 as? SweepWelcomeUndeliverable).map { $0.triedKeys > 0 } ?? false },
                recheck: { [self] in
                    if let memberAccountKey {
                        await PersonAddressRecheck.recheck(
                            accountKey: memberAccountKey,
                            accountIdHint: memberAccountIdHints[memberAccountKey])
                    } else if let mailbox = try? resolve().mailbox {
                        await PersonAddressRecheck.recheck(mailbox: mailbox)
                    }
                },
                publish: { attempt in
                    guard let mailboxTarget = attempt.target else {
                        throw SweepWelcomeUndeliverable(triedKeys: 0)
                    }
                    let row = try mailboxTarget.seal(kind: .welcome, body: bytes)
                    let id: Int64
                    if attempt.openUnsigned {
                        // ADR-0060 section 6 — one proofless write, spending a
                        // `mailbox-write` token, as a contact request to a
                        // person in Everyone is written.
                        id = try await deliverOpenSweepWelcome {
                            try await client.publishMailbox(
                                pseudonym: mailboxTarget.address, payload: row, writeAuth: nil
                            ).id
                        }
                        ReachCenter.shared.dropOpenAdd(
                            groupIdHex: groupIdHex, person: mailboxTarget.accountKey)
                    } else {
                        id = try await deliverSweepWelcome(
                            candidates: attempt.candidates,
                            publish: { auth in
                                try await client.publishMailbox(
                                    pseudonym: mailboxTarget.address, payload: row, writeAuth: auth
                                ).id
                            })
                    }
                    delivered = mailboxTarget.address
                    return id
                })
            // Delivered to the re-checked mailbox: a later sweep resolving
            // that mailbox must not send the same Welcome there again.
            if let delivered { sweepWelcomeWrites.insert(digest + "|" + base64Encode(delivered)) }
            return id
        } catch {
            sweepWelcomeWrites.remove(dedupe)
            throw error
        }
    }

    /// ADR-0074 section 3.5a rule 6 — publish a Welcome addressed by a
    /// mailbox recorded earlier (the ADR-0040 outbox): to the current mailbox
    /// of whoever it names, and after a refusal once more when a gated fetch
    /// of that person's identity moved the mailbox or the grant.
    private func publishWelcomeRecheckingOnce(
        recorded: Data, payload: Data, groupIdHex: String
    ) async throws -> PublishResult {
        let client = dsClient
        // ADR-0078 — the recorded address resolves to the verified mailbox of
        // whoever it names (their current one); `payload` is the raw Welcome,
        // sealed here as a `.welcome` row. No mailbox known: nothing is
        // written and the entry stays un-acked.
        func resolve() throws -> MailboxWritePlan {
            guard let plan = ReachCenter.shared.writePlan(toMailbox: recorded, groupIdHex: groupIdHex) else {
                NSLog("ADR-0078: no verified mailbox for a recorded Welcome; it stays owed")
                throw MailboxUnknown()
            }
            return plan
        }
        return try await RefusedMailboxWrite.write(
            resolve: resolve,
            recheck: {
                await PersonAddressRecheck.recheck(mailbox: (try? resolve().target.address) ?? recorded)
            },
            publish: { plan in
                let row = try plan.target.seal(kind: .welcome, body: payload)
                return try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                    try await client.publishMailbox(
                        pseudonym: plan.target.address, payload: row, writeAuth: auth)
                }
            })
    }

    /// ADR-0050 2.5a item 7 — a grant (or a committer delegation) was just kept
    /// for someone: retry the Welcomes owed for lack of a key, in every group
    /// that has one. Each retry is an ordinary sweep attempt
    /// (``sweepIfCommitter(groupIdHex:)``), which resolves owed Welcomes first
    /// whatever this device's rank; it is spawned, never awaited, because this
    /// is reached from inside a poll's apply region.
    func retryOwedWelcomes() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        for (groupIdHex, record) in groups {
            let owes = commitOutbox.loadUnacked(
                accountIdB64: accountIdB64, groupIdB64: base64Encode(record.groupId)
            ).contains { $0.sweepWelcomesOwed == true }
            guard owes else { continue }
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let owedWelcomeRetry = self.owedWelcomeRetry {
                    await owedWelcomeRetry(groupIdHex)
                } else {
                    await self.sweepIfCommitter(groupIdHex: groupIdHex)
                }
            }
        }
    }

    /// Stands in for the sweep ``retryOwedWelcomes()`` runs. Injectable for
    /// tests.
    var owedWelcomeRetry: (@MainActor (String) async -> Void)?

    /// ADR-0053 point 2 / ADR-0078 — claim the account's ONE mailbox (contact
    /// requests, acks and every Welcome), under the mailbox key derived from
    /// the account seed. Device mailboxes are gone: none is claimed or read.
    /// Unsigned: the mailbox key is public and a claim grants nothing. Every
    /// session start, i.e. after every login, restore and signup. Best effort.
    public func claimOwnMailboxes() {
        let signers: [DsProofSigner] = Self.ownMailboxSigners(accountIdentity: accountIdentity)
        let client = dsClient
        // ADR-0060 — the hooks the reach registration reads, wired before the
        // claim so the sync below sees this account's groups.
        ReachCenter.shared.groupIdsProvider = { [weak self] in
            self?.allGroupIds() ?? []
        }
        // ADR-0050 2.5a item 7 — a key for someone just arrived: a Welcome
        // owed to their mailbox for lack of one is retried now.
        ReachCenter.shared.onGrantKept = { [weak self] _ in
            Task { @MainActor in self?.retryOwedWelcomes() }
        }
        // A rotation or a setting change changed the grants the profiles
        // carry: republish them.
        ReachCenter.shared.onGrantsChanged = { [weak self] in
            Task { @MainActor in
                await self?.publishOwnProfileToAllGroups()
                await self?.sendIntroductions()
            }
        }
        // The claims are this binding's own mailboxes, made with signers
        // captured above. What follows them is the next account's own work
        // (its ``claimOwnMailboxes()`` runs it) once this one has left.
        let bound = binding.capture()
        Task {
            for signer in signers {
                guard bound.isCurrent else { return }
                do {
                    try await client.claimMailbox(
                        pseudonym: signer.mailboxAddress(),
                        bodyJson: signer.mailboxClaimBodyJson())
                } catch {
                    // No error text: it names the mailbox address (ADR-0078).
                    NSLog("ADR-0053: mailbox claim failed, retried on the next session: \(type(of: error))")
                }
            }
            // ADR-0060 section 2 — a claimed mailbox starts CLOSED. Register
            // this account's reach keys on it right after the claim, so the
            // people it handed keys to can write to it.
            await MainActor.run { ReachCenter.shared.sync() }
            await self.sendIntroductions()
        }
    }

    /// The mailbox signers ``claimOwnMailboxes()`` claims under: the account's
    /// mailbox key alone (ADR-0078; no device mailbox), none before
    /// bootstrap.
    nonisolated static func ownMailboxSigners(accountIdentity: Identity?) -> [DsProofSigner] {
        accountIdentity.map { [dsMailboxSigner(accountIdentity: $0)] } ?? []
    }

    /// ADR-0060 section 4 — contacts of contacts. In each one-to-one
    /// conversation: this account's own introduction record as an
    /// IntroductionOffer (while 2.1 is on), and the records this account's
    /// contacts offered, as a padded Introductions bundle capped at
    /// `maxBundleRecords()`. The two are tracked apart, as on web: an offer is
    /// re-sent when the own record changed, a bundle when the relayed set did,
    /// and each is marked sent only after its send succeeded. Best effort.
    public func sendIntroductions() async {
        guard ReachCenter.shared.isActive else { return }
        // The record and the relayed set read below are this binding's
        // account's; they are never sent into another account's
        // conversations.
        let bound = binding.capture()
        let offer = await ReachCenter.shared.ownIntroductionRecord()
        guard bound.isCurrent else { return }
        let offerKey = offer.map { Data(SHA256.hash(data: $0)).base64EncodedString() }
        let relayed = IntroductionBook.shared.offeredRecords()
        let bundleDigest = Self.introductionsBundleDigest(relayed)
        let capped = Self.relayBundleRecords(relayed)
        for groupIdHex in groups.keys.sorted() {
            guard bound.isCurrent else { return }
            guard conversationKind(groupIdHex: groupIdHex) == .chat,
                  (memberCount(groupIdHex: groupIdHex) ?? 0) == 2,
                  !membershipEnded(membershipStateNow(groupIdHex: groupIdHex))
            else { continue }
            // An offer and a bundle go out once per conversation, so a device
            // that missed them asks, and the answer sends them again.
            let peer = oneToOnePeerSignatureKey(groupIdHex: groupIdHex)
            if let peer {
                let held = ReachCenter.shared.heldIntroductions(
                    fromAccountKeyB64: peer.base64EncodedString())
                if ReachRules.asksForIntroductions(
                    isOneToOne: isOneToOne(groupIdHex: groupIdHex), held: held) {
                    await profileRequester.noteIntroductionsMissing(
                        groupIdHex: groupIdHex, holdsContactGrant: held.contactGrant)
                    guard bound.isCurrent else { return }
                }
            }
            // The offer goes only to the contacts the setting allows; the
            // answer to an ask comes through here too. The relay below is
            // unchanged.
            if let offer, let offerKey,
               ReachRules.offersRecord(
                   to: peer.flatMap { ReachCenter.shared.contactAccountId(ofKey: $0) },
                   setting: ReachCenter.shared.setting,
                   allows: { allowsContact(setting: $0, accountId: $1) }),
               ReachCenter.shared.offerNeedsSending(groupIdHex: groupIdHex, key: offerKey) {
                do {
                    try await publishApplicationMessage(
                        groupIdHex: groupIdHex, plaintextBytes: try composeIntroductionOffer(record: offer))
                    guard bound.isCurrent else { return }
                    ReachCenter.shared.markOfferSent(groupIdHex: groupIdHex, key: offerKey)
                } catch {
                    NSLog("ADR-0060: introduction offer not sent: \(error)")
                }
            }
            if !capped.isEmpty,
               ReachCenter.shared.bundleNeedsSending(groupIdHex: groupIdHex, digest: bundleDigest) {
                do {
                    let bundle = try composeIntroductionsBundle(
                        records: capped, updatedAtMs: UInt64(Self.nowMs()))
                    try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: bundle)
                    guard bound.isCurrent else { return }
                    ReachCenter.shared.markBundleSent(groupIdHex: groupIdHex, digest: bundleDigest)
                } catch {
                    NSLog("ADR-0060: introductions bundle not sent: \(error)")
                }
            }
        }
    }

    /// Whether a ``sendIntroductions()`` run started by
    /// ``requestIntroductions()`` is in progress, and whether another was
    /// asked for meanwhile.
    private var introductionsRunning = false
    private var introductionsAgain = false

    /// Run ``sendIntroductions()`` once now; a request during a run asks for
    /// one more after it, so two runs never overlap and none is lost.
    func requestIntroductions() {
        if introductionsRunning {
            introductionsAgain = true
            return
        }
        introductionsRunning = true
        Task { @MainActor in
            repeat {
                introductionsAgain = false
                await sendIntroductions()
            } while introductionsAgain
            introductionsRunning = false
        }
    }

    /// The records one relayed bundle carries: at most `maxBundleRecords()`,
    /// as on web (the core refuses a larger bundle).
    static func relayBundleRecords(_ records: [Data]) -> [Data] {
        Array(records.prefix(Int(maxBundleRecords())))
    }

    /// The digest the relay tracks a bundle by: over the whole relayed set in
    /// a stable order, so reordering never counts as a change.
    static func introductionsBundleDigest(_ records: [Data]) -> String {
        let joined = records.sorted { $0.lexicographicallyPrecedes($1) }
            .reduce(Data()) { $0 + Data(SHA256.hash(data: $1)) }
        return Data(SHA256.hash(data: joined)).base64EncodedString()
    }

    /// The account key of every person in every conversation this device is
    /// in (ADR-0050: one key per person), for local people search.
    public func allMemberAccountKeys() -> [Data] {
        var seen = Set<Data>()
        var out: [Data] = []
        for groupIdHex in groups.keys.sorted() {
            for member in members(groupIdHex: groupIdHex) ?? []
            where seen.insert(member.signaturePublicKey).inserted {
                out.append(member.signaturePublicKey)
            }
        }
        return out
    }

    /// Every conversation group this device is in (ADR-0060: the `member` and
    /// `group` reach scopes).
    /// The ids of every group this device is still a member of: a group the
    /// account was removed from (or left) is not among them, so the reach
    /// registration sees it as left (ADR-0078 section 9).
    public func allGroupIds() -> [Data] {
        groups.values.compactMap { record in
            readIsActive { try record.group.isActive() } == false ? nil : record.groupId
        }
    }

    /// ADR-0053 point 7 — whether this device reads the knock queue: the
    /// owner or an admin, the roles that can approve a join.
    ///
    /// Gated on the ROLE, not on `ownCommitterRank() != nil`, which used to be
    /// the same thing and is not since ADR-0050 2.5a: a plain member holds a
    /// rank while its pending store has a device admission it could sweep, and
    /// approving a join is still owner-or-admin. A member reading knocks would
    /// fill a quarantine nobody on that device may act on.
    nonisolated static func readsKnocks(role: Role?) -> Bool {
        role == .owner || role == .admin
    }

    /// ADR-0053 point 7 — read what non-members knocked on `groupIdHex` and
    /// feed the external-join proposals to the group, under the commit lock.
    /// Owner / admin devices only (the ones that can approve), on their own
    /// cursor. Every row goes through the core's knock gate,
    /// `Group.processKnock`, NEVER `processIncomingVerifyingRoles`: it refuses,
    /// before any processing, anything that is not a stranger's external-join
    /// proposal for this group, so a member's commit or application message
    /// replayed into the unauthenticated knock queue cannot fork the admins
    /// who read it from everyone else. A refused row (an attribution, which
    /// ``fetchJoinAttributions(groupIdHex:)`` parses on its own, or anything
    /// else) is dropped and the cursor still advances.
    private func pollKnocks(groupIdHex: String) async {
        guard let record = groups[groupIdHex], let identity = canonicalIdentity,
              Self.readsKnocks(role: try? record.group.ownRole(identity: identity))
        else { return }
        // The cursor survives a relaunch: a request rejected here must not be
        // read back into the quarantine from the knock queue on the next start.
        let cursorAccount = currentAccountIdB64
        let cursorKey = cursorAccount.map {
            "me.lakko.chat.knock-cursor.v1.\($0).\(groupIdHex)"
        }
        if record.knockCursor == 0, let cursorKey {
            record.knockCursor = Int64(UserDefaults.standard.integer(forKey: cursorKey))
        }
        let after = record.knockCursor
        // The rows are read with this binding's signer and their cursor is
        // kept under its account (`cursorKey`): they are not applied to the
        // record of an account that signed in while the read was out.
        let bound = binding.capture()
        let raw: FetchResult
        do {
            raw = try await dsClient.fetchKnocks(
                groupId: record.groupId, after: after, waitSecs: 0,
                auth: dsAuth(groupIdHex: groupIdHex))
        } catch {
            NSLog("ADR-0053: knock fetch failed: \(error)")
            return
        }
        guard bound.isCurrent, !raw.messages.isEmpty else { return }
        try? await withCommitLock(groupIdHex: groupIdHex) { [self] in
            guard let identity = canonicalIdentity, let record = groups[groupIdHex],
                  record.knockCursor == after else { return }
            // ADR-0087 section 5 — a knock row is a KeyPackage sealed to the
            // group's knock key: opened with the seed of the row's own version
            // and kept through `retainJoinRequest`. A private group takes a
            // row only with a capability of this group. A full pending set
            // stops the cursor BEFORE the row, so the backlog stays on the DS
            // instead of being lost.
            for row in raw.messages {
                let step = openSealedKnock(
                    record: record, groupIdHex: groupIdHex, identity: identity, row: row.payload,
                    createdAt: row.createdAt, rowId: row.id)
                if step == .stop {
                    record.sawUnkeptExternalJoin = true
                    break
                }
                record.knockCursor = max(record.knockCursor, row.id)
            }
            // An accepted proposal enters the retained request set, which lives
            // in the MLS storage snapshot, so the cursor may move past these rows
            // only once it is durable — it may lag the durable state, never lead
            // it (`Chat/DurableCursor.swift`). A failed write re-reads them.
            // Written for every non-empty batch rather than only when a row was
            // accepted: conservative, and the batch is small.
            do {
                try persistSnapshotDurableOrThrow()
            } catch {
                record.knockCursor = after
                return
            }
            state = state.with(threads: buildThreadList())
            // The account was captured before the fetch: its cursor is not
            // written once its session has ended (``AccountStoreGate``). The
            // snapshot persist above does not cover that: with no account
            // bound any more it is a no-op, not a refusal.
            if let cursorKey, let cursorAccount,
               let gate = AccountStoreGate.enter(cursorAccount) {
                defer { gate.leave() }
                UserDefaults.standard.set(Int(record.knockCursor), forKey: cursorKey)
            }
        }
    }

    // MARK: - ADR-0040 (G-35) durable pending-commit outbox

    /// Persist the pending-commit outbox entry AND the N+1 MLS snapshot durably,
    /// FAIL-CLOSED, BEFORE any publish — the load-bearing step of every
    /// group-membership commit path.
    ///
    /// iOS has NO cross-store atomic transaction (the outbox is a per-entry file,
    /// the snapshot a separate AES-GCM file), so this uses the ADR-0040-sanctioned
    /// **entry-first, then snapshot** ordering. Both writes are fail-closed
    /// (throw): if EITHER throws, the caller ABORTS before publishing, so the
    /// group is never advanced on the wire without a durable, re-publishable
    /// record of the commit.
    ///
    /// The one torn middle state — outbox entry landed, snapshot still at N —
    /// provably means the commit was NEVER published (publish is strictly after
    /// BOTH writes), and ``sweepPendingCommits(record:)`` DISCARDS such an orphan
    /// entry (its `targetEpoch` was not reached by the durably-restored group).
    /// The forbidden state "snapshot at N+1 with no outbox entry" is impossible:
    /// the entry is written strictly first.
    ///
    /// Unlike the send-path ``persistSnapshotDurableOrThrow()`` (a no-op when
    /// there is no identity / account — that path has no reload that could
    /// re-consume a generation), this THROWS when the identity / device key is
    /// unavailable: a commit we cannot durably record must NOT be published.
    private func persistCommitEntryThenSnapshotOrThrow(_ entry: NewCommitEntry) throws {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else {
            throw ChatError.argument(
                "ChatRepository not bootstrapped — commit aborted before publish"
            )
        }
        // 1) ENTRY FIRST (fail-closed). Holds the commit + Welcome / rendezvous
        //    bytes so a crash before / during publish can re-publish them.
        try commitOutbox.putNewDurable(accountIdB64: accountIdB64, entry: entry)
        // 2) SNAPSHOT SECOND (fail-closed). Advances the durable MLS state to
        //    N+1. A throw here leaves the torn entry-then-snapshot state the
        //    sweep discards (the commit is never published because we abort).
        let blob = try identity.snapshotStorage()
        try snapshotStore.putSnapshotDurable(accountIdB64: accountIdB64, blob: blob)
    }

    /// Mark an outbox entry acked after BOTH artifacts have published.
    /// Best-effort: a failure leaves the entry un-acked, so the next restart
    /// sweep re-publishes it idempotently (peers drop the wrong-epoch duplicate;
    /// the committer's echo filter is re-armed).
    private func ackCommitBestEffort(
        entryId: String,
        commitMessageId: Int64?,
        secondMessageId: Int64? = nil
    ) {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        commitOutbox.markAckedBestEffort(
            accountIdB64: accountIdB64,
            id: entryId,
            commitMessageId: commitMessageId,
            secondMessageId: secondMessageId
        )
    }

    /// Restart / group-open sweep (ADR-0040, G-35). Runs ONCE per group per
    /// session (from ``pollGroup(groupIdHex:waitSecs:)``, before the first poll).
    ///
    /// The pure ``planSweep(currentEpoch:entries:)`` classifies each un-acked
    /// entry against the group's durably-restored `Group.epoch()`
    /// (torn-write reconciliation):
    ///
    ///   - `currentEpoch >= targetEpoch` -> the commit WAS applied durably ->
    ///     re-publish idempotently + re-arm the own-echo filter, then ack.
    ///   - `currentEpoch <  targetEpoch` -> a torn entry-then-snapshot write
    ///     whose snapshot was lost -> the commit was NEVER published -> DISCARD
    ///     the orphan entry, stay at N (clean).
    private func sweepPendingCommits(record: GroupRecord, groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        // `record` and the outbox rows are this binding's account's. Each
        // locked region below refuses to start once that account has left;
        // the direct re-publish after them is checked here.
        let bound = binding.capture()
        // ADR-0045 obligation 1 — a staged sweep is DURABLE and freezes this
        // group's own membership operations across relaunches, so a verdict
        // owed from a previous run must be finished before anything else. It is
        // wired into this existing restart seam rather than into a second one,
        // because two restart paths over the same durable state is how they
        // drift.
        //
        // It runs FIRST: the legacy re-publish loop below would otherwise
        // re-publish the sweep's own commit as if it were a direct-path entry
        // and — worse — re-publish its Welcome, which ADR-0045 forbids before
        // the verdict.
        //
        // Under `withCommitLock`, because adjudicating APPLIES journal rows and
        // mutates the shared OpenMLS provider — exactly what the chain protects.
        // Not a deadlock: `pollGroup` calls this method BEFORE it takes the
        // chain for its own apply, so the two acquisitions are sequential rather
        // than nested. (The legacy re-publish loop below stays outside on
        // purpose — it only publishes, and it is reached from the same
        // un-nested position.)
        do {
            _ = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                // ADR-0057 S2 finding 1 — an owed verdict that merges (or the
                // rows it applies) may carry this account's ownership transfer.
                let ownerBefore = roleTableOwner(groupIdHex: groupIdHex)
                defer {
                    noteOwnerTransition(
                        groupIdHex: groupIdHex, before: ownerBefore,
                        after: roleTableOwner(groupIdHex: groupIdHex))
                }
                return try await resolveOwedVerdict(sweepPorts(record: record, groupIdHex: groupIdHex))
            }
        } catch {
            // The journal was unreadable, so the verdict stays owed and the
            // sweep stays staged for the next attempt (which runs at the tail of
            // every poll). Membership on this group is frozen until then — a
            // temporary freeze that self-heals, which is strictly better than
            // merging or abandoning on a verdict nobody read.
            NSLog("ADR-0045 owed sweep verdict unresolved: \(error)")
        }
        // A sweep whose verdict SETTLED but whose Welcome never reached the wire
        // owes only the fan-out. It is invisible to every other recovery path —
        // the merge cleared `hasStagedSweep()` and emptied the pending set, and
        // the legacy loop below skips sweep entries — so without this the added
        // member stays in every peer's roster, addressed by every later message,
        // permanently unable to construct group state.
        //
        // It runs AFTER the owed verdict on purpose: a still-staged sweep owes a
        // VERDICT, and only once that has merged is a Welcome allowed on the wire
        // at all (ADR-0045 obligation 2). `resolveOwedWelcomes` refuses while
        // anything is staged, so the order here is belt to that braces.
        do {
            _ = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                try await resolveOwedWelcomes(sweepPorts(record: record, groupIdHex: groupIdHex))
            }
        } catch {
            // The DS is still unreachable. The durable marker survives, so the
            // next sweep — group open or poll tail — retries the fan-out.
            NSLog("ADR-0045 owed sweep Welcome unresolved: \(error)")
        }
        guard bound.isCurrent else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let entries = commitOutbox.loadUnacked(
            accountIdB64: accountIdB64,
            groupIdB64: groupIdB64
        )
        guard !entries.isEmpty else { return }
        // The durably-restored epoch. A failure to read it is treated as 0
        // (fail-safe -> nothing is <= 0 unless targetEpoch is 0 too, so a
        // genuine pending entry is discarded rather than blindly re-published
        // against an unknown-epoch group).
        let currentEpoch = (try? record.group.epoch()) ?? 0
        // ADR-0045 — a staged-sweep entry is NOT re-publishable on this path.
        // Two reasons, and either alone is decisive: its device is durably at N,
        // not N+1, so the unconditional-re-publish argument this loop rests on
        // does not hold for it; and its Welcome must not reach the wire until
        // the verdict says the commit won. `resolveOwedVerdict` above owns it.
        let directEntries = entries.filter { $0.sweepVerdictPending != true }
        guard !directEntries.isEmpty else { return }
        for action in Self.planSweep(currentEpoch: currentEpoch, entries: directEntries) {
            guard bound.isCurrent else { return }
            switch action {
            case .discardTornWrite(let entry):
                // The snapshot never reached targetEpoch -> the commit was never
                // published -> drop the orphan, the device stays at N.
                commitOutbox.deleteEntryBestEffort(
                    accountIdB64: accountIdB64,
                    id: entry.id
                )
            case .republish(let entry):
                await republishPendingCommit(
                    record: record,
                    groupIdHex: groupIdHex,
                    accountIdB64: accountIdB64,
                    entry: entry
                )
            }
        }
    }

    /// Pure sweep planner (ADR-0040 torn-write reconciliation) — see
    /// ``sweepPendingCommits(record:)``. Split out from the executing sweep so
    /// the load-bearing decision is directly unit-testable.
    ///
    /// An unparseable `targetEpoch` is treated as unreachable (`UInt64.max`) ->
    /// discard — a malformed entry can never be blindly re-published.
    ///
    /// `nonisolated` — a pure function over its arguments with no actor state, so
    /// callers (and unit tests) invoke it from any context.
    nonisolated static func planSweep(
        currentEpoch: UInt64,
        entries: [PendingCommitEntry]
    ) -> [CommitSweepAction] {
        entries.map { entry in
            let target = UInt64(entry.targetEpoch) ?? UInt64.max
            return currentEpoch >= target
                ? .republish(entry)
                : .discardTornWrite(entry)
        }
    }

    /// Deliver what an un-acked direct-commit entry still owes (its Welcome),
    /// then ack. Since ADR-0081 the entry is written after the commit route
    /// took the commit, so the commit itself is never re-sent. A failure
    /// (offline / DS down) leaves the entry un-acked for the next sweep — safe.
    /// A corrupt (undecodable) entry is dropped.
    private func republishPendingCommit(
        record: GroupRecord,
        groupIdHex: String,
        accountIdB64: String,
        entry: PendingCommitEntry
    ) async {
        guard let commitBytes = base64Decode(entry.commitB64) else {
            // Cannot re-publish garbage — drop the corrupt entry.
            commitOutbox.deleteEntryBestEffort(accountIdB64: accountIdB64, id: entry.id)
            return
        }
        let bound = binding.capture()
        do {
            // ADR-0081 — a direct commit's entry is written only after the
            // commit route took the commit, so the commit is on the journal
            // already and is never re-sent here; only the Welcome may still be
            // owed. Its message id is not in the entry, so the ack carries none.
            _ = commitBytes.count
            // ADR-0045 S7 — a direct-path second artifact is ALWAYS a mailbox
            // Welcome; since ADR-0053 no `WelcomeRendezvous` is written to a
            // group journal by any path.
            var secondId: Int64?
            if let secondB64 = entry.secondArtifactB64,
               let secondBytes = base64Decode(secondB64),
               let mailboxB64 = entry.secondArtifactMailboxB64,
               let recorded = base64Decode(mailboxB64) {
                // ADR-0074 section 3.5a rule 1 — the entry may have been
                // recorded before the addressee's rotation was observed.
                // Welcome -> the new member's mailbox (NOT this device's group
                // journal), so no echo re-arm — the committer never re-fetches
                // it.
                secondId = try await publishWelcomeRecheckingOnce(
                    recorded: recorded, payload: secondBytes, groupIdHex: groupIdHex
                ).id
                try bound.require()
            }
            ackCommitBestEffort(
                entryId: entry.id,
                commitMessageId: nil,
                secondMessageId: secondId
            )
        } catch {
            // Offline / DS down -> leave un-acked; the next sweep retries. Safe.
            NSLog("commit-outbox re-publish failed, will retry: \(entry.id) \(error)")
        }
    }

    /// The receive path's cursor-may-lag-never-lead gate.
    ///
    /// ONE per repository, which is the accurate granularity: `snapshotStorage()`
    /// serialises the WHOLE provider, so a failed persist leaves every group
    /// behind and any successful one flushes them all. See
    /// `Chat/DurableCursor.swift`.
    private let cursorGate = DurableCursorGate()

    /// The persist a proposal makes before it is published.
    ///
    /// Best effort against a disk or Keychain failure, as it always was (a
    /// proposal advances no epoch). NOT best effort against a refusal: when
    /// no account is bound, or the account's session has ended
    /// (``AccountStoreGate``), the proposal's state was not written and the
    /// account that built it is gone, so this throws and nothing is
    /// published. A refused persist always means nothing went out.
    private func persistSnapshotBeforePublish() throws {
        guard canonicalIdentity != nil, let accountIdB64 = currentAccountIdB64 else {
            throw StaleBindingError()
        }
        if AccountStoreGate.isSealed(accountIdB64) { throw AccountStoreGate.Sealed() }
        persistSnapshotBestEffort()
    }

    func persistSnapshotBestEffort() {
        guard let identity = canonicalIdentity,
              let accountIdB64 = currentAccountIdB64 else { return }
        do {
            let blob = try identity.snapshotStorage()
            snapshotStore.putSnapshot(
                accountIdB64: accountIdB64,
                blob: blob
            )
        } catch {
            // best-effort
        }
        // Phase 2a3f1 — co-located with the MLS snapshot
        // persist so every mutation that ratchets the group
        // also writes the resulting plaintext-message log.
        // Keeps the on-disk pair in sync without dispersing
        // call sites.
        persistMessageLogBestEffort()
    }

    /// Capture the current `(cursor, messages)` map across
    /// every known group and write it to the encrypted message
    /// log (Phase 2a3f1). Best-effort — Keychain / FS failures
    /// log + swallow.
    func persistMessageLogBestEffort() {
        guard let accountIdB64 = currentAccountIdB64 else { return }
        var slots: [String: PersistedGroup] = [:]
        for (hex, record) in groups {
            slots[hex] = PersistedGroup(
                cursor: record.cursor,
                // A row that has not reached the server is not written. It
                // has no server id to be keyed on, the other side has never
                // heard of it, and a relaunch that restored it would show a
                // message as if it had been sent. That is also why a failed
                // send stops being retryable at a relaunch: keeping it would
                // be a durable outbox, which is a different thing and is
                // unbuilt.
                messages: PersistedMessage.persistable(record.messages),
                lastViewedMessageId: record.lastViewedMessageId,
                feedSeen: record.feedSeen.isEmpty ? nil : record.feedSeen.sorted(),
                syncedThroughMessageId: record.syncedThroughMessageId
            )
        }
        let payload = MessageLogPayload(groups: waitingLogSlots.composed(with: slots))
        messageLogStore.putLog(accountIdB64: accountIdB64, payload: payload)
    }

    /// A history bundle reached the log while this account's repository runs
    /// (a device pairing while signed in, or one whose Welcomes came first):
    /// the rows go to the record of a group this device holds, else they wait
    /// for its join. `slots` are the bundle's own rows, so a persist that ran
    /// since the bundle reached the disk cost nothing.
    func adoptHistoryBundle(accountIdB64: String, slots: [String: PersistedGroup]) {
        guard currentAccountIdB64 == accountIdB64 else { return }
        for (groupIdHex, slot) in slots {
            guard let record = groups[groupIdHex] else {
                waitingLogSlots.add(groupIdHex, slot)
                continue
            }
            record.adoptHistory(slot)
            // A row counted before the bundle came, that the bundle also
            // carried, is no longer news.
            if let floor = slot.syncedThroughMessageId, var unread = unreadMessageIds[groupIdHex] {
                unread = unread.filter { !PairingHistory.isHistory(messageId: $0, floor: floor) }
                unreadMessageIds[groupIdHex] = unread.isEmpty ? nil : unread
            }
        }
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
    }

    /// ADR-0073 R.5 — one thread per CONVERSATION. The groups this device
    /// holds are folded by lineage: the thread is the conversation's current
    /// group (the head), with the rows of the older segments it still holds
    /// in front, under history ids.
    func buildThreadList() -> [ChatThread] {
        let arrivals = currentAccountIdB64.map { groupArrivalStore.load(accountIdB64: $0) } ?? [:]
        return conversationPlans().compactMap { plan -> ChatThread? in
            guard let head = groups[plan.headHex] else { return nil }
            // ADR-0087 section 9 — the row leaves the moment the end is known,
            // whatever step the end is at.
            guard !plan.heldSegments.contains(where: { isEnding(groupIdHex: $0) }) else { return nil }
            let headAt = plan.heldSegments.firstIndex(of: plan.headHex) ?? 0
            let older = Array(plan.heldSegments.prefix(headAt))
            var history: [ChatMessage] = []
            var unread = unreadMessageIds[plan.headHex]?.count ?? 0
            for (index, hex) in older.enumerated() {
                guard let record = groups[hex] else { continue }
                let distance = older.count - index
                unread += unreadMessageIds[hex]?.count ?? 0
                for message in record.messages where message.messageId > 0 && message.ack == .sent {
                    history.append(message.asHistoryRow(distanceFromHead: distance))
                }
            }
            let thread = head.toThread(
                unread: unread,
                conversationIdHex: plan.conversationHex,
                history: history,
                historyGroupIdHexes: older,
                segmentIndex: conversationLineage.segmentIndexOf(plan.headHex),
                leaveRequested: leaveRequestedAt(groupIdHex: plan.headHex) != nil,
                // The conversation's own arrival first: moving into a
                // successor group is not a new conversation (ADR-0073 R.5).
                arrivedAtMs: arrivals[plan.conversationHex] ?? arrivals[plan.headHex])
            // Bundle v3 — what another device of this account wrote to the
            // person before this one-to-one existed, drawn as own waiting
            // rows until the real messages arrive. Display only: not in the
            // record, so no send, retry or log write ever sees them.
            let copies = displayCopyRows(
                conversationHex: plan.conversationHex, ownKey: AuthorIdentity.own(group: head.group))
            return copies.isEmpty ? thread : thread.appending(copies)
        }
        .sorted { $0.conversationIdHex < $1.conversationIdHex }
    }

    /// Decode a lowercase hex string into bytes. Returns `nil` on
    /// any malformed input (odd length, non-hex chars).
    static func hexToBytes(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data()
        out.reserveCapacity(hex.count / 2)
        var i = hex.startIndex
        while i < hex.endIndex {
            let next = hex.index(i, offsetBy: 2)
            let byteStr = hex[i..<next]
            guard let byte = UInt8(byteStr, radix: 16) else {
                return nil
            }
            out.append(byte)
            i = next
        }
        return out
    }
}

// MARK: - State shape

/// Snapshot of every ``ChatRepository`` field a UI cares about.
/// ADR-0045 — what a membership action did.
///
/// The two arms are not two flavours of success, they are two different things
/// that happened, and the UI has to say so: a `committed` action took effect
/// across the group, a `proposed` one is a request that lands when the committer
/// sweeps. Collapsing them into one shape would force the members sheet to
/// assert an effect that has not happened, which is exactly the optimistic-update
/// lie `docs/ux/proposal-based-membership.md` forbids.
public enum MembershipOutcome: Equatable {
    /// A direct commit landed. No membership call returns this case since the
    /// ADR-0081 amendment of 2026-10-19 deleted the direct-commit paths (a
    /// group with no role table is refused); kept so the outcome consumers
    /// keep one shape.
    case committed(commitMessageId: Int64, welcomeMessageId: Int64?)
    /// A proposal was published. It takes effect when a committer sweeps it.
    case proposed(proposalMessageId: Int64)
}

public struct ChatRepositoryState: Equatable {
    public let threads: [ChatThread]
    public let bootstrapped: Bool

    public init(threads: [ChatThread] = [], bootstrapped: Bool = false) {
        self.threads = threads
        self.bootstrapped = bootstrapped
    }

    public func with(
        threads: [ChatThread]? = nil,
        bootstrapped: Bool? = nil
    ) -> ChatRepositoryState {
        ChatRepositoryState(
            threads: threads ?? self.threads,
            bootstrapped: bootstrapped ?? self.bootstrapped
        )
    }
}

/// One conversation thread — keyed by hex group id.
public struct ChatThread: Equatable {
    /// Hex-encoded MLS group id; used as the DS journal key and as
    /// the route argument for the 1×1 ChatScreen.
    public let groupIdHex: String
    public let messages: [ChatMessage]
    /// Cursor for the next ``ChatClient/pollMessages(identity:group:after:waitSecs:)``
    /// call.
    public let cursor: Int64
    /// ADR-0045 UX §6.2.4 — has this device's own membership ended?
    ///
    /// Read off `Group.isActive()` when the thread snapshot is built, so the
    /// chat list can render its one muted line and suppress the unread badge
    /// without reaching for a live `Group` handle it does not hold. A torn read
    /// reports `false` — an unreadable group is not evidence of an eviction —
    /// and the state is TERMINAL, so a snapshot can never be stale in the
    /// direction that matters.
    public let membershipEnded: Bool
    /// Incoming messages this device applied while the chat was not open, since
    /// it was last opened here. In memory only; see ``UnreadCounting``.
    public let unread: Int
    /// ADR-0063 section 5 — ids the feed showed long enough to count as seen;
    /// the unread badge skips them.
    public let feedSeen: Set<Int64>
    /// ADR-0063 section 5 — this device's read watermark for the group.
    public let lastViewedMessageId: Int64?
    /// `docs/ux/second-device.md` section 2 — the pairing-history floor of
    /// ``groupIdHex``: a row at or below it is never unread on this device.
    /// Local, never sent.
    public let syncedThroughMessageId: Int64?
    /// ADR-0073 R.5 — the CONVERSATION this thread is: the hex of the
    /// lineage's first group. The key of everything a person attaches to a
    /// conversation (pin / mute / archive, folders, the draft, the read
    /// position, the route). Equal to ``groupIdHex`` until the conversation
    /// continues in a successor group; from then on ``groupIdHex`` is the
    /// conversation's CURRENT group (the head: sends, polls, calls) and this
    /// stays what it was.
    public let conversationIdHex: String
    /// ADR-0073 R.5 — the older groups of the conversation this device still
    /// holds, oldest first. Their rows are in ``messages`` under history ids
    /// (``LineageRules/historyRowId(_:distanceFromHead:)``), read-only.
    public let historyGroupIdHexes: [String]
    /// ADR-0073 R.5 — the index of ``groupIdHex`` in the conversation's
    /// segments: what the cross-device read position carries past the first
    /// segment.
    public let segmentIndex: Int
    /// UX 9.4 — this device asked to leave and the request has not landed.
    /// The row stays and says `Waiting to leave.`
    public let leaveRequested: Bool
    /// When this device created or joined the conversation, for a list row
    /// with no message yet (``GroupArrival``). `nil` when it is not known.
    public let arrivedAtMs: Int64?

    public init(
        groupIdHex: String,
        messages: [ChatMessage],
        cursor: Int64,
        unread: Int = 0,
        feedSeen: Set<Int64> = [],
        lastViewedMessageId: Int64? = nil,
        membershipEnded: Bool = false,
        conversationIdHex: String? = nil,
        historyGroupIdHexes: [String] = [],
        segmentIndex: Int = 0,
        syncedThroughMessageId: Int64? = nil,
        leaveRequested: Bool = false,
        arrivedAtMs: Int64? = nil
    ) {
        self.arrivedAtMs = arrivedAtMs
        self.leaveRequested = leaveRequested
        self.syncedThroughMessageId = syncedThroughMessageId
        self.conversationIdHex = conversationIdHex ?? groupIdHex
        self.historyGroupIdHexes = historyGroupIdHexes
        self.segmentIndex = segmentIndex
        self.groupIdHex = groupIdHex
        self.messages = messages
        self.cursor = cursor
        self.unread = unread
        self.feedSeen = feedSeen
        self.lastViewedMessageId = lastViewedMessageId
        self.membershipEnded = membershipEnded
    }
}

extension ChatThread {
    /// The same thread with `rows` after its messages (display-only rows).
    func appending(_ rows: [ChatMessage]) -> ChatThread {
        ChatThread(
            groupIdHex: groupIdHex,
            messages: messages + rows,
            cursor: cursor,
            unread: unread,
            feedSeen: feedSeen,
            lastViewedMessageId: lastViewedMessageId,
            membershipEnded: membershipEnded,
            conversationIdHex: conversationIdHex,
            historyGroupIdHexes: historyGroupIdHexes,
            segmentIndex: segmentIndex,
            syncedThroughMessageId: syncedThroughMessageId,
            leaveRequested: leaveRequested,
            arrivedAtMs: arrivedAtMs)
    }
}

/// One message in a ``ChatThread``.
///
/// Phase 1.19c3: now carries an optional `attachments` list
/// alongside `text`. Outgoing messages populated locally on
/// ``ChatRepository/sendMessage(groupIdHex:plaintext:attachments:)``;
/// incoming messages populated from the parsed envelope on
/// ``ChatClient/pollMessages(identity:group:after:waitSecs:)``.
/// Empty list = pre-1.19-style text-only message; identical
/// rendering to before.
public struct ChatMessage: Equatable, Identifiable {
    public let messageId: Int64
    public let direction: Direction
    public internal(set) var text: String
    public let attachments: [AttachmentRef]
    public let createdAt: String
    /// ADR-0023 — the original author's IDENTITY: their Ed25519 signature
    /// public key, base64. Set on arrival from the identity the receive loop
    /// resolved out of the Application frame's sender leaf, in the epoch that
    /// named it; for our own outgoing messages it is this device's own key,
    /// stamped at send. `nil` when the author could not be resolved (a system
    /// row, an external sender, a row stored before this field existed) — such
    /// a message can never be a valid edit / delete target (the author-only
    /// gate fails closed).
    ///
    /// It is not a leaf, and there is nowhere left that keeps one: OpenMLS
    /// hands a vacated leaf to the next member added, so a leaf stored here
    /// would start naming a stranger after the next commit — permanently,
    /// because this field is persisted. See ``AuthorIdentity``.
    public let authorKeyB64: MemberIdentity?
    /// ADR-0023 — `true` after an author-gated edit op folded its
    /// new text into this message. The UI renders a subtle
    /// "(edited)" marker.
    public internal(set) var edited: Bool
    /// ADR-0023 — `true` after an author-gated delete op
    /// tombstoned this message. The UI renders a muted "message
    /// deleted" placeholder in place of the body.
    public internal(set) var deleted: Bool
    /// ADR-0023 — reactions folded onto this message, one entry
    /// per `(reactor, emoji)`. Re-receiving the same pair
    /// toggles the entry off. The UI aggregates by emoji into
    /// chips with counts.
    public internal(set) var reactions: [Reaction]
    /// Whether this row reached the server.
    ///
    /// A message the user wrote appears in the conversation immediately and
    /// settles afterwards, so the words are somewhere they can be seen and
    /// retried rather than only in the composer. Before this the send was
    /// awaited first and the row appended only on success: a message written
    /// on a bad connection left the person looking at an error and an
    /// unchanged composer, with nothing in the conversation at all.
    ///
    /// Defaults to ``Ack/sent`` so every row that comes off the journal, the
    /// message log or a call transcript is correct without naming it: those
    /// rows exist BECAUSE they were delivered. Only the local send path sets
    /// anything else.
    public internal(set) var ack: Ack
    /// A client-side identity for a row this device created, `0` for a row
    /// that came from the server.
    ///
    /// Needed because an unsent row has no `messageId` yet, and two of them
    /// would then share the same identity - which for SwiftUI's `ForEach`
    /// means one row drawn for two messages. It survives the flip to
    /// ``Ack/sent`` so the row is not torn down and rebuilt at the moment it
    /// lands, and it is never persisted: an unsent row is not written at all.
    public let localId: Int64
    /// ADR-0052 decision 1 — what this message replies to, when it is a
    /// reply: the target's DS `messageId` plus the snapshot of the quoted
    /// author and excerpt the replier took when Reply was chosen. `nil` for
    /// an ordinary message.
    ///
    /// The quote is drawn from THIS value and never re-read from the target
    /// (`docs/ux/message-actions.md` section 3.1), so it renders the same
    /// whether the original was edited, deleted or never held on this device.
    public let reply: ReplyRef?
    /// ADR-0052 decision 2 — set when this message was forwarded from
    /// somewhere else: the original author's display name as the forwarder's
    /// timeline showed it (a courtesy label, never a verified identity), and
    /// — when the forwarder held the original — where it came from, so a
    /// recipient who is also in that conversation can be taken to it. `nil`
    /// for a message written in this conversation.
    public let forward: ForwardRef?
    /// ADR-0061 section 6 — set when the sender's client wrote this message in
    /// a newer chat wire version than this build reads: the version number,
    /// with `text` empty and no attachments. The bubble and the conversation
    /// row draw the catalog sentence `chat.message.unsupported` from it at
    /// display time, so nothing English is stored and the line follows a
    /// language switch. `nil` for every message this build could read.
    public let unsupportedVersion: UInt64?
    /// ADR-0082 section 5 / ADR-0023 — the DS message id of the newest edit or
    /// delete op folded into this message; `nil` when it was never edited. A
    /// reaction does not move it. A stored translation or transcript made
    /// before it changed is stale (`storedResultIsCurrent`, with 0 for `nil`).
    public internal(set) var lastOpId: Int64?

    /// Raise ``lastOpId`` to an op's id; never lowers it (a re-fetch of an
    /// older op changes nothing).
    mutating func noteOp(_ opMessageId: Int64?) {
        guard let opMessageId else { return }
        if lastOpId.map({ opMessageId > $0 }) ?? true { lastOpId = opMessageId }
    }

    /// Whether an outgoing row reached the server.
    public enum Ack: Equatable {
        /// On its way. The bubble reads "Sending...".
        case pending
        /// The server has it; `messageId` and `createdAt` are real.
        case sent
        /// It did not get there and nothing reached the other side. The
        /// bubble reads "Not sent - Retry" and keeps the text.
        case failed
    }

    public init(
        messageId: Int64,
        direction: Direction,
        text: String,
        attachments: [AttachmentRef] = [],
        createdAt: String,
        authorKeyB64: MemberIdentity? = nil,
        edited: Bool = false,
        deleted: Bool = false,
        reactions: [Reaction] = [],
        ack: Ack = .sent,
        localId: Int64 = 0,
        reply: ReplyRef? = nil,
        forward: ForwardRef? = nil,
        unsupportedVersion: UInt64? = nil,
        lastOpId: Int64? = nil
    ) {
        self.lastOpId = lastOpId
        self.messageId = messageId
        self.direction = direction
        self.text = text
        self.attachments = attachments
        self.createdAt = createdAt
        self.authorKeyB64 = authorKeyB64
        self.edited = edited
        self.deleted = deleted
        self.reactions = reactions
        self.ack = ack
        self.localId = localId
        self.reply = reply
        self.forward = forward
        self.unsupportedVersion = unsupportedVersion
    }

    /// `Identifiable` conformance for SwiftUI `ForEach`.
    ///
    /// The server-assigned messageId is monotonic per journal and doubles as
    /// the identity for every row that has one. A row this device created
    /// prefers its ``localId``, and keeps preferring it after the send lands:
    /// an identity that changed at that moment would tear the bubble down and
    /// build a new one, losing any selection or animation mid-send.
    public var id: Int64 { localId != 0 ? localId : messageId }

    /// ADR-0073 R.5 — this stored row of an OLDER segment of the
    /// conversation, as the open timeline shows it: the same row under its
    /// on-screen history id, with a reply's target moved the same way so a
    /// quote still jumps to the message it answered (a reply target is a
    /// message of the SAME segment). Display-only: never persisted and never
    /// the target of an edit, a deletion, a reaction, a reply or a pin.
    func asHistoryRow(distanceFromHead: Int) -> ChatMessage {
        let shift: (Int64) -> Int64 = {
            LineageRules.historyRowId($0, distanceFromHead: distanceFromHead)
        }
        return ChatMessage(
            messageId: shift(messageId),
            direction: direction,
            text: text,
            attachments: attachments,
            createdAt: createdAt,
            authorKeyB64: authorKeyB64,
            edited: edited,
            deleted: deleted,
            reactions: reactions,
            ack: .sent,
            localId: 0,
            reply: reply.map {
                ReplyRef(
                    target: $0.target > 0 ? shift($0.target) : $0.target,
                    author: $0.author, excerpt: $0.excerpt)
            },
            forward: forward,
            unsupportedVersion: unsupportedVersion,
            lastOpId: lastOpId)
    }

    /// ADR-0023 — aggregate the reaction list into `(emoji,
    /// count, reactedByMe)` rows for the chip UI.
    ///
    /// `own` is this device's signature key, NOT its leaf: a leaf compared
    /// against a stored reactor would start matching a stranger after a
    /// renumber, lighting the "you reacted" highlight on somebody else's
    /// reaction. A nil `own` (no handle) falls back to the sentinel our own
    /// unresolved reactions were filed under, so the highlight still matches
    /// what this device actually contributed.
    public func reactionSummary(own: MemberIdentity?) -> [ReactionTally] {
        let mineKey = own ?? AuthorIdentity.ownReactionSentinel
        var order: [String] = []
        var counts: [String: Int] = [:]
        var mine: Set<String> = []
        for r in heldReactions {
            if counts[r.emoji] == nil { order.append(r.emoji) }
            counts[r.emoji, default: 0] += 1
            if r.reactor == mineKey { mine.insert(r.emoji) }
        }
        return order.map {
            ReactionTally(
                emoji: $0,
                count: counts[$0] ?? 0,
                reactedByMe: mine.contains($0)
            )
        }
    }

    /// Reactions-and-emoji spec section 10 — what the reaction op log comes
    /// to: per person (account key) at most 3, in journal order, a 4th
    /// ignored for good. Decided by the one Rust fold (`foldReactions`) every
    /// member runs. An op whose reactor is not a 32-byte key (the own
    /// sentinel of a torn roster read) is kept aside and applied after, in
    /// order, with the same rules, so this device still shows its own tap.
    public var heldReactions: [Reaction] { ReactionFold.held(ops: reactions) }

    /// The emoji `reactor` holds on this message, in the order the fold kept.
    public func heldEmoji(of reactor: MemberIdentity?) -> [String] {
        guard let reactor else { return [] }
        return heldReactions.filter { $0.reactor == reactor }.map(\.emoji)
    }

    /// The distinct emoji held on this message by anyone.
    public var heldKinds: [String] {
        var seen: [String] = []
        for r in heldReactions where !seen.contains(r.emoji) { seen.append(r.emoji) }
        return seen
    }
}

/// ADR-0052 decision 1 — a reply's reference to the message it answers.
///
/// `target` is the quoted message's DS `messageId`, the same handle
/// edit / delete / react use. `author` and `excerpt` are the SNAPSHOT the
/// replier took when Reply was chosen: a claim by the replier, carried in the
/// envelope so that everyone in the conversation draws the same quote, and
/// never dressed as verified. Either may be `nil` on a reply that arrived
/// with only the target id; the quote block then draws an empty name line
/// and the excerpt "Message" (``replyQuoteLines(_:)``).
public struct ReplyRef: Equatable, Codable {
    public let target: Int64
    public let author: String?
    public let excerpt: String?

    public init(target: Int64, author: String?, excerpt: String?) {
        self.target = target
        self.author = author
        self.excerpt = excerpt
    }
}

/// ADR-0052 decision 2 — what a forwarded message carries about where it
/// came from.
///
/// `author` is the ORIGINAL author's display name as the forwarder's timeline
/// showed it when Forward was chosen: a snapshot and a courtesy label, never
/// a verified identity, never a route to that person
/// (`docs/ux/message-actions.md` section 5). `originGroupIdHex` and
/// `originMessageId` name the original message — both or neither — so a
/// recipient who also holds that conversation can jump to it; for anyone else
/// the pair is an opaque id they cannot open. Every field may be `nil` on a
/// forward that arrived without it; the bubble then reads "Forwarded".
public struct ForwardRef: Equatable, Codable {
    public let author: String?
    public let originGroupIdHex: String?
    public let originMessageId: Int64?

    public init(author: String?, originGroupIdHex: String?, originMessageId: Int64?) {
        self.author = author
        // The origin is a pair: half of one points at nothing.
        if let originGroupIdHex, let originMessageId {
            self.originGroupIdHex = originGroupIdHex
            self.originMessageId = originMessageId
        } else {
            self.originGroupIdHex = nil
            self.originMessageId = nil
        }
    }

    /// Whether this forward names the original message.
    public var hasOrigin: Bool { originGroupIdHex != nil && originMessageId != nil }
}

/// ADR-0023 — a single reaction: who reacted with which emoji.
///
/// `reactor` is the member's signature key, never their MLS leaf. The
/// leaf-renumbering "open question" this type used to carry was not an open
/// question but a live defect: a member added into a departed member's vacated
/// leaf toggled the departed member's reaction OFF, and because removing the
/// last reactor prunes the emoji from the set entirely, the chip disappeared
/// rather than merely miscounting. See ``AuthorIdentity``.
public struct Reaction: Equatable {
    public let reactor: MemberIdentity
    public let emoji: String
    /// Reactions-and-emoji spec section 17.3 — an explicit add or remove.
    /// `ChatMessage.reactions` is the op log; ``ChatMessage/heldReactions``
    /// is what the Rust fold keeps of it.
    public let add: Bool
    /// The op's DS message id: ops fold in ascending order (0 for an op
    /// whose id is not known, which sorts first).
    public let at: Int64

    public init(reactor: MemberIdentity, emoji: String, add: Bool = true, at: Int64 = 0) {
        self.reactor = reactor
        self.emoji = emoji
        self.add = add
        self.at = at
    }
}

/// ADR-0023 — aggregated reaction row for the chip UI.
public struct ReactionTally: Equatable, Identifiable {
    public let emoji: String
    public let count: Int
    /// `true` when the local member contributed this emoji —
    /// tapping the chip toggles it back off.
    public let reactedByMe: Bool

    public var id: String { emoji }

    public init(emoji: String, count: Int, reactedByMe: Bool) {
        self.emoji = emoji
        self.count = count
        self.reactedByMe = reactedByMe
    }
}

public enum Direction: Equatable {
    case outgoing
    case incoming
    case system
}

// MARK: - Internal per-group record

/// Mutable per-group state — separate from the immutable
/// ``ChatThread`` snapshot the StateFlow surfaces.
///
/// Class (reference type) rather than struct because the inner
/// `Group` UniFFI handle is a class-bound reference + we want
/// the mutation paths to update the same record without copy-
/// on-write.
final class GroupRecord {
    let group: Group
    let groupId: Data
    private(set) var messages: [ChatMessage] = []
    private(set) var cursor: Int64 = 0
    /// ADR-0027 / UX §7.1 — an external-join request arrived that the core did
    /// NOT keep (the quarantine was full). It is absent from
    /// ``Group/pendingExternalJoins()`` and can be approved by nobody, so the
    /// members sheet cannot learn about it from the retained set — this is the
    /// only place the fact survives.
    ///
    /// Read by the sheet on its next refresh and cleared when an admin acts on
    /// the queue, which is what may have made room.
    var sawUnkeptExternalJoin: Bool = false
    /// ADR-0057 — the accounts the currently staged sweep evicts, recorded at
    /// the stage so its registration can revoke their registrar keys. Empty
    /// after a restart (an owed verdict re-registers nothing; the owner's
    /// reconcile dismisses the departed registrars afterwards).
    var stagedEvictedAccountKeys: [Data] = []
    /// The DS message id of this device's own staged commit, set when the
    /// commit route answered it; the membership row of the merge is filed
    /// under it (``ChatRepository/noteOwnCommitMembershipChange``).
    var ownPublishedCommitMessageId: Int64?
    /// ADR-0053 point 7 — how far this device has read the group's knocks. Its
    /// own id space, never mixed with ``cursor``. Seeded from, and written back
    /// to, a per-account `UserDefaults` key by the knock poll.
    var knockCursor: Int64 = 0

    init(group: Group, groupId: Data) {
        self.group = group
        self.groupId = groupId
    }

    /// Phase 2a3f1 — seed the in-memory timeline + cursor from
    /// a persistent snapshot. Called from `ChatRepository.bootstrap`
    /// after the live `Group` handle has been rehydrated; the
    /// MLS state + the message log are stored separately on
    /// disk and the order they get reattached doesn't matter.
    /// Idempotent on the message ids — duplicate seeds replace
    /// rather than append.
    func seed(
        messages: [ChatMessage],
        cursor: Int64,
        lastViewedMessageId: Int64? = nil,
        feedSeen: Set<Int64> = [],
        syncedThroughMessageId: Int64? = nil
    ) {
        self.messages = messages
        self.cursor = cursor
        self.lastViewedMessageId = lastViewedMessageId
        self.feedSeen = feedSeen
        self.syncedThroughMessageId = syncedThroughMessageId
    }

    /// `docs/ux/second-device.md` section 2 — the highest id a pairing bundle
    /// carried for this group; `nil` when none did. Moves up only.
    private(set) var syncedThroughMessageId: Int64?

    /// Take in what the log held for this group: the rows of a history bundle
    /// and its floor. The journal cursor stays this record's own.
    func adoptHistory(_ slot: PersistedGroup) {
        syncedThroughMessageId = PairingHistory.higherFloor(
            syncedThroughMessageId, slot.syncedThroughMessageId)
        messages = PairingHistory.mergeLiveRows(
            held: messages, bundle: slot.messages.compactMap { $0.toChatMessage() })
        lastViewedMessageId = PairingHistory.higherFloor(lastViewedMessageId, slot.lastViewedMessageId)
        feedSeen.formUnion(slot.feedSeen ?? [])
    }

    /// A polled row the history bundle already brought: it stays as it is.
    private func heldFromBundle(_ messageId: Int64) -> Bool {
        PairingHistory.heldFromBundle(
            floor: syncedThroughMessageId, messageId: messageId, rows: messages)
    }

    /// ADR-0063 section 5 — this device's read watermark for the group and
    /// the feed-seen ids above it. Persisted with the message log.
    var lastViewedMessageId: Int64?
    var feedSeen: Set<Int64> = []

    /// The rows as the seen rule reads them.
    var seenRows: [FeedSeen.Row] {
        messages.map { FeedSeen.Row(messageId: $0.messageId, incoming: $0.direction == .incoming) }
    }

    /// The chat screen shows everything it holds: move the watermark to the
    /// highest server id and drop the feed ids below it.
    func markAllViewed() -> Bool {
        let highest = messages.lazy.map(\.messageId).filter { $0 > 0 }.max()
        guard let highest, highest > (lastViewedMessageId ?? Int64.min) else { return false }
        lastViewedMessageId = highest
        feedSeen = feedSeen.filter { $0 > highest }
        return true
    }

    /// ADR-0045 — the sweep's verdict APPLIES the journal rows below its own
    /// commit, so it must advance the same cursor the poll drives or the poll
    /// would re-feed consumed rows to OpenMLS. Monotone: a stale caller can
    /// never wind it backward.
    func advanceCursor(to newCursor: Int64) {
        if newCursor > cursor { cursor = newCursor }
    }

    /// The next identity for a row this device creates.
    ///
    /// Negative and below every id this group already holds. DS ids are
    /// positive and monotone, so a local row can never collide with one.
    /// Allocated over ``ChatMessage/id`` rather than over `messageId`,
    /// because identity is what must be unique and the two are not the same
    /// field for a row that has not landed yet - a call row and an unsent
    /// message both live down here.
    func nextLocalId() -> Int64 {
        min(0, messages.map(\.id).min() ?? 0) - 1
    }

    /// Put a message the user just wrote into the conversation, before it has
    /// been anywhere. It settles through ``markSent(localId:messageId:createdAt:)``
    /// or ``markFailed(localId:)``.
    /// The rows this device created that never reached the server (sending
    /// or failed), as they stand.
    var unsentRows: [ChatMessage] {
        messages.filter { $0.messageId <= 0 && $0.localId != 0 }
    }

    /// Adopt an unsent row from another segment of the same conversation
    /// (ADR-0073 R.5): the same row under a local id of this record, in the
    /// state it was in.
    @discardableResult
    func adoptUnsentRow(_ row: ChatMessage, reply: ReplyRef?) -> Int64 {
        let localId = nextLocalId()
        messages.append(
            ChatMessage(
                messageId: 0, direction: .outgoing, text: row.text, attachments: row.attachments,
                createdAt: row.createdAt, authorKeyB64: row.authorKeyB64, ack: row.ack,
                localId: localId, reply: reply, forward: row.forward))
        return localId
    }

    /// Take a row this device created out of the timeline (ADR-0073 R.7
    /// case 1: a held message leaves the old segment when it is flushed into
    /// the successor).
    func removeLocalRow(localId: Int64) {
        messages.removeAll { $0.localId == localId && $0.messageId <= 0 }
    }

    func appendPending(
        localId: Int64,
        text: String,
        attachments: [AttachmentRef],
        createdAt: String,
        authorKeyB64: MemberIdentity?,
        reply: ReplyRef? = nil,
        forward: ForwardRef? = nil
    ) {
        messages.append(
            ChatMessage(
                messageId: 0,
                direction: .outgoing,
                text: text,
                attachments: attachments,
                createdAt: createdAt,
                authorKeyB64: authorKeyB64,
                ack: .pending,
                localId: localId,
                reply: reply,
                forward: forward
            )
        )
    }

    /// The send landed: take the server's id and time, and stop saying
    /// "Sending...". The identity does not move, so the bubble is not rebuilt.
    func markSent(localId: Int64, messageId: Int64, createdAt: String) {
        guard let idx = messages.firstIndex(where: { $0.localId == localId }) else { return }
        let old = messages[idx]
        messages[idx] = ChatMessage(
            messageId: messageId,
            direction: old.direction,
            text: old.text,
            attachments: old.attachments,
            createdAt: createdAt,
            authorKeyB64: old.authorKeyB64,
            edited: old.edited,
            deleted: old.deleted,
            reactions: old.reactions,
            ack: .sent,
            localId: localId,
            reply: old.reply,
            forward: old.forward,
            lastOpId: old.lastOpId
        )
    }

    /// Trying again: back to "Sending...".
    func markPending(localId: Int64) {
        guard let idx = messages.firstIndex(where: { $0.localId == localId }) else { return }
        messages[idx].ack = .pending
    }

    /// The send did not get there. The row STAYS, with its text: that is the
    /// whole point of appending it before the publish.
    func markFailed(localId: Int64) {
        guard let idx = messages.firstIndex(where: { $0.localId == localId }) else { return }
        messages[idx].ack = .failed
    }

    func appendOutgoing(
        text: String,
        attachments: [AttachmentRef] = [],
        messageId: Int64,
        createdAt: String,
        authorKeyB64: MemberIdentity?
    ) {
        messages.append(
            ChatMessage(
                messageId: messageId,
                direction: .outgoing,
                text: text,
                attachments: attachments,
                createdAt: createdAt,
                authorKeyB64: authorKeyB64
            )
        )
    }

    /// `docs/ux/calls.md` section 6 — append a row this device wrote itself
    /// (today: a finished call). A system row with no author: nobody sent it,
    /// so nobody may edit or delete it.
    func appendLocalRow(text: String, messageId: Int64, createdAt: String) {
        messages.append(
            ChatMessage(
                messageId: messageId,
                direction: .system,
                text: text,
                createdAt: createdAt
            )
        )
    }

    /// ADR-0023 — apply an edit op locally to our own outgoing
    /// message (the sender never receives its own app message
    /// back, so the send path mutates the timeline directly).
    func applyLocalEdit(target: Int64, newText: String, opMessageId: Int64? = nil) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        guard !messages[idx].deleted else { return }
        messages[idx].text = newText
        messages[idx].edited = true
        messages[idx].noteOp(opMessageId)
    }

    /// ADR-0023 — apply a delete op locally to our own outgoing
    /// message.
    func applyLocalDelete(target: Int64, opMessageId: Int64? = nil) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        messages[idx].deleted = true
        messages[idx].noteOp(opMessageId)
        MessageTranslationPurge.messageDeleted(groupId: groupId, messageId: target)
    }

    /// ADR-0023 — toggle our own reaction locally on `target`.
    func applyLocalReaction(
        target: Int64, emoji: String, reactor: MemberIdentity, add: Bool, at: Int64
    ) {
        recordReaction(target: target, reactor: reactor, emoji: emoji, add: add, at: at)
    }

    /// Consume a batch of receive-path events — the ONE place a decoded event
    /// becomes record state.
    ///
    /// Two callers, and that is the point. The poll is the obvious one
    /// (``ChatRepository`` calls this directly from its locked apply region).
    /// The other is the ADR-0045 committer sweep: its verdict applies every
    /// journal row the DS ordered below its own commit and then advances the SAME
    /// cursor over them, so those rows are gone from the poll's future forever. A
    /// chat message another member sent in the window between the sweep's
    /// stand-down re-read and its publish lands in exactly that set, and before
    /// this it was decrypted and then thrown away. Feeding both paths through this
    /// function is what keeps the two from drifting — every envelope kind is
    /// folded here once.
    ///
    /// Deliberately does NOT touch ``cursor``, and neither does any other
    /// event-folding entry point: since the receive-durability fix, the ONLY way
    /// this record's cursor moves on either path is
    /// ``applyDurableCursorBatch(gate:touchedMlsState:persist:advance:)``, which
    /// refuses to advance it past a batch whose snapshot did not reach disk. A
    /// second writer here would not merely let one path wind the other's cursor
    /// — it would route around that gate, which is the defect it exists to
    /// prevent. See `Chat/DurableCursor.swift`.
    ///
    /// Events must arrive in journal order; the op fold assumes it.
    func consumeEvents(
        _ events: [IncomingChatEvent],
        ownAuthorKey: MemberIdentity? = nil,
        callSignalingSink: ((Int64, Data, String, UInt32?, MemberIdentity?, MemberIdentity?) -> Void)? = nil,
        readReceiptSink: ((MemberIdentity, Int64) -> Void)? = nil,
        chatStatusSink: ((ChatStatusEvent) -> Void)? = nil,
        pushRoutingSecretSink: ((Data, UInt64, UInt32?) -> Void)? = nil,
        committerBeaconSink: ((UInt32?, String) -> Void)? = nil,
        membershipProposalSink: ((ProposalRecord, String) -> Void)? = nil,
        profileSink: ((ProfileClaim, MemberIdentity?) -> Void)? = nil,
        groupNameSink: ((String, UInt64, UInt32?) -> Void)? = nil,
        profileRequestSink: ((MemberIdentity) -> Void)? = nil,
        groupContactRequestSink: ((Data, MemberIdentity) -> Void)? = nil,
        pinSink: ((FfiPinOp, UInt32?) -> Void)? = nil,
        registrarSeedSink: ((Data, UInt32?) -> Void)? = nil,
        scopedDiscoverySink: ((ScopedDiscoveryEnvelope, MemberIdentity?) -> Void)? = nil,
        sharedChannelsSink: ((FfiSharedChannels, MemberIdentity?) -> Void)? = nil,
        groupClosedSink: ((Int64, Data?, UInt32?) -> Void)? = nil,
        groupSettingSink: ((GroupSettingEnvelope, UInt32?) -> Void)? = nil,
        reactionToOwnSink: ((Int64) -> Void)? = nil
    ) {
        for event in events {
            switch event {
            case .application(
                let messageId, let text, let attachments, let senderKey, let createdAt, let reply,
                let forward):
                // ADR-0050 — a message from this PERSON's other device
                // arrives as an incoming row (another leaf sealed it) but is
                // ours: same side, no sender name.
                let fromOwnDevice = senderKey != nil && senderKey == ownAuthorKey
                if heldFromBundle(messageId) { continue }
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: fromOwnDevice ? .outgoing : .incoming,
                        text: text,
                        attachments: attachments,
                        createdAt: createdAt,
                        authorKeyB64: senderKey,
                        reply: reply,
                        forward: forward
                    )
                )
            case .unsupported(let messageId, let version, let senderKey, let createdAt):
                // ADR-0061 section 6 — a message this build cannot read: a
                // row in the timeline (it is a real message the person was
                // sent), drawn from the version at display time.
                let fromOwnDevice = senderKey != nil && senderKey == ownAuthorKey
                if heldFromBundle(messageId) { continue }
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: fromOwnDevice ? .outgoing : .incoming,
                        text: "",
                        createdAt: createdAt,
                        authorKeyB64: senderKey,
                        unsupportedVersion: version
                    )
                )
            case .op(let opMessageId, let op, let target, let text, let emoji, let senderKey, let createdAt):
                // ADR-0024 — typing / read-receipt ops render NO bubble
                // and never touch the timeline. They surface to the view
                // model via `chatStatusSink` (ephemeral typing state +
                // monotone read watermark). The ADR-0023 mutation ops
                // (edit / delete / reaction) still fold into the target.
                switch op {
                case .typing(let isTyping):
                    if let senderKey = senderKey {
                        chatStatusSink?(
                            .typing(
                                sender: senderKey,
                                isTyping: isTyping,
                                createdAt: createdAt
                            )
                        )
                    }
                    continue
                case .readReceipt:
                    // The durable watermark is fed here, independent of
                    // whether any chat surface is attached; our own echo
                    // (another device of this account) is not a reader.
                    if let senderKey = senderKey, senderKey != ownAuthorKey {
                        readReceiptSink?(senderKey, target)
                    }
                    if let senderKey = senderKey {
                        chatStatusSink?(
                            .readReceipt(sender: senderKey, upTo: target)
                        )
                    }
                    continue
                case .edit, .delete, .reaction:
                    applyIncomingOp(
                        op: op,
                        target: target,
                        text: text,
                        emoji: emoji,
                        senderKey: senderKey,
                        at: opMessageId
                    )
                    // Reactions-and-emoji spec section 12 — someone else added
                    // a reaction to one of this person's own messages.
                    if case .reaction(true, _) = op, let senderKey, senderKey != ownAuthorKey,
                       messages.contains(where: { $0.messageId == target && $0.direction == .outgoing }) {
                        reactionToOwnSink?(target)
                    }
                }
            case .system(let messageId, let systemKind, let createdAt):
                if heldFromBundle(messageId) { continue }
                messages.append(
                    ChatMessage(
                        messageId: messageId,
                        direction: .system,
                        text: Self.systemRowText(systemKind),
                        createdAt: createdAt
                    )
                )
            case .callSignaling(
                let messageId, let payloadBytes, let createdAt, let senderLeaf, let senderDeviceKey,
                let senderKey):
                // Phase 3a9 — route into the app-level call host (1x1) or the
                // group-call record (group bodies). Out-of-band — never touches
                // the chat timeline. The DS message id rides along: it is half
                // of the call host's exactly-once key.
                callSignalingSink?(messageId, payloadBytes, createdAt, senderLeaf, senderDeviceKey, senderKey)
            case .externalJoinRequest(_, _, let retained, _):
                // ADR-0027 (G-3) — a non-member's external Add proposal,
                // retained pending an owner/admin approval. Not a timeline
                // row; the members sheet reads the retained set via
                // `pendingExternalJoins(groupIdHex:)`. The `state` republish
                // below nudges the view model to re-query when its sheet is
                // open.
                //
                // Except for the one thing that read cannot see. A request the
                // core did NOT keep is absent from the retained set, so it
                // renders as nothing at all — correct about the Approve button
                // and silent about the joiner, who is now waiting on an admin
                // nobody told. §7.1 recorded that state as unsurfaceable;
                // `retained` is what made it statable, so it is recorded here
                // rather than dropped.
                if case .informational = externalJoinAffordance(retained: retained) {
                    sawUnkeptExternalJoin = true
                }
            case .pushRoutingSecret(_, let secret, let mintEpoch, let senderLeaf, _):
                // ADR-0042 — a `push_routing_secret` distribution envelope.
                // Not a timeline row: routed out to the repository, which owns
                // the push-routing store AND the live `Group` handle the owner
                // gate needs. The MLS-authenticated `senderLeaf` rides along so
                // only the ADR-0022 owner can CHANGE the secret
                // (`lakko-security` MEDIUM-1); on accept the repository persists
                // it (idempotent) + subscribes under the derived opaque
                // `push_gid` (invariant #5). ADR-0044 — the `mintEpoch` rides
                // along so the gate can also ask whether the envelope is NEWER
                // than what is held, which is what refuses a stale replay.
                pushRoutingSecretSink?(secret, mintEpoch, senderLeaf)
            case .committerBeacon(_, let senderLeaf, let createdAt):
                // ADR-0045 decision 2 — a committer-liveness beacon. Not a
                // timeline row: it is an observation about WHICH owner / admin
                // can sweep the pending proposal set, carried by the event's
                // `senderLeaf` + `createdAt` (the two things the field-less
                // envelope deliberately omits). Routed out to the repository,
                // which owns the observation store AND the live `Group` handle
                // the leaf-to-signature-key resolution needs.
                committerBeaconSink?(senderLeaf, createdAt)
            case .membershipProposal(_, let record, let createdAt):
                // ADR-0045 — a membership proposal is a PANEL signal, never a
                // timeline row (UX §10). Routed out so the repository can stamp
                // it with the DS row's time; the core's retained pending set
                // stays the truth about what is pending.
                membershipProposalSink?(record, createdAt)
            case .profile(_, let profile, let senderKey, _):
                // ADR-0048 — a member's profile. Not a timeline row: routed out
                // with the signature key resolved at receipt (C-68), which is
                // the only identity it may ever be stored under.
                profileSink?(profile, senderKey)
            case .scopedDiscovery(_, let envelope, let senderKey, _):
                // ADR-0060 — a "who can find me" service envelope. Not a
                // timeline row: routed out with the sender resolved at receipt.
                scopedDiscoverySink?(envelope, senderKey)
            case .groupName(_, let name, let updatedAtMs, let senderLeaf, _):
                // ADR-0051 — the group's shared name. Not a timeline row:
                // routed out to the repository, which owns the name store AND
                // the live `Group` handle the role gate needs. The
                // MLS-authenticated `senderLeaf` rides along so only the
                // owner / admins can set it; the stamp so the strictly-newer
                // order can refuse a stale re-send.
                groupNameSink?(name, updatedAtMs, senderLeaf)
            case .groupSetting(_, let setting, let senderLeaf, _):
                // ADR-0087 — the group's photo, allowed reactions, @name or a
                // knock seed. Not a timeline row: routed out to the
                // repository, which gates it against the live group.
                groupSettingSink?(setting, senderLeaf)
            case .groupClosed(let messageId, _, let successorGroupId, let senderLeaf, _):
                // ADR-0073 R.6 — a successor claim. Never a timeline row and
                // never anything a person sees: routed out to the repository,
                // which judges it against its own accepted reset.
                groupClosedSink?(messageId, successorGroupId, senderLeaf)
            case .profileRequest(_, let senderKey, _):
                // ADR-0048 §2 trigger 4 — someone asked the group to introduce
                // itself. Not a timeline row: routed out so the repository can
                // republish this device's own profile through the growth path.
                // This account's own echo (another of our devices asking) is
                // not a question for us, the same exclusion the read watermark
                // applies; and a request with no resolvable sender is dropped,
                // never answered — the fail-closed side, as for the beacon.
                guard let senderKey, senderKey != ownAuthorKey else { continue }
                profileRequestSink?(senderKey)
            case .groupContactRequest(_, let envelope, let senderKey, _):
                // `docs/ux/who-can-find-me.md` 6.4 — never a timeline row.
                // Our own (sent from another of our devices) is not for us;
                // one with no resolvable sender is dropped, since the check
                // binds the request to the poster.
                guard let senderKey, senderKey != ownAuthorKey else { continue }
                groupContactRequestSink?(envelope, senderKey)
            case .pin(_, let op, let senderLeaf, _):
                // ADR-0052 decision 3 — a shared pin / unpin of one message.
                // Not a timeline row: routed out to the repository, which owns
                // the pin store AND the live `Group` handle the standing check
                // needs. The MLS-authenticated `senderLeaf` rides along so only
                // someone with standing can pin for everyone; the stamp so the
                // strictly-newer order can refuse a stale re-send.
                pinSink?(op, senderLeaf)
            case .registrarSeed(_, let plaintext, let senderLeaf, _):
                // ADR-0057 — a registrar seed from the journal root. Not a
                // timeline row: routed out to the repository, which gates it on
                // the addressee and on the sender being the owner, then opens
                // and stores it.
                registrarSeedSink?(plaintext, senderLeaf)
            case .serviceEnvelope:
                // A service envelope this build cannot read (a newer version
                // from a client ahead of this one). Dropped: no bubble, no
                // chat-list preview, no unread count.
                continue
            case .sharedChannels(_, let list, let senderKey, _):
                // ADR-0059 section 6 — a member's shared channel list. Not a
                // timeline row: routed out with the ACCOUNT key resolved at
                // receipt (C-68), the only key it may be filed under. A list
                // that failed its bounds, or has no attributable sender, is
                // dropped here.
                guard let list, let senderKey else { continue }
                sharedChannelsSink?(list, senderKey)
            }
        }
    }

    /// ADR-0023 — fold an incoming edit / delete / reaction op
    /// into its target message. The poll is incremental so each
    /// op is applied exactly once.
    ///
    /// **Author-only gate** (the security core): an edit or
    /// delete is applied ONLY when the op's sender IDENTITY equals
    /// the target message's stored author identity; otherwise it is
    /// dropped (fail-closed — invariant #4). An unknown target or
    /// an op with no resolvable sender is likewise dropped. A reaction
    /// is accepted from any member (reacting to others' messages
    /// is the point) but attributed to its sender identity for dedup
    /// + per-user toggle.
    ///
    /// Both comparisons are on the **signature key**, resolved back in the
    /// receive loop from the MLS sender leaf in the epoch that named it.
    /// Comparing the raw leaves here is what let a member added into a
    /// departed member's vacated leaf rewrite and erase that member's history
    /// — and, because the author leaf was persisted, keep doing so across
    /// every restart. See ``AuthorIdentity``.
    private func applyIncomingOp(
        op: OpKind,
        target: Int64,
        text: String,
        emoji: String?,
        senderKey: MemberIdentity?,
        at: Int64
    ) {
        guard let senderKey = senderKey else { return }
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        switch op {
        case .edit:
            guard AuthorIdentity.editDeleteAuthorized(
                author: messages[idx].authorKeyB64, opSender: senderKey) else { return }
            guard !messages[idx].deleted else { return }
            messages[idx].text = text
            messages[idx].edited = true
            messages[idx].noteOp(at)
        case .delete:
            guard AuthorIdentity.editDeleteAuthorized(
                author: messages[idx].authorKeyB64, opSender: senderKey) else { return }
            messages[idx].deleted = true
            messages[idx].noteOp(at)
            MessageTranslationPurge.messageDeleted(groupId: groupId, messageId: target)
        case .reaction(let add, _):
            guard let emoji = emoji, !emoji.isEmpty else { return }
            recordReaction(target: target, reactor: senderKey, emoji: emoji, add: add, at: at)
        case .typing, .readReceipt:
            // ADR-0024 — typing / read-receipt ops are routed out of
            // the timeline fold in `consumeEvents` (they render no
            // bubble), so they never reach this path. Handled here only
            // to keep the switch exhaustive.
            return
        }
    }

    /// Append one reaction op to `target`'s op log. The fold
    /// (``ChatMessage/heldReactions``) decides what it comes to: an add of an
    /// emoji the person holds and a remove of one they do not are no-ops, and
    /// a 4th add is ignored for good. Shared by the incoming-op fold and the
    /// local own-reaction path.
    ///
    /// `reactor` is the account key the sender leaf resolved to at receipt:
    /// keyed on the leaf, a newcomer at a vacated leaf would act on the
    /// departed member's reactions.
    private func recordReaction(
        target: Int64, reactor: MemberIdentity, emoji: String, add: Bool, at: Int64
    ) {
        guard let idx = messages.firstIndex(where: { $0.messageId == target }) else { return }
        // The same op seen twice (a re-poll of a row already applied) is one op.
        if at > 0, messages[idx].reactions.contains(where: {
            $0.at == at && $0.reactor == reactor && $0.emoji == emoji
        }) { return }
        messages[idx].reactions.append(Reaction(reactor: reactor, emoji: emoji, add: add, at: at))
    }

    func toThread(
        unread: Int = 0,
        conversationIdHex: String? = nil,
        history: [ChatMessage] = [],
        historyGroupIdHexes: [String] = [],
        segmentIndex: Int = 0,
        leaveRequested: Bool = false,
        arrivedAtMs: Int64? = nil
    ) -> ChatThread {
        ChatThread(
            groupIdHex: DsClient.pseudonymHex(groupId),
            messages: history.isEmpty ? messages : history + messages,
            cursor: cursor,
            unread: unread,
            feedSeen: feedSeen,
            lastViewedMessageId: lastViewedMessageId,
            // ADR-0045 UX §6.2.4 — `nil` (a torn handle) reports `false`: an
            // unreadable group is not evidence of an eviction, and this line
            // is a claim about fact rather than about capability.
            membershipEnded: readIsActive { try group.isActive() } == false,
            conversationIdHex: conversationIdHex,
            historyGroupIdHexes: historyGroupIdHexes,
            segmentIndex: segmentIndex,
            syncedThroughMessageId: syncedThroughMessageId,
            leaveRequested: leaveRequested,
            arrivedAtMs: arrivedAtMs
        )
    }

    /// Nothing of this record's history survives: it is about to be written
    /// away, and a persist of the log must not bring it back.
    func forgetHistory() {
        messages = []
        lastViewedMessageId = nil
        feedSeen = []
        syncedThroughMessageId = nil
    }

    /// ADR-0050 — this device's leaf was retired and it was re-admitted: the
    /// Welcome replaced the group, and the history and the journal position
    /// of the old record carry.
    func adoptRetiredRecord(_ old: GroupRecord) {
        messages = old.messages
        lastViewedMessageId = old.lastViewedMessageId
        feedSeen = old.feedSeen
        syncedThroughMessageId = old.syncedThroughMessageId
        advanceCursor(to: old.cursor)
    }

    /// `docs/ux/second-device.md` section 4, "Membership rows" — write `row`
    /// on the LAST plain commit row at or after `fromIndex` (the rows one
    /// batch appended). `false` when the batch filed no commit row.
    @discardableResult
    func recordMembershipChange(_ row: MembershipRow, fromIndex: Int) -> Bool {
        guard let index = messages.indices.last(where: {
            $0 >= fromIndex && messages[$0].direction == .system
                && messages[$0].text == MembershipRow.plainCommitText
        }) else { return false }
        messages[index].text = row.marker
        return true
    }

    private static func systemRowText(_ kind: SystemKind) -> String {
        switch kind {
        case .commit: return MembershipRow.plainCommitText
        case .externalJoinProposal: return "[mls external join]"
        }
    }
}

/// What a founder does with its new group's known-groups row when delivering
/// the Welcome fails (``ChatRepository/startGroupWith(recipientAccountId:recipientSignaturePubkey:kind:recipientPlan:)``).
/// The row is written before the first Welcome; pure so the rule is testable.
enum FoundingRollback {
    /// `true` when no Welcome went out: nobody else holds the group, and the
    /// row goes again. Once one did, the group has a member and the row
    /// stays. An accept never reaches this: its founding is on record and
    /// stays owed.
    static func forgetsRow(welcomesDelivered: Int) -> Bool {
        welcomesDelivered == 0
    }
}

/// How founding the one-to-one of an accept ended.
public enum AnswerFounding: Equatable {
    /// The requester's Welcome was written.
    case delivered(groupIdHex: String)
    /// The group stands and the requester's Welcome is owed.
    case welcomeOwed(groupIdHex: String)

    public var groupIdHex: String {
        switch self {
        case .delivered(let id), .welcomeOwed(let id): return id
        }
    }
}

/// The retry of an accept's owed Welcome, over injected steps. An entry goes
/// only after its Welcome landed, the ack only after that; a failure leaves
/// the entry for the next retry and sends no ack.
enum OwedAnswers {
    /// The group founded for request `requestMessageId`, when `rows` hold its
    /// record: another Accept carries on from it and founds nothing.
    static func foundedGroupHex(requestMessageId: Int64, rows: [PendingCommitEntry]) -> String? {
        rows.first { $0.answerRequestMessageId == requestMessageId }
            .flatMap { base64Decode($0.groupId) }
            .map { DsClient.pseudonymHex($0) }
    }

    struct Ports {
        var deliver: @MainActor (PendingCommitEntry) async throws -> Void
        var landed: @MainActor (PendingCommitEntry) -> Void
        var ack: @MainActor (Int64) async -> Void
    }

    @MainActor
    static func resolve(rows: [PendingCommitEntry], ports: Ports) async {
        for row in rows {
            do {
                try await ports.deliver(row)
            } catch {
                NSLog("contact accept: an owed Welcome was not delivered: %@", ContactAccept.reason(of: error))
                continue
            }
            ports.landed(row)
            if let messageId = row.answerRequestMessageId { await ports.ack(messageId) }
        }
    }
}
