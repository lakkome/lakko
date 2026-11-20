// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.ChannelRotationState
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelRotationPolicyDefault
import uniffi.lakko_ffi_uniffi.channelRotationStateAfterRotation

/**
 * ADR-0087 section 12 / plan step 9 — the machinery a channel needs, run by
 * the app rather than by a screen: the rotation heartbeat, the co-publisher
 * requests read off the PublisherGroup's knock, the recorded commits that
 * still owe a step, the owed handle rekey and record republish, and a public
 * channel's seed envelope per epoch. Runs beside [SubscriberAdmission] in the
 * session shell, so a channel rotates whether or not its owner opens it.
 * Each pass reads only channels this device publishes.
 *
 * One instance per [ChannelsUiState] ([forChannels]); it owns the open
 * PublisherGroup handles, so no screen holds one. Every MLS call runs on the
 * shared [ChannelPublisherIdentity]; passes are serialised by one mutex.
 */
/** A vault delete after a channel delete waits at most this long (chosen). */
private suspend fun withTimeoutOrNullVault(block: suspend () -> Unit) {
    kotlinx.coroutines.withTimeoutOrNull(VAULT_DELETE_TIMEOUT_MS) { block() }
}

public class ChannelPublisherLoop private constructor(
    private val context: Context,
    private val state: ChannelsUiState,
    private val client: ChannelsClient,
) {
    init {
        CoPublisherBindingStore.init(context)
    }

    private val mutex = Mutex()
    private val groups = HashMap<String, PublisherGroup>()
    private val claimed = HashSet<String>()
    private val lastRotationCheckMs = HashMap<String, Long>()
    private val publicSeedFiled = HashMap<String, UInt>()
    private val ds = DsClient()
    // Per channel: where the journal and the knocks were read up to, and
    // when each read and the count last ran (the idle request budget).
    private val journalCursor = HashMap<String, Long>()
    private val knockCursor = HashMap<String, Long>()
    private val lastJournalMs = HashMap<String, Long>()
    private val lastKnockMs = HashMap<String, Long>()
    private val lastCountMs = HashMap<String, Long>()

    /** Channels a current publisher asked to rotate, read on their
     *  PublisherGroup (the owner's device; collapsed into one rotation). */
    private val rotateRequested = HashSet<String>()

    /** Rotate requests this co-publisher has yet to send, on disk: the
     *  subscriber row is already gone, so a lost request would leave the
     *  removed key reading until the owner's next scheduled rotation. */
    private val rotateRequestOwed: OwedRecordMarks = PrefsOwedRotateRequests(context) { state.boundAccount }

    /** Channel records a re-issue after a commit still owes, per account. */
    private val recordOwed: OwedRecordMarks = PrefsOwedRecordMarks(context) { state.boundAccount }

    private val _pending = MutableStateFlow<Map<String, List<ByteArray>>>(emptyMap())

    /** Per channel, the co-publisher requests waiting (each a 32-byte joiner
     *  key, the handle [approve] and [reject] take). Owner devices only. */
    public val pending: StateFlow<Map<String, List<ByteArray>>> = _pending.asStateFlow()

    private val _subscriberCounts = MutableStateFlow<Map<String, Long>>(emptyMap())

    /** Section 15 — the exact subscriber count per published channel, as
     *  the last pass read it. */
    public val subscriberCounts: StateFlow<Map<String, Long>> = _subscriberCounts.asStateFlow()

    /** One publisher of a channel as the PublisherGroup names it. */
    public data class Publisher(val accountKey: ByteArray, val owner: Boolean, val self: Boolean)

    private val _publishers = MutableStateFlow<Map<String, List<Publisher>>>(emptyMap())

    /** Section 15 — the publishers of each channel this device publishes,
     *  read from the PublisherGroup's members on every pass. */
    public val publishers: StateFlow<Map<String, List<Publisher>>> = _publishers.asStateFlow()

    /** Whether a publisher besides the owner remains in [channelId] (the
     *  `channelInfoSections` input); `true` until a pass has read it. */
    public fun hasOtherPublishers(channelId: String): Boolean =
        _publishers.value[channelId]?.any { !it.owner } ?: true

    /** Where a channel that ended on this device files its notice (the
     *  session shell sets it to the membership-end notice). */
    @Volatile
    public var onChannelEnded: ((channelId: String, name: String) -> Unit)? = null

    /** One pass over every channel this device publishes, then the rotations
     *  that came due (each after a random wait, ADR-0090 section 4). Never
     *  throws. */
    public suspend fun passAll() {
        if (state.boundAccount == null) return
        val due = LinkedHashMap<String, Boolean>()
        mutex.withLock {
            // ADR-0090 — a channel whose PublisherGroup this device now holds
            // (admitted by another device, or a relaunch before the device
            // identity was ready) gets its publisher keys.
            try {
                adoptHeldGroupsLocked()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "held publisher groups not adopted: ${e.javaClass.simpleName}")
            }
            // Security pass follow-up — a suspected reset that was a false
            // alarm lifts by itself once signed reads succeed again at the
            // held group's epoch.
            try {
                probeSuspendedLocked()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "suspended channels not probed: ${e.javaClass.simpleName}")
            }
            // ADR-0064 section 7 (amendment of 2026-11-14) — an owner device
            // that holds no PublisherGroup enters from the channel's vault
            // head; decision A below stays the fallback for a channel with no
            // head (never for one whose entry is still unsettled).
            val unsettled = try {
                enterFromVaultLocked()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "owned channels not entered from their heads: ${e.javaClass.simpleName}")
                null
            }
            // ADR-0090 decision A — this device is the account's only one.
            try {
                if (unsettled != null) refoundIfOwedLocked(unsettled)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "owned channels not re-founded yet: ${e.javaClass.simpleName}")
            }
            for (id in state.keysByChannel.value.keys.toList()) {
                try {
                    pass(id)?.let { forced -> due[id] = forced }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    if (ChannelRemovalWatch.isDeleted(e)) {
                        // Section 15 — the owner deleted the channel.
                        endHere(id)
                        continue
                    }
                    Log.w(TAG, "channel pass failed", e)
                    dropGroup(id)
                }
            }
            // Part B — a Welcome held because it came before this device
            // applied its own removal: tried again after the journal reads.
            try {
                retryHeldWelcomesLocked()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "held publisher Welcomes not retried: ${e.javaClass.simpleName}")
            }
        }
        // ADR-0090 section 4 — a due rotation waits a random 0 to 60 s outside
        // the pass lock, then re-reads the journal head and re-evaluates
        // before it stages: two owner devices rarely stage on one epoch, and
        // when they do the DS takes one.
        for ((id, forced) in due) {
            kotlinx.coroutines.delay(rotationJitter())
            mutex.withLock {
                try {
                    rotateDueLocked(id, forced)
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    if (forced) rotateRequested += id
                    dropGroup(id)
                    Log.w(TAG, "a due rotation did not run yet: ${e.javaClass.simpleName}")
                }
            }
        }
        // Section 13a — reaction writes a failed send left owed.
        ChannelFeedbackOutbox.resendAll(context, state, client)
    }

    /** The random wait before a due rotation is staged. */
    internal var rotationJitter: () -> Long = { kotlin.random.Random.nextLong(0L, ROTATION_JITTER_MAX_MS + 1) }

    /** The channel ended on this device (deleted, or this account is no
     *  longer one of its publishers): its state goes from every device of
     *  the account, as an unsubscribe does (section 9). */
    private fun endHere(channelId: String) {
        val name = state.channels.value.firstOrNull { it.id == channelId }
            ?.let { it.sharedName.ifBlank { it.localName } }.orEmpty()
        dropGroup(channelId)
        state.boundAccount?.let {
            HeldPublisherWelcomes.clear(context, it, channelId)
            // Security pass F2/F3 - the replay records go with the channel.
            JoinedPublisherWelcomes.clear(context, it, channelId)
            DeletedPublisherGroupEpochs.clear(context, it, channelId)
        }
        rotateRequestOwed.clear(channelId)
        _publishers.update { it - channelId }
        _pending.update { it - channelId }
        ChannelRemovalWatch.endEverywhere(state, channelId, name, onChannelEnded)
    }

    private suspend fun readPublishers(pub: ChannelPublisherIdentity, channelId: String, pg: PublisherGroup) {
        val (members, ownerKey) = try {
            pub.withChannel { ids -> ids.mls { pg.members() to pg.ownerAccountKey() } }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            return
        }
        val ownKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        _publishers.update { it + (channelId to publishersOf(members, ownerKey, ownKey)) }
    }

    /** After another device's commit was applied, the composer takes the seed
     *  of the group's epoch (a seed-changing commit of an owner device: a
     *  rotation, an admission, a publisher change) for that channel epoch
     *  ([ChannelsUiState.notePublisherSeedAt]). No-op when nothing moved. */
    private suspend fun refreshSeedAfterPeerCommit(pub: ChannelPublisherIdentity, pg: PublisherGroup, channelId: String) {
        try {
            val read = pub.withChannel { ids ->
                ids.mls { device ->
                    if (!pg.isActive() || pg.hasStagedCommit()) {
                        null
                    } else {
                        pg.epoch() to pg.channelSeed(device)
                    }
                }
            } ?: return
            // 2026-11-20 — a group more than one epoch ahead of the service
            // (a run of commits whose reseal never landed) maps its current
            // epoch to the service's next one, where the reseal that lands
            // will seal it.
            val service = state.epochByChannel.value[channelId]
            val anchored = service?.let { reanchoredOffset(read.first, ChannelEpochOffsets.get(channelId), it) }
            if (service != null && anchored != null) {
                // The service's epoch when it already serves this seed (a
                // device that applied the whole run after the reseal landed),
                // else the next one.
                val atService = state.subscribedSeeds(channelId)[service]?.contentEquals(read.second) == true
                val target = if (atService) service else service + 1u
                ChannelEpochOffsets.set(channelId, target.toLong() - read.first.toLong())
                state.notePublisherSeedAt(channelId, target, read.second)
                state.anchorSeed(channelId, target, read.second)
                Log.i(TAG, "a publisher group was ahead of the channel by more than one epoch; re-mapped")
                return
            }
            val epoch = ChannelEpochOffsets.channelEpochOf(channelId, read.first) ?: return
            val held = state.keysByChannel.value[channelId] ?: return
            if (held.seedEpoch == epoch && held.seed.contentEquals(read.second)) return
            state.notePublisherSeedAt(channelId, epoch, read.second)
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            Log.w(TAG, "publisher seed not refreshed: ${e.javaClass.simpleName}")
        }
    }

    /** Whether this account OWNS [channelId] AND this device holds its
     *  PublisherGroup (ADR-0090: then this device is an owner device that may
     *  commit; a device of the owner that holds no leaf reads and waits). */
    public fun isOwner(channelId: String): Boolean =
        state.publishesHere(channelId) && state.accountRoleOf(channelId) == ChannelAccountRole.OWNER

    private suspend fun groupFor(pub: ChannelPublisherIdentity, channelId: String): PublisherGroup {
        groups[channelId]?.let { return it }
        val opened = pub.withChannel { ids -> CoPublisher.openPublisherGroup(ids, channelId, state.coPublished) }
        groups[channelId] = opened
        return opened
    }

    /**
     * Run [block] (a Welcome replacing this device's PublisherGroup of
     * [channelId]) with the pass lock held and the loop's open handle of that
     * group closed first: the handle holds the group's old in-memory state,
     * and a pass using it after the replacement would act on the old group
     * (an inactive one would make this device a reader again). The next pass
     * reopens the group from the provider.
     */
    public suspend fun <T> releasingGroup(channelId: String, block: suspend () -> T): T =
        mutex.withLock {
            dropGroup(channelId)
            block()
        }

    private fun dropGroup(channelId: String) {
        groups.remove(channelId)?.close()
        // A reopened group reads its journal again from the start (rows it
        // already merged are skipped by the core).
        journalCursor.remove(channelId)
        knockCursor.remove(channelId)
        lastJournalMs.remove(channelId)
    }

    /** Signed journal reads refused in a row, per channel (section 2.5). */
    private val refusedReads = HashMap<String, Int>()

    /** Since when the channel service's epoch trails the group's (section 4,
     *  the unfinished-reseal takeover), per channel. */
    private val trailingSince = HashMap<String, Long>()

    /** When a device request for a device was last sent (section 2.2), keyed
     *  `channel|device hex`. */
    private val deviceRequestSentMs = HashMap<String, Long>()

    /** The admission mark last sent on each channel (section 6). */
    private val markSent = HashMap<String, Long>()

    /** Device requests read this pass, applied after the read (owner). */
    private class DeviceRequest(val sender: ByteArray, val plaintext: ByteArray, val decoded: uniffi.lakko_ffi_uniffi.FfiPublisherDeviceRequest)

    /**
     * One channel this device publishes. Returns `null`, or the rotation that
     * came due (`true` when a publisher asked for it, `false` when the
     * schedule says so).
     */
    private suspend fun pass(channelId: String): Boolean? {
        val pub = state.publisher ?: return null
        val account = state.boundAccount ?: return null
        if (channelId in state.orphaned.value) return null
        if (!pub.hasDevice()) return null
        val pg = groupFor(pub, channelId)
        val owner = isOwner(channelId)
        // Section 15 (phase B) — a co-publisher the owner removed (or that
        // stepped down) is no longer in the group: the channel ends here.
        val active = pub.withChannel { ids -> ids.mls { pg.isActive() } }
        if (!active && settleInactiveGroup(pub, channelId, pg, owner)) return null
        // ADR-0057 — an owner device claims the journal (an idempotent
        // re-send); a co-publisher never does.
        if (owner && claimed.add(channelId)) {
            try {
                pub.withChannel { ids ->
                    CoPublisher.claimJournal(ds, ids, pg, state.coPublished ?: InMemoryCoPublishedJournalMarks())
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                claimed.remove(channelId)
                Log.w(TAG, "publisher journal claim did not run", e)
            }
        }
        val store = state.commitStore
        // ADR-0090 — a staged commit no record names never left the device.
        if (owner && store != null) pub.withChannel { ids -> CoPublisher.abandonOrphanStage(ids, pg, account, store) }
        // Co-publisher requests (knock rows, owner devices, slowly) and peers'
        // commits (from the cursor), each on its own cadence.
        val now0 = System.currentTimeMillis()
        val readJournal = now0 - (lastJournalMs[channelId] ?: 0L) >= JOURNAL_INTERVAL_MS
        val readKnocks = owner && now0 - (lastKnockMs[channelId] ?: 0L) >= KNOCK_INTERVAL_MS
        // Security pass on the channel entry — the owner account's leaves
        // before the read, to see an entry (or any added leaf) it applies.
        val ownLeavesBefore = if (owner && (readJournal || readKnocks)) ownAccountLeaves(pub, pg) else null
        val poll = if (readJournal || readKnocks) {
            readJournalLocked(pub, pg, channelId, account, readKnocks) ?: return null
        } else {
            CoPublisher.JournalPoll(applied = 0, pending = 0)
        }
        // ADR-0064 section 7, security pass — an entry another device made
        // added a leaf of the owner account: the own-device reconcile
        // (ADR-0090 2.1, unchanged) runs now, so a leaf whose device is not
        // in the DeviceGroup roster is retired in this pass.
        var reconciled = false
        val after = if (ownLeavesBefore != null && poll.epochMoved) ownAccountLeaves(pub, pg) else emptyList()
        if (reconcileAfterRead(ownLeavesBefore, poll.epochMoved) { after }) {
            reconciled = true
            try {
                reconcileOwnDevices(pub, channelId, pg, account, pub.withChannel { ids -> ids.mls { pg.ownerAccountKey() } })
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "ADR-0064: the reconcile after an applied entry did not run yet: ${e.javaClass.simpleName}")
                return null
            }
            // The reconcile's commit dropped the handle: the next pass reopens it.
            if (groups[channelId] !== pg) return null
        }
        if (poll.applied > 0) {
            // ADR-0090 section 4 — a seed-changing commit of another owner
            // device starts the rotation clock again, at its DS row time.
            poll.lastCommitAtMs?.let { at ->
                state.saveRotationState(channelId, StoredRotationState(at.toULong(), 0u))
            }
            refreshSeedAfterPeerCommit(pub, pg, channelId)
        }
        writeHeadAfterPoll(pub, pg, owner, poll)
        readPublishers(pub, channelId, pg)
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val ownKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        val ownerKey = pub.withChannel { ids -> ids.mls { pg.ownerAccountKey() } }
        val publisherAccounts = _publishers.value[channelId].orEmpty().map { it.accountKey }
        val deviceRequests = ArrayList<DeviceRequest>()
        poll.applications.forEachIndexed { i, plaintext ->
            val sender = poll.senders.getOrNull(i)
            // Section 7 / 15, ADR-0090 section 6 — what an owner device sealed
            // to this account (a co-publisher's, or this owner account's own
            // other devices): an admission seed, the read key, earlier seeds.
            // Kept only from a device of the owner account, resolved at receipt.
            if (sender != null && sender.contentEquals(ownerKey)) {
                pub.withIdentity(persist = false) { id ->
                    ChannelAdmissionSeedShare.receive(context, account, channelId, id, plaintext)
                    ChannelAdmissionSeedShare.receiveReadKey(context, account, channelId, id, plaintext)
                    if (state.keysByChannel.value.containsKey(channelId)) {
                        ChannelAdmissionSeedShare.receiveEpochSeeds(state, channelId, id, plaintext)
                    }
                }
            }
            // ADR-0090 section 6 — the admission mark, kept as the maximum
            // from a current publisher.
            val mark = try {
                uniffi.lakko_ffi_uniffi.decodeChannelAdmissionMarkEnvelope(plaintext)
            } catch (_: Throwable) {
                null
            }
            if (mark != null) {
                if (mark.channelId.contentEquals(channelBytes)) {
                    // Security pass — only from a device of the OWNER account,
                    // and only up to the rows this device has read itself.
                    val verdict = admissionMarkVerdict(
                        held = ChannelAdmissionMark.get(context, account, channelId),
                        received = mark.throughSeq.toLong(),
                        sender = sender,
                        ownerKey = ownerKey,
                        seenThrough = ChannelAdmissionMark.seenThrough(context, account, channelId),
                        pendingHeld = ChannelAdmissionMark.pending(context, account, channelId),
                    )
                    if (sender != null && sender.contentEquals(ownerKey)) {
                        ChannelAdmissionMark.setPending(context, account, channelId, verdict.pending)
                    }
                    verdict.apply?.let { through ->
                        ChannelAdmissionMark.raise(context, account, channelId, through)
                        markSent[channelId] = maxOf(markSent[channelId] ?: 0L, through)
                    }
                }
                return@forEachIndexed
            }
            // ADR-0090 section 2.2 — a publisher's device request.
            val request = try {
                uniffi.lakko_ffi_uniffi.decodePublisherDeviceRequestEnvelope(plaintext)
            } catch (_: Throwable) {
                null
            }
            if (request != null) {
                if (owner && sender != null) deviceRequests += DeviceRequest(sender, plaintext, request)
                return@forEachIndexed
            }
            if (!owner || sender == null) return@forEachIndexed
            // Section 15 (phase B) — a co-publisher's posted binding is filed
            // for a later removal.
            val groupId = runCatching { pg.groupId() }.getOrNull()
            if (groupId != null && !sender.contentEquals(ownKey) &&
                CoPublisher.recordJournalBinding(groupId, channelBytes, sender, plaintext)
            ) {
                return@forEachIndexed
            }
            // ADR-0087 section 15 (2026-11-08), ADR-0090 decision C — a
            // co-publisher asks an owner device to rotate.
            val requested = try {
                uniffi.lakko_ffi_uniffi.decodeChannelRotateRequestEnvelope(plaintext)
            } catch (_: Throwable) {
                null
            }
            if (requested != null) {
                if (honoursRotateRequest(owner, requested, channelBytes, sender, ownKey, publisherAccounts)) {
                    rotateRequested += channelId
                }
                return@forEachIndexed
            }
            val stepDown = runCatching { uniffi.lakko_ffi_uniffi.isPublisherStepDown(plaintext, channelBytes) }
                .getOrDefault(false)
            if (!stepDown || sender.contentEquals(ownKey)) return@forEachIndexed
            try {
                removeLocked(channelId, sender)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "a publisher's step-down was not applied yet", e)
            }
        }
        // Phase B — a co-publisher posts its own binding once per epoch.
        val epochNow = pub.withChannel { ids -> ids.mls { pg.epoch() } }
        if (!owner && !CoPublisherBindingStore.posted(channelId, epochNow)) {
            try {
                pub.withChannel { ids -> CoPublisher.postBinding(ds, ids, pg, channelBytes) }
                CoPublisherBindingStore.markPosted(channelId, epochNow)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "the co-publisher binding was not posted yet: ${e.javaClass.simpleName}")
            }
        }
        if (owner) {
            // ADR-0090 open question 4 — the owner account's admission seeds
            // ride the account's register, so a restored device holds them.
            ChannelAdmissionKeys.syncToRegister(context, account, channelId)
            val epochSeeds = state.heldEpochSeeds(channelId)
            if (ChannelAdmissionSeedShare.anyOwed(context, account, channelId, epochSeeds.keys)) {
                pub.withChannel { ids ->
                    ChannelAdmissionSeedShare.sendOwed(context, account, channelId, ids, pg, ds, epochSeeds)
                }
            }
        }
        // A rotate request this co-publisher could not send yet.
        if (!owner && rotateRequestOwed.isOwed(channelId)) sendRotateRequestLocked(channelId)
        if (owner) {
            approveNamed(pub, channelId, pg)
            val waiting = pub.withChannel { ids -> CoPublisher.pending(ids, pg) }
            _pending.update { it + (channelId to waiting) }
            // ADR-0090 section 2.1 — the owner account's devices (once a pass).
            if (!reconciled) reconcileOwnDevices(pub, channelId, pg, account, ownerKey)
            // Section 2.2 — the publishers' device requests.
            for (r in deviceRequests) applyDeviceRequest(pub, channelId, pg, account, ownerKey, publisherAccounts, r)
        } else {
            // Section 2.2 — this co-publisher's other devices are asked for.
            requestOwnDevices(pub, channelId, pg, ownerKey)
        }
        // Section 6 — an owner mark this device could not count yet (it had
        // not read that far): counted now up to the rows it has read since.
        val pendingMark = ChannelAdmissionMark.pending(context, account, channelId)
        if (pendingMark > 0L) {
            val verdict = pendingAdmissionMark(
                ChannelAdmissionMark.get(context, account, channelId),
                pendingMark,
                ChannelAdmissionMark.seenThrough(context, account, channelId),
            )
            ChannelAdmissionMark.setPending(context, account, channelId, verdict.pending)
            verdict.apply?.let { through ->
                ChannelAdmissionMark.raise(context, account, channelId, through)
                markSent[channelId] = maxOf(markSent[channelId] ?: 0L, through)
            }
        }
        // Section 6 — the admission mark this device advanced goes to every
        // publishing device (only an owner device's mark is counted).
        if (owner) sendAdmissionMarkIfAdvanced(pub, channelId, pg, account)
        // 2026-11-20 — an owned channel whose owner proof was refused moves
        // its owner key to the current key first ([ChannelOwnerRepair]); each
        // channel is tried at most once per process.
        suspend fun repairOwnerKeyIfRefused() {
            if (!owner) return
            val admission = SubscriberAdmission.forChannels(state, client)
            ChannelOwnerRepair.repairIfRefused(
                context = context,
                accountIdB64 = account,
                channelId = channelId,
                refused = PublisherCommitRetry.shared.hold(account, channelId)?.refused == true ||
                    ChannelSubscriptionSync.readerKeyRetry.hold(account, channelId)?.refused == true ||
                    admission.isRefused(channelId),
                client = client,
            ) {
                PublisherCommitRetry.shared.noteSuccess(account, channelId)
                ChannelSubscriptionSync.readerKeyRetry.noteSuccess(account, channelId)
                admission.clearHold(channelId)
            }
        }
        repairOwnerKeyIfRefused()
        // G-50 / ADR-0090 — recorded commits that still owe a step (an
        // unanswered commit route included).
        if (store != null && store.entries(account).isNotEmpty()) {
            val finished = pub.withChannel { ids ->
                CoPublisher.replayPublisherCommits(ds, client, ids, account, store, state.coPublished)
            }
            if (finished.isNotEmpty()) {
                dropGroup(channelId)
                for (id in finished) {
                    val landed = try {
                        client.fetchEpoch(id)
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        null
                    }
                    state.refreshPublisherKey(id, landed)
                    recordOwed.mark(id)
                }
            }
        }
        // 2026-11-20 — every pass, so a mapping broken by commits with no
        // reseal is re-anchored even when no peer commit arrives.
        refreshSeedAfterPeerCommit(pub, pg, channelId)
        if (state.takeReplayedCommit(channelId)) recordOwed.mark(channelId)
        if (recordOwed.isOwed(channelId)) {
            reissueRecordOwed(recordOwed, channelId) { ChannelMetaPublisher.reissue(state, client, channelId) }
        }
        val channel = state.channels.value.firstOrNull { it.id == channelId }
        if (owner && channel != null) {
            ChannelDiscovery.sendOwedRekeyIfAny(context, state, client, account, channel)
            ChannelDiscovery.sendOwedRecordIfAny(context, pub, account, channel)
        }
        var due: Boolean? = null
        if (owner) {
            // A co-publisher's rotate request: any owner device rotates once
            // for every request read before it.
            if (rotatesChannelKeyHere(owner) && rotateRequested.remove(channelId)) due = true
            // The rotation heartbeat (ADR-0090 decision C: every owner device).
            val now = System.currentTimeMillis()
            if (due == null && now - (lastRotationCheckMs[channelId] ?: 0L) >= ROTATION_HEARTBEAT_MS) {
                lastRotationCheckMs[channelId] = now
                if (rotationIsDue(channelId, now)) due = false
            }
            takeOverUnfinishedReseal(pub, channelId, pg, account)
            filePublicSeedIfDue(pub, channelId)
        }
        // Section 15 — the exact count, for the info screen (not every pass).
        if (System.currentTimeMillis() - (lastCountMs[channelId] ?: 0L) >= COUNT_INTERVAL_MS) {
            lastCountMs[channelId] = System.currentTimeMillis()
            try {
                val count = ChannelPublisherSigners.derive(pub, channelId).use { client.subscriberCount(channelId, it) }
                _subscriberCounts.update { it + (channelId to count) }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // The info screen keeps the last count.
            }
        }
        // A refusal recorded during this step is repaired now, not a pass later.
        repairOwnerKeyIfRefused()
        return due
    }

    /**
     * Read the journal (and, with [readKnocks], the knocks) from the cursor.
     * ADR-0090 section 2.5 — reads refused in a row, or a DS that names a
     * newest epoch below this device's, mean the journal was reset under it
     * (a re-founding on another device): its local group goes and it reads,
     * until it is admitted again. Returns `null` then.
     */
    private suspend fun readJournalLocked(
        pub: ChannelPublisherIdentity,
        pg: PublisherGroup,
        channelId: String,
        account: String,
        readKnocks: Boolean,
    ): CoPublisher.JournalPoll? {
        val now0 = System.currentTimeMillis()
        val poll = try {
            pub.withChannel { ids ->
                CoPublisher.pollJournal(
                    ds,
                    ids,
                    pg,
                    onRequest = { joinerAccount, accountId ->
                        ChannelAdmissionSeedShare.rememberRequest(context, account, channelId, joinerAccount, accountId)
                    },
                    after = journalCursor[channelId] ?: 0L,
                    knockAfter = if (readKnocks) knockCursor[channelId] ?: 0L else null,
                )
            }
        } catch (e: me.lakko.app.ds.DsProofRefusedError) {
            val refused = (refusedReads[channelId] ?: 0) + if (e.clockSkewed) 0 else 1
            refusedReads[channelId] = refused
            val own = pub.withChannel { ids -> ids.mls { pg.epoch() } }
            // Only a device with a sibling can have been re-founded under:
            // the account's only device never drops its own group.
            val siblings = (me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.rosterSignatureKeys()?.size ?: 0) > 1
            if (siblings && journalResetUnder(refused, null, own)) {
                becomeReader(pub, channelId)
                return null
            }
            throw e
        }
        refusedReads.remove(channelId)
        lastJournalMs[channelId] = now0
        journalCursor[channelId] = poll.journalCursor
        if (readKnocks) {
            lastKnockMs[channelId] = now0
            poll.knockCursor?.let { knockCursor[channelId] = it }
        }
        val own = pub.withChannel { ids -> ids.mls { pg.epoch() } }
        if (journalResetUnder(0, poll.latestEpoch, own)) {
            becomeReader(pub, channelId)
            return null
        }
        return poll
    }

    /**
     * The channel's PublisherGroup this device holds is no longer active (its
     * own leaf was removed by a commit it applied; walk of 2026-11-14): what
     * [inactivePublisherGroupVerdict] says. `true` when the pass stops here
     * (this device now waits for a Welcome, reads, or ended the channel; or
     * the roster is not read yet and nothing changes this pass).
     */
    private suspend fun settleInactiveGroup(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        owner: Boolean,
    ): Boolean {
        val ownAccountKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        // OpenMLS merges the removing commit's tree before it marks the group
        // inactive: the roster read here is the one after the removal.
        val ownAccountHasLeaf: Boolean? = try {
            pub.withChannel { ids -> ids.mls { pg.members().any { it.accountKey.contentEquals(ownAccountKey) } } }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
        val roster = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.rosterSignatureKeys()
        val verdict = inactivePublisherGroupVerdict(
            ownerDevice = owner,
            ownAccountHasLeaf = ownAccountHasLeaf,
            otherOwnDevices = roster?.let { it.size > 1 },
        )
        when (verdict) {
            InactiveGroupVerdict.UNDECIDED -> return true
            InactiveGroupVerdict.REMOVED -> endHere(channelId)
            // The old group stays until an owner device's Welcome replaces it
            // ([CoPublisher.joinReplacingHeld]); the reader line shows.
            InactiveGroupVerdict.WAIT_FOR_WELCOME -> becomeReader(pub, channelId)
            // Nobody can admit this device: it reads, and the next pass's
            // entry from the saved copy (else decision A) takes the channel
            // ([holdsLiveGroup]: the evicted group does not block either).
            InactiveGroupVerdict.READER -> {
                dropGroup(channelId)
                state.dropPublisherKeys(channelId)
            }
        }
        Log.i(TAG, "ADR-0090: this device's leaf left the channel's publisher group ($verdict)")
        return true
    }

    /**
     * Part B (2026-11-14) — each held PublisherGroup Welcome
     * ([HeldPublisherWelcomes]) is tried through the normal replace rule
     * ([CoPublisher.joinAdmissionWelcome]); kept while the held group is
     * still active and unsuspected, dropped when the rule ran and refused,
     * when it is too old, or when the channel is no longer one the account
     * owns or co-publishes. A join that lands makes this device a publisher
     * again. Runs under the pass lock.
     */
    private suspend fun retryHeldWelcomesLocked() {
        val account = state.boundAccount ?: return
        val pub = state.publisher ?: return
        if (!pub.hasDevice()) return
        val channels = HeldPublisherWelcomes.channels(context, account)
        if (channels.isEmpty()) return
        val registers = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.channelSubscriptions?.value ?: return
        val targets = publisherWelcomeTargets(registers.map { viewOf(it) }, state.channels.value.map { it.id }.toSet())
        val coPublished = state.coPublished
        for (channel in channels) {
            val target = targets.firstOrNull { it.channelIdHex.equals(channel, ignoreCase = true) }
            if (target == null) {
                HeldPublisherWelcomes.clear(context, account, channel)
                continue
            }
            val channelId = state.channels.value.firstOrNull { it.id.equals(channel, ignoreCase = true) }?.id ?: continue
            val owned = target.role == ChannelAccountRole.OWNER
            val joinedGroupId = if (owned) null else target.publisherGroupId.takeIf { it.size == 32 }
            for (held in HeldPublisherWelcomes.heldFor(context, account, channelId)) {
                val welcome = try {
                    java.util.Base64.getDecoder().decode(held.welcomeB64)
                } catch (_: IllegalArgumentException) {
                    HeldPublisherWelcomes.drop(context, account, channelId, held.welcomeB64)
                    continue
                }
                // The removal is not applied yet: nothing is tried (no handle
                // closed, no journal re-read), only the age bound applies.
                if (pub.withChannel { ids -> CoPublisher.heldGroupWaitsForRemoval(ids, channelId, joinedGroupId) }) {
                    if (heldWelcomeStep(false, true, held.heldAtMs, System.currentTimeMillis()) == HeldWelcomeStep.DROP) {
                        HeldPublisherWelcomes.drop(context, account, channelId, held.welcomeB64)
                    }
                    continue
                }
                dropGroup(channelId)
                val keys = try {
                    pub.withChannel { ids ->
                        CoPublisher.joinAdmissionWelcome(
                            ids,
                            welcome,
                            channelId,
                            owned = owned,
                            joinedGroupId = joinedGroupId,
                            ownerAccountKey = if (owned) {
                                null
                            } else {
                                coPublisherWelcomeOwnerKey(target.ownerAccountKey, coPublished?.ownerKeyOf(channelId))
                            },
                            coPublished = coPublished,
                            existingSigningSeed = state.persistedPublisherSigningSeed(channelId),
                        )
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    null
                }
                when (heldWelcomeStep(keys != null, stillWaiting = false, held.heldAtMs, System.currentTimeMillis())) {
                    HeldWelcomeStep.JOINED_CLEAR_CHANNEL -> {
                        HeldPublisherWelcomes.clear(context, account, channelId)
                        keys?.let { state.setKeys(channelId, it) }
                        Log.i(TAG, "ADR-0090: a held Welcome admitted this device to post again")
                        break
                    }
                    HeldWelcomeStep.KEEP -> Unit
                    HeldWelcomeStep.DROP -> HeldPublisherWelcomes.drop(context, account, channelId, held.welcomeB64)
                }
            }
        }
    }

    /** ADR-0090 section 2.5 — the journal was reset under this device: drop
     *  its PublisherGroup state and its composer; it reads until another
     *  device of the account admits it again (the waiting line shows). */
    private suspend fun becomeReader(pub: ChannelPublisherIdentity, channelId: String) {
        // Security pass — a refused read and a DS epoch are not
        // authenticated: the group state is KEPT. This device only stops
        // posting and rotating under the old seed and reads (the waiting
        // line) until an owner-signed Welcome for the same group replaces it
        // ([CoPublisher.joinReplacingHeld]).
        Log.i(TAG, "ADR-0090: the publisher journal may have been reset under this device; it reads until admitted again")
        dropGroup(channelId)
        refusedReads.remove(channelId)
        val account = state.boundAccount ?: return
        ChannelSuspensions.suspend(context, account, channelId)
        state.dropPublisherKeys(channelId)
    }

    /** Matching signed reads in a row per suspended channel. */
    private val suspensionOk = HashMap<String, Int>()

    /**
     * Security pass follow-up — each suspended channel whose group this
     * device still holds is probed with one signed read under the held
     * group's epoch key: [SUSPENSION_LIFT_READS] successes in a row with the
     * DS naming the held group's epoch lift the suspension and reopen the
     * composer keys; anything else keeps it (the owner-signed Welcome path
     * stays the other way out).
     */
    private suspend fun probeSuspendedLocked() {
        val account = state.boundAccount ?: return
        val pub = state.publisher ?: return
        if (!pub.hasDevice()) return
        for (channelId in ChannelSuspensions.all(context, account)) {
            if (state.channels.value.none { it.id.equals(channelId, ignoreCase = true) }) continue
            val id = state.channels.value.first { it.id.equals(channelId, ignoreCase = true) }.id
            val pg = try {
                pub.withChannel { ids -> CoPublisher.loadPublisherGroup(ids, id, state.coPublished) }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                null
            } ?: continue
            try {
                val groupEpoch = pub.withChannel { ids -> ids.mls { pg.epoch() } }
                val read = try {
                    val signer = pub.withChannel { ids -> CoPublisher.signerOf(pg, ids) }
                    signer?.let { ds.fetchGroup(pg.groupId(), after = 0L, limit = 1, waitSecs = 0, signer = it, reclaimOnRefusal = false) }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    null
                }
                val probe = suspensionProbe(suspensionOk[id] ?: 0, read != null, read?.latestEpoch, groupEpoch)
                suspensionOk[id] = probe.consecutiveOk
                if (!probe.lift) continue
                suspensionOk.remove(id)
                ChannelSuspensions.clear(context, account, id)
                val keys = pub.withChannel { ids ->
                    ChannelsComposer.freshPublisherKeys(ids, id, state.persistedPublisherSigningSeed(id), joinedGroupId = state.joinedGroupIdOf(id))
                }
                state.setKeys(id, keys)
                Log.i(TAG, "ADR-0090: a suspected reset was a false alarm; this device posts again")
            } finally {
                pg.close()
            }
        }
    }

    /**
     * ADR-0090 — a channel the account owns or co-publishes (the subscription
     * register) whose PublisherGroup this device now holds but whose keys it
     * has not: open its keys. Covers a relaunch that ran before this device's
     * bound identity was ready, and an admission whose Welcome was joined.
     */
    private suspend fun adoptHeldGroupsLocked() {
        val pub = state.publisher ?: return
        if (!pub.hasDevice()) return
        val registers = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.channelSubscriptions?.value ?: return
        for (r in registers) {
            if (!r.subscribed || r.role == uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.SUBSCRIBER) continue
            val channelId = ChannelsComposer.bytesToHex(r.channelId)
            if (state.keysByChannel.value.containsKey(channelId) || channelId in state.orphaned.value) continue
            // A suspected reset keeps the group but posts nothing under it
            // until an owner-signed Welcome replaces it.
            if (ChannelSuspensions.isSuspended(context, state.boundAccount ?: return, channelId)) continue
            val joined = if (r.role == uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.CO_PUBLISHER) {
                state.joinedGroupIdOf(channelId) ?: r.publisherGroupId.takeIf { it.size == 32 } ?: continue
            } else {
                null
            }
            val keys = try {
                pub.withChannel { ids ->
                    ChannelsComposer.freshPublisherKeys(
                        ids,
                        channelId,
                        state.persistedPublisherSigningSeed(channelId),
                        joinedGroupId = joined,
                    )
                }
            } catch (_: ChannelsComposerException.NotPublishedHere) {
                continue
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "a held publisher group was not opened: ${e.javaClass.simpleName}")
                continue
            }
            if (state.channels.value.none { it.id == channelId }) continue
            state.setKeys(channelId, keys)
            Log.i(TAG, "ADR-0090: this device posts in a channel it was admitted to")
        }
    }

    /** The signature keys of the owner account's leaves in [pg]. */
    private suspend fun ownAccountLeaves(pub: ChannelPublisherIdentity, pg: PublisherGroup): List<ByteArray> =
        pub.withChannel { ids ->
            ids.mls {
                val owner = pg.ownerAccountKey()
                pg.members().filter { it.accountKey.contentEquals(owner) }.map { it.signaturePublicKey }
            }
        }

    /** ADR-0064 section 7 — an owner device that applied another owner
     *  device's commit writes the channel's head with the current epoch.
     *  Best effort. */
    private suspend fun writeHeadAfterPoll(
        pub: ChannelPublisherIdentity,
        pg: PublisherGroup,
        owner: Boolean,
        poll: CoPublisher.JournalPoll,
    ) {
        if (!owner || !poll.epochMoved) return
        val staged = pub.withChannel { ids -> ids.mls { pg.hasStagedCommit() } }
        if (!headWriteAfterPoll(owner, poll.epochMoved, staged)) return
        pub.withChannel { ids -> ChannelVaultHeads.writeCurrent(ids, pg) }
    }

    /** When an entry from the vault head was last tried, per channel, and
     *  whether that try left it unsettled (no answer). */
    private val lastEntryMs = HashMap<String, Long>()
    private val entryUnsettled = HashSet<String>()

    /**
     * ADR-0064 section 7 (amendment of 2026-11-14) — every channel the
     * account OWNS (the subscription register) whose PublisherGroup this
     * device does not hold: a recorded entry still owed is resumed; else the
     * channel's vault head is read and the saved epochs tried newest first
     * ([CoPublisher.enterFromSaved]); entered, this device posts. No head, or
     * every saved epoch not taken: it waits as before (decision A when it
     * applies). Returns the channels whose entry is unsettled (owed, or the
     * head not reached), which decision A must leave alone.
     */
    private suspend fun enterFromVaultLocked(): Set<String> {
        val account = state.boundAccount ?: return emptySet()
        val pub = state.publisher ?: return emptySet()
        if (!pub.hasDevice()) return emptySet()
        val store = state.commitStore ?: return emptySet()
        val registers = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.channelSubscriptions?.value
            ?: return emptySet()
        val unsettled = HashSet<String>()
        for (r in registers) {
            if (r.role != uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.OWNER) continue
            val channelId = ChannelsComposer.bytesToHex(r.channelId)
            if (state.channels.value.none { it.id == channelId }) continue
            if (state.keysByChannel.value.containsKey(channelId)) continue
            // A recorded entry whose verdict the DS has not given: send again.
            if (store.entries(account).any { it.kind == PublisherCommitEntry.Kind.ENTER && it.channelIdHex == channelId.lowercase() }) {
                unsettled += channelId
                val finished = pub.withChannel { ids ->
                    CoPublisher.replayPublisherCommits(ds, client, ids, account, store, state.coPublished, onlyChannel = channelId)
                }
                if (channelId in finished) postFromEntered(pub, channelId)
                continue
            }
            // A held group whose leaf was removed (inactive) is not held: it
            // can never read or commit again, and it is deleted before the
            // entry below (the core refuses an entry over a group it holds).
            val heldActive = pub.withChannel { ids ->
                CoPublisher.loadPublisherGroup(ids, channelId, state.coPublished)?.let { g ->
                    try {
                        ids.mls { g.isActive() }
                    } finally {
                        g.close()
                    }
                }
            }
            val wanted = entryWanted(
                ownerRole = true,
                subscribed = r.subscribed,
                holdsKeys = false,
                holdsGroup = holdsLiveGroup(heldActive != null, heldActive == true),
                suspended = ChannelSuspensions.isSuspended(context, account, channelId),
                orphaned = channelId in state.orphaned.value,
            )
            if (!wanted) continue
            val now = System.currentTimeMillis()
            if (now - (lastEntryMs[channelId] ?: 0L) < ENTRY_RETRY_MS) {
                if (channelId in entryUnsettled) unsettled += channelId
                continue
            }
            lastEntryMs[channelId] = now
            val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
            if (heldActive == false) {
                dropGroup(channelId)
                pub.withChannel { ids -> CoPublisher.deleteLocalPublisherGroup(ids, channelId, joinedGroupId = null) }
            }
            val entered = try {
                val saved = pub.withChannel { ids -> ChannelVaultHeads.readReentry(ids, channelBytes) }
                try {
                    enterNewestFirst(saved) { r0 ->
                        pub.withChannel { ids -> CoPublisher.enterFromSaved(ds, client, ids, channelId, r0, account, store) }
                    }
                } finally {
                    saved.forEach { it.dsAccessSeed.fill(0) }
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                entryUnsettled += channelId
                unsettled += channelId
                Log.w(TAG, "ADR-0064: an owned channel was not entered yet: ${e.javaClass.simpleName}")
                continue
            }
            entryUnsettled -= channelId
            if (entered) {
                Log.i(TAG, "ADR-0064: this device entered an owned channel from its saved state")
                postFromEntered(pub, channelId)
            }
        }
        return unsettled
    }

    /** The entry landed: this device posts in the channel (the composer
     *  appears, nothing else is shown). */
    private suspend fun postFromEntered(pub: ChannelPublisherIdentity, channelId: String) {
        dropGroup(channelId)
        val keys = pub.withChannel { ids ->
            ChannelsComposer.freshPublisherKeys(ids, channelId, state.persistedPublisherSigningSeed(channelId))
        }
        state.setKeys(channelId, keys)
        afterRotation(channelId)
    }

    /**
     * ADR-0090 decision A — this device founded the account's DeviceGroup
     * alone: found again the PublisherGroup of every channel the register says
     * the account owns, reset its journal under the same root, and reseal the
     * new seed to every subscriber whose ticket it can open (the admission
     * seeds come from the account's register). Co-publishers are invited
     * again by the person.
     */
    private suspend fun refoundIfOwedLocked(unsettled: Set<String>) {
        val account = state.boundAccount ?: return
        if (!ChannelRefounding.isOwed(context, account)) return
        val pub = state.publisher ?: return
        if (!pub.hasDevice()) return
        val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active ?: return
        // Only while the account's DeviceGroup is this device alone: a device
        // with a sibling never founds again (it waits to be admitted); the
        // record stays for the next pass.
        val roster = sync.rosterSignatureKeys() ?: return
        val self = pub.withChannel { ids -> ids.device.signaturePublicKey() }
        if (!refoundAllowed(roster, self)) return
        val registers = sync.channelSubscriptions.value ?: return
        var left = false
        var seen = false
        for (r in registers) {
            if (!r.subscribed || r.role != uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.OWNER) continue
            val channelId = ChannelsComposer.bytesToHex(r.channelId)
            seen = true
            if (ChannelRefounding.isDone(context, account, channelId)) continue
            // ADR-0064 section 7 — an entry from the head is still owed or its
            // head could not be read: never founded again under it.
            if (channelId in unsettled) {
                left = true
                continue
            }
            if (state.channels.value.none { it.id == channelId }) {
                left = true
                continue
            }
            try {
                val refounded = refoundLocked(pub, channelId)
                ChannelRefounding.markDone(context, account, channelId)
                if (refounded) Log.i(TAG, "ADR-0090: an owned channel was founded again on this device")
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                left = true
                Log.w(TAG, "an owned channel was not founded again yet: ${e.javaClass.simpleName}")
            }
        }
        // Finished only once the register named an owned channel and every
        // one was handled: a register not restored yet keeps it owed.
        if (seen && !left) ChannelRefounding.finish(context, account)
    }

    private suspend fun refoundLocked(pub: ChannelPublisherIdentity, channelId: String): Boolean {
        // A device that still holds the group publishes it already: nothing
        // to found again (the journal is not reset under anyone).
        // An inactive one (its leaf was removed) is founded again over: the
        // re-founding deletes it first ([holdsLiveGroup]).
        val heldActive = pub.withChannel { ids ->
            CoPublisher.loadPublisherGroup(ids, channelId, state.coPublished)?.let { g ->
                try {
                    ids.mls { g.isActive() }
                } finally {
                    g.close()
                }
            }
        }
        if (holdsLiveGroup(heldActive != null, heldActive == true)) return false
        dropGroup(channelId)
        pub.withChannel { ids -> CoPublisher.foundAndClaimAtCreation(ds, ids, channelId, refound = true) }
        val keys = pub.withChannel { ids ->
            ChannelsComposer.freshPublisherKeys(ids, channelId, state.persistedPublisherSigningSeed(channelId))
        }
        state.setKeys(channelId, keys)
        // The commit records of the old chain will never land.
        val account = state.boundAccount ?: return true
        state.commitStore?.let { store ->
            store.entries(account).filter { it.channelIdHex == channelId.lowercase() }.forEach { store.remove(account, it.id) }
        }
        knowEpochBeforeCommit(channelId)
        val pg = groupFor(pub, channelId)
        val target = CoPublisher.nextServerEpoch(client, channelId)
        // Decision A — the new group starts at MLS epoch 0 while the service
        // moves to [target]: from now on the channel epoch is the group's
        // plus this offset (every later commit moves both by one).
        val groupEpoch = pub.withChannel { ids -> ids.mls { pg.epoch() } }
        ChannelEpochOffsets.set(channelId, target - groupEpoch.toLong())
        // Security pass — every subscriber whose ticket this device can open
        // (a private channel's); a private channel with no admission seed
        // here reseals only to the account's own reader key and leaves the
        // other rows to the admission pass; only a channel that was never
        // private reseals to every row.
        val admitted = ChannelSubscriberRows.admissible(context, state, client, channelId)
        val public = state.channels.value.firstOrNull { it.id == channelId }?.sharedListed == true
        val own = ChannelReseal.accountKeysFor(account, channelId)
        pub.withChannel { ids ->
            when (val plan = refoundResealPlan(admitted, public, own)) {
                is RefoundReseal.EveryRow -> resealToSubscribers(client, ids, pg, channelId, target, null, own)
                is RefoundReseal.Keys -> resealToKeys(client, ids, pg, channelId, target, plan.keys)
            }
        }
        afterRotation(channelId)
        return true
    }

    /** ADR-0090 section 2.1 — an owner device admits the account's devices
     *  with no leaf and retires the leaves whose device left, in one commit. */
    private suspend fun reconcileOwnDevices(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        account: String,
        ownerKey: ByteArray,
    ) {
        val roster = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.rosterSignatureKeys() ?: return
        if (roster.isEmpty()) return
        val store = state.commitStore ?: return
        val plan = pub.withChannel { ids ->
            val members = ids.mls { pg.members() }
            planDeviceReconcile(
                members.filter { it.accountKey.contentEquals(ownerKey) }.map { it.signaturePublicKey },
                roster,
                ids.device.signaturePublicKey(),
            )
        }
        if (plan.isEmpty) return
        val admit = if (plan.admit.isEmpty()) {
            emptyList()
        } else {
            val published = fetchOwnKeyPackages(account) ?: return
            plan.admit.mapNotNull { key ->
                keyPackageToAdmit(key, ownerKey, published, { uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(it) }) {
                    uniffi.lakko_ffi_uniffi.keyPackageAccountKey(it)
                }
            }
        }
        if (admit.isEmpty() && plan.retire.isEmpty()) return
        knowEpochBeforeCommit(channelId)
        val outcome = try {
            pub.withChannel { ids ->
                CoPublisher.admitDevices(ds, client, ids, pg, admit, plan.retire, ownerKey, channelId, account, store)
            }
        } catch (e: Throwable) {
            dropGroup(channelId)
            throw e
        }
        Log.i(TAG, "ADR-0090: own devices admitted ${admit.size}, retired ${plan.retire.size}: $outcome")
        if (outcome == PublisherCommitOutcome.LANDED) afterRotation(channelId) else lastJournalMs.remove(channelId)
    }

    /** This account's devices with a published KeyPackage (device key,
     *  KeyPackage), or `null` when the directory could not be read. */
    private suspend fun fetchOwnKeyPackages(account: String): List<Pair<ByteArray, ByteArray>>? {
        val accountId = try {
            java.util.Base64.getDecoder().decode(account)
        } catch (_: IllegalArgumentException) {
            return null
        }
        return try {
            me.lakko.app.discovery.DiscoveryClient().use { d ->
                d.setSessionToken(me.lakko.app.auth.SessionTokens.provider?.current())
                d.fetchKeyPackages(accountId)?.map { it.deviceId to it.keyPackage } ?: emptyList()
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * ADR-0090 section 2.2 — a co-publisher's publishing device asks an owner
     * device to admit its account's other devices (with committer
     * delegations into its own mailbox) and to retire leaves whose device
     * left. It never commits. Sent again every [DEVICE_REQUEST_RESEND_MS]
     * while not acted on.
     */
    private suspend fun requestOwnDevices(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        ownerKey: ByteArray,
    ) {
        val account = state.boundAccount ?: return
        val roster = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.rosterSignatureKeys() ?: return
        val ownKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        val plan = pub.withChannel { ids ->
            val members = ids.mls { pg.members() }
            planDeviceReconcile(
                members.filter { it.accountKey.contentEquals(ownKey) }.map { it.signaturePublicKey },
                roster,
                ids.device.signaturePublicKey(),
            )
        }
        if (plan.isEmpty) return
        val now = System.currentTimeMillis()
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        fun dueFor(key: ByteArray): Boolean =
            now - (deviceRequestSentMs["$channelId|${ChannelsComposer.bytesToHex(key)}"] ?: 0L) >= DEVICE_REQUEST_RESEND_MS
        val envelopes = ArrayList<Pair<ByteArray, ByteArray>>()
        val toAdmit = plan.admit.filter(::dueFor)
        if (toAdmit.isNotEmpty()) {
            val published = fetchOwnKeyPackages(account) ?: return
            val reach = me.lakko.app.reach.ReachManager.peek()
            val binding = reach?.ownMailboxBinding()
            if (binding != null) {
                val delegations = try {
                    reach.composeOwnMailboxDelegations(listOf(ownerKey))
                } catch (_: Throwable) {
                    emptyList()
                }
                for (key in toAdmit) {
                    val kp = keyPackageToAdmit(key, ownKey, published, { uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(it) }) {
                        uniffi.lakko_ffi_uniffi.keyPackageAccountKey(it)
                    } ?: continue
                    envelopes += key to uniffi.lakko_ffi_uniffi.composePublisherDeviceAdmit(channelBytes, kp, binding, delegations)
                }
            }
        }
        for (key in plan.retire.filter(::dueFor)) {
            envelopes += key to uniffi.lakko_ffi_uniffi.composePublisherDeviceRetire(channelBytes, key)
        }
        for ((key, envelope) in envelopes) {
            try {
                pub.withChannel { ids -> CoPublisher.publishApplication(ds, ids, pg, envelope) }
                deviceRequestSentMs["$channelId|${ChannelsComposer.bytesToHex(key)}"] = now
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "a device request was not sent yet: ${e.javaClass.simpleName}")
            }
        }
    }

    /** ADR-0090 section 2.2 / decision B — an owner device acts on a
     *  publisher's device request: admit its device (the Welcome to its
     *  mailbox under the delegation it carried) or retire its leaf. */
    private suspend fun applyDeviceRequest(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        account: String,
        ownerKey: ByteArray,
        publisherAccounts: List<ByteArray>,
        r: DeviceRequest,
    ) {
        val store = state.commitStore ?: return
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val action = r.decoded.action
        val members = pub.withChannel { ids -> ids.mls { pg.members() } }
        val verdict = when (action) {
            is uniffi.lakko_ffi_uniffi.FfiPublisherDeviceAction.Admit -> {
                val kpAccount = runCatching { uniffi.lakko_ffi_uniffi.keyPackageAccountKey(action.keyPackage) }.getOrNull()
                val leaf = runCatching { uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(action.keyPackage) }.getOrNull()
                // Already a leaf: nothing to do.
                if (leaf != null && members.any { it.signaturePublicKey.contentEquals(leaf) }) return
                deviceRequestVerdict(true, r.decoded.channelId, channelBytes, r.sender, ownerKey, publisherAccounts, true, kpAccount, null)
            }
            is uniffi.lakko_ffi_uniffi.FfiPublisherDeviceAction.Retire -> {
                val leafAccount = members.firstOrNull { it.signaturePublicKey.contentEquals(action.deviceKey) }?.accountKey
                deviceRequestVerdict(true, r.decoded.channelId, channelBytes, r.sender, ownerKey, publisherAccounts, false, null, leafAccount)
            }
        }
        if (verdict == DeviceRequestVerdict.IGNORE) return
        var header: String? = null
        var binding: ByteArray? = null
        if (action is uniffi.lakko_ffi_uniffi.FfiPublisherDeviceAction.Admit) {
            binding = action.mailboxBinding
            val env = try {
                uniffi.lakko_ffi_uniffi.publisherDeviceRequestDelegations(r.plaintext, r.sender)
            } catch (_: Throwable) {
                null
            }
            if (env != null) {
                header = try {
                    uniffi.lakko_ffi_uniffi.checkCommitterDelegations(env)
                    uniffi.lakko_ffi_uniffi.committerDelegationFor(env, ownerKey)
                } catch (_: Throwable) {
                    null
                }
                binding = env.addedMailboxBinding
            }
        }
        knowEpochBeforeCommit(channelId)
        val outcome = try {
            pub.withChannel { ids ->
                when (action) {
                    is uniffi.lakko_ffi_uniffi.FfiPublisherDeviceAction.Admit -> CoPublisher.admitDevices(
                        ds, client, ids, pg, listOf(action.keyPackage), emptyList(), r.sender, channelId, account, store,
                        delegationHeader = header,
                        delegationBinding = binding,
                    )
                    is uniffi.lakko_ffi_uniffi.FfiPublisherDeviceAction.Retire -> CoPublisher.admitDevices(
                        ds, client, ids, pg, emptyList(), listOf(action.deviceKey), null, channelId, account, store,
                    )
                }
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            dropGroup(channelId)
            Log.w(TAG, "a publisher's device request was not applied yet: ${e.javaClass.simpleName}")
            return
        }
        if (outcome == PublisherCommitOutcome.LANDED) afterRotation(channelId) else lastJournalMs.remove(channelId)
    }

    /** Section 6 — send this device's admission mark when it advanced past
     *  what it last sent. */
    private suspend fun sendAdmissionMarkIfAdvanced(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        account: String,
    ) {
        val mark = ChannelAdmissionMark.get(context, account, channelId)
        if (mark <= 0L || mark <= (markSent[channelId] ?: 0L)) return
        try {
            val envelope = uniffi.lakko_ffi_uniffi.composeChannelAdmissionMarkEnvelope(
                ChannelsComposer.channelIdFromHex(channelId),
                mark.toULong(),
            )
            pub.withChannel { ids -> CoPublisher.publishApplication(ds, ids, pg, envelope) }
            markSent[channelId] = mark
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "the admission mark was not sent yet: ${e.javaClass.simpleName}")
        }
    }

    /**
     * ADR-0090 section 4 — the rotation policy with inputs every owner device
     * sees: the clock starts at the DS row time of the commit that began the
     * epoch (this device's own rotation, or a peer commit it applied), and
     * the post count is the posts listed under the current channel epoch.
     */
    private fun rotationIsDue(channelId: String, nowMs: Long): Boolean {
        val stored = state.rotationState(channelId)
            ?: StoredRotationState(nowMs.toULong(), 0u).also { state.saveRotationState(channelId, it) }
        val epoch = state.epochByChannel.value[channelId]
        val counted = if (epoch == null) {
            stored.postsSinceRotation
        } else {
            val epochs = state.postsByChannel.value[channelId].orEmpty().map { post ->
                try {
                    val wire = java.util.Base64.getUrlDecoder().decode(post.payloadB64)
                    uniffi.lakko_ffi_uniffi.decodeChannelPost(wire).use { it.epochId() }
                } catch (_: Throwable) {
                    null
                }
            }
            postsUnderEpoch(epochs, epoch, stored.postsSinceRotation)
        }
        return try {
            uniffi.lakko_ffi_uniffi.channelShouldRotate(
                channelRotationPolicyDefault(),
                ChannelRotationState(lastRotationMs = stored.lastRotationMs, postsSinceRotation = counted),
                nowMs.toULong(),
            )
        } catch (_: Throwable) {
            false
        }
    }

    /** A due rotation, after the random wait: re-read the journal head and
     *  apply it, re-evaluate (a requested one runs unless another device
     *  rotated meanwhile), then stage. */
    private suspend fun rotateDueLocked(channelId: String, forced: Boolean) {
        val pub = state.publisher ?: return
        val account = state.boundAccount ?: return
        if (!isOwner(channelId)) return
        val pg = groupFor(pub, channelId)
        val before = pub.withChannel { ids -> ids.mls { pg.epoch() } }
        val ownLeavesBefore = ownAccountLeaves(pub, pg)
        lastJournalMs.remove(channelId)
        val poll = readJournalLocked(pub, pg, channelId, account, readKnocks = false) ?: return
        if (poll.applied > 0) {
            poll.lastCommitAtMs?.let { at -> state.saveRotationState(channelId, StoredRotationState(at.toULong(), 0u)) }
            refreshSeedAfterPeerCommit(pub, pg, channelId)
        }
        writeHeadAfterPoll(pub, pg, true, poll)
        val after = pub.withChannel { ids -> ids.mls { pg.epoch() } }
        if (after != before) {
            Log.i(TAG, "ADR-0090: another owner device committed first; this rotation is not needed")
            // Security pass — an entry applied here is reconciled at once.
            if (poll.epochMoved && ownLeafAdded(ownLeavesBefore, ownAccountLeaves(pub, pg))) {
                reconcileOwnDevices(pub, channelId, pg, account, pub.withChannel { ids -> ids.mls { pg.ownerAccountKey() } })
            }
            return
        }
        if (!forced && !rotationIsDue(channelId, System.currentTimeMillis())) return
        // 2026-11-20 — no new rotation while a recorded commit of the channel
        // still owes its reseal (seen live on iOS: one more epoch merged every
        // pass, each reseal refused `401`). That reseal, when it lands, seals
        // a seed no subscriber holds yet to the subscribers listed then, so a
        // forced rotation (a removed subscriber) is covered by it too.
        if (PublisherCommitRetry.hasOwedCommit(channelId, state.commitStore?.entries(account).orEmpty())) {
            Log.i(TAG, "ADR-0090: a recorded commit still owes its reseal; no rotation staged")
            return
        }
        knowEpochBeforeCommit(channelId)
        val stored = state.rotationState(channelId)
        val rotationState = if (forced || stored == null) {
            ChannelRotationState(0uL, 0u)
        } else {
            ChannelRotationState(lastRotationMs = 0uL, postsSinceRotation = stored.postsSinceRotation)
        }
        val result = try {
            rotate(pub, pg, channelId, rotationState)
        } catch (e: Throwable) {
            dropGroup(channelId)
            throw e
        }
        if (result.lost) {
            lastJournalMs.remove(channelId)
            Log.i(TAG, "ADR-0090: rotation lost to another owner device (409)")
            return
        }
        state.saveRotationState(
            channelId,
            StoredRotationState(result.newState.lastRotationMs, result.newState.postsSinceRotation),
        )
        lastRotationCheckMs[channelId] = System.currentTimeMillis()
        if (result.rotated) afterRotation(channelId)
    }

    /**
     * ADR-0090 section 4 — the unfinished-reseal takeover: when the channel
     * service has not taken the group's epoch for [UNFINISHED_RESEAL_WAIT_MS]
     * and this device has no commit of its own running for it, this owner
     * device reseals the current seed itself (a second reseal is refused by
     * the service at `current + 1`, ADR-0030).
     */
    private suspend fun takeOverUnfinishedReseal(
        pub: ChannelPublisherIdentity,
        channelId: String,
        pg: PublisherGroup,
        account: String,
    ) {
        val groupEpoch = pub.withChannel { ids -> ids.mls { if (pg.hasStagedCommit()) null else pg.epoch() } }
            ?.let { ChannelEpochOffsets.channelEpochOf(channelId, it)?.toLong() } ?: return
        val service = try {
            client.fetchEpoch(channelId).toLong()
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            return
        }
        if (service >= groupEpoch) {
            trailingSince.remove(channelId)
            return
        }
        val now = System.currentTimeMillis()
        val since = trailingSince.getOrPut(channelId) { now }
        val pending = state.commitStore?.entries(account)?.any { it.channelIdHex == channelId.lowercase() } == true
        if (!resealTakeoverDue(groupEpoch, service, since, now, pending)) return
        // 2026-11-20 — a reseal of this channel refused or failed waits out
        // its hold instead of being tried again every pass.
        if (!PublisherCommitRetry.shared.mayAttempt(account, channelId)) return
        try {
            pub.withChannel { ids ->
                resealToSubscribers(
                    client,
                    ids,
                    pg,
                    channelId,
                    service + 1,
                    ChannelReseal.throughFor(account, channelId),
                    ChannelReseal.accountKeysFor(account, channelId),
                )
            }
            trailingSince.remove(channelId)
            PublisherCommitRetry.shared.noteSuccess(account, channelId)
            Log.i(TAG, "ADR-0090: an unfinished reseal was taken over")
            afterRotation(channelId)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            val hold = PublisherCommitRetry.shared.noteFailure(account, channelId, e)
            Log.w(
                TAG,
                "an unfinished reseal was not taken over (${PublisherCommitFailure.logLabel(e)}); " +
                    if (hold.refused) "not tried again until the next start" else "tried again after a wait",
            )
        }
    }

    private suspend fun rotate(
        pub: ChannelPublisherIdentity,
        pg: PublisherGroup,
        channelId: String,
        rotationState: ChannelRotationState,
    ): RotationCron.OnceResult {
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val store = state.commitStore ?: throw IllegalStateException("no commit store")
        return pub.withChannel { ids ->
            RotationCron(
                publisherGroup = pg,
                ids = ids,
                channelIdHex = channelId,
                channelsClient = client,
                saveState = { next ->
                    state.saveRotationState(channelId, StoredRotationState(next.lastRotationMs, next.postsSinceRotation))
                },
                accountIdB64 = account,
                store = store,
            ).runOnce(
                policy = channelRotationPolicyDefault(),
                state = rotationState,
                nowMs = System.currentTimeMillis().toULong(),
            )
        }
    }

    /** The epoch the current keys serve, read before a commit replaces them,
     *  so [ChannelsUiState.refreshPublisherKey] can keep the outgoing seed
     *  under it (section 13: the publisher still reads its older posts). */
    private suspend fun knowEpochBeforeCommit(channelId: String) {
        if (state.epochByChannel.value[channelId] != null) return
        try {
            state.setEpoch(channelId, client.fetchEpoch(channelId))
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            // Unknown: the outgoing seed is then not kept.
        }
    }

    /** A commit of this owner device landed, its reseal included: the
     *  composer's key under the epoch the reseal landed at, that epoch
     *  current, the record re-issued under the new key (verbatim, owed until
     *  it lands), the public seed of the new epoch. */
    private suspend fun afterRotation(channelId: String) {
        val landed = try {
            client.fetchEpoch(channelId)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
        state.refreshPublisherKey(channelId, landed)
        // ADR-0090 section 4 — any commit that changes the seed restarts the
        // rotation clock.
        state.saveRotationState(channelId, StoredRotationState(System.currentTimeMillis().toULong(), 0u))
        reissueRecordOwed(recordOwed, channelId) { ChannelMetaPublisher.reissue(state, client, channelId) }
        state.publisher?.let { if (isOwner(channelId)) filePublicSeedIfDue(it, channelId) }
    }

    /**
     * Section 6 — a PUBLIC channel publishes one seed envelope per epoch,
     * sealed under its read key, so a new subscriber reads at once. An owner
     * device files the current epoch's; the identical envelope again is a
     * success.
     */
    private suspend fun filePublicSeedIfDue(pub: ChannelPublisherIdentity, channelId: String) {
        val channel = state.channels.value.firstOrNull { it.id == channelId } ?: return
        if (!channel.sharedListed) return
        if (!state.keysByChannel.value.containsKey(channelId)) return
        val current = state.epochByChannel.value[channelId]
            ?: client.fetchEpoch(channelId).also { state.setEpoch(channelId, it) }
        // Section 15 — only an epoch the service has reached, with its seed.
        val sealing = state.sealingSeed(channelId, current) ?: return
        val epoch = sealing.epoch
        if (publicSeedFiled[channelId] == epoch) return
        val envelope = pub.withIdentity(persist = false) { id ->
            val readKey = ChannelDiscovery.readKey(id, channelId)
            try {
                uniffi.lakko_ffi_uniffi.sealPublicSeedEnvelope(
                    readKey,
                    ChannelsComposer.channelIdFromHex(channelId),
                    epoch,
                    sealing.seed,
                )
            } finally {
                readKey.fill(0)
            }
        }
        ChannelPublisherSigners.derive(pub, channelId).use { client.putPublicSeed(channelId, epoch, envelope, it) }
        publicSeedFiled[channelId] = epoch
    }

    /** Owner device: approve a co-publisher request (STAGED: the DS decides;
     *  taken, the joiner is added, the new key resealed to every subscriber
     *  and its key appointed). Throws [CoPublisher.PublisherCommitLost] when
     *  another owner device committed first. */
    public suspend fun approve(channelId: String, joinerKey: ByteArray) {
        mutex.withLock { approveLocked(channelId, joinerKey) }
    }

    private suspend fun approveLocked(channelId: String, joinerKey: ByteArray) {
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val store = state.commitStore ?: throw IllegalStateException("no commit store")
        val pg = groupFor(pub, channelId)
        knowEpochBeforeCommit(channelId)
        val joinerAccount = CoPublisher.joinerAccountOf(joinerKey)
        try {
            pub.withChannel { ids ->
                CoPublisher.approve(ds, client, ids, pg, joinerKey, channelId, account, store)
            }
        } catch (e: Throwable) {
            dropGroup(channelId)
            if (e is CoPublisher.PublisherCommitLost) lastJournalMs.remove(channelId)
            throw e
        }
        joinerAccount?.let { ChannelAdmissionSeedShare.notePublisher(context, account, channelId, it) }
        afterRotation(channelId)
        val waiting = pub.withChannel { ids -> CoPublisher.pending(ids, groupFor(pub, channelId)) }
        _pending.update { it + (channelId to waiting) }
    }

    /**
     * Section 15 — an owner device approves without asking a request that
     * came by a live publisher link of this account (`Add publisher` named
     * that person), then revokes the used link through the register, which
     * deletes its ledger row. Any other request waits for the owner.
     */
    private suspend fun approveNamed(pub: ChannelPublisherIdentity, channelId: String, pg: PublisherGroup) {
        val records = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.linkRecords?.value ?: return
        if (records.none { it.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.PUBLISHER && !it.revoked }) return
        val groupId = runCatching { pg.groupId() }.getOrNull() ?: return
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val pending = try {
            pub.withChannel { ids -> CoPublisher.pending(ids, pg) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            emptyList()
        }
        for (joiner in pending) {
            val cap = CoPublisher.capabilityOfJoiner(groupId, joiner) ?: continue
            val link = PublisherLinkMatch.linkOf(
                records,
                channelBytes,
                cap,
                System.currentTimeMillis(),
                PublisherLinkMatch.coreOpen,
            ) ?: continue
            try {
                approveLocked(channelId, joiner)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "a named publisher's request was not approved yet: ${e.javaClass.simpleName}")
                return
            }
            try {
                GroupLinkRegister.revoke(link, state, client)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "a used publisher link was not revoked yet: ${e.javaClass.simpleName}")
            }
            return
        }
    }

    /** Owner device: remove the publisher [accountKey] (section 15, phase B). */
    public suspend fun removePublisher(channelId: String, accountKey: ByteArray) {
        mutex.withLock { removeLocked(channelId, accountKey) }
    }

    private suspend fun removeLocked(channelId: String, accountKey: ByteArray) {
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val store = state.commitStore ?: throw IllegalStateException("no commit store")
        val pg = groupFor(pub, channelId)
        knowEpochBeforeCommit(channelId)
        try {
            pub.withChannel { ids ->
                CoPublisher.removePublisher(ds, client, ids, pg, accountKey, channelId, account, store)
            }
        } catch (e: Throwable) {
            dropGroup(channelId)
            if (e is CoPublisher.PublisherCommitLost) lastJournalMs.remove(channelId)
            throw e
        }
        ChannelAdmissionSeedShare.forgetPublisher(context, account, channelId, accountKey)
        // Section 7 — the removed person held the admission seed: links made
        // from now on name a new version, which it never gets.
        ChannelAdmissionKeys.current(context, account, channelId)?.let { (version, seed) ->
            seed.fill(0)
            if (version < UShort.MAX_VALUE) {
                val nextVersion = (version + 1u).toUShort()
                val next = uniffi.lakko_ffi_uniffi.generateChannelAdmissionSeed()
                try {
                    ChannelAdmissionKeys.put(context, account, channelId, nextVersion, next)
                    ChannelAdmissionKeys.toRegister(channelId, nextVersion, next)
                } finally {
                    next.fill(0)
                }
            }
        }
        afterRotation(channelId)
        readPublishers(pub, channelId, groupFor(pub, channelId))
    }

    /** A co-publisher steps down (section 15, phase B): an owner device
     *  removes this account; the channel ends here once it has. */
    public suspend fun stepDown(channelId: String) {
        mutex.withLock {
            val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
            val pg = groupFor(pub, channelId)
            pub.withChannel { ids -> CoPublisher.stepDown(ds, ids, pg) }
        }
    }

    /**
     * ADR-0090 section 2.4 — this device leaves (Forget device, or it was
     * removed): for every channel it publishes it asks an owner device to
     * retire its leaf (an owner device cannot remove its own leaf). Best
     * effort; if lost, the reconcile of another device retires the leaf.
     */
    public suspend fun retireOwnLeavesBestEffort() {
        mutex.withLock {
            val pub = state.publisher ?: return
            if (!pub.hasDevice()) return
            for (channelId in state.keysByChannel.value.keys.toList()) {
                try {
                    val pg = groupFor(pub, channelId)
                    pub.withChannel { ids ->
                        val envelope = uniffi.lakko_ffi_uniffi.composePublisherDeviceRetire(
                            ChannelsComposer.channelIdFromHex(channelId),
                            ids.device.signaturePublicKey(),
                        )
                        CoPublisher.publishApplication(ds, ids, pg, envelope)
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.w(TAG, "a retire request was not sent: ${e.javaClass.simpleName}")
                }
            }
        }
    }

    /** Owner: delete the channel (section 15, phase B): the service first,
     *  then the channel ends on every device of the account. */
    public suspend fun deleteChannel(channelId: String) {
        mutex.withLock {
            val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
            // ADR-0064 section 1 — the channel's vault goes after the channel
            // delete landed, best effort. The bound identity is the lineage
            // one (the vault's root).
            pub.withIdentity(persist = false) { id ->
                deleteChannelThenVault(
                    deleteChannel = { client.deleteChannel(channelId, id) },
                ) {
                    withTimeoutOrNullVault { ChannelVaultHeads.deleteVault(id, ChannelsComposer.channelIdFromHex(channelId)) }
                }
            }
            endHere(channelId)
        }
    }

    /** Owner: drop a co-publisher request (no commit). */
    public suspend fun reject(channelId: String, joinerKey: ByteArray) {
        mutex.withLock {
            val pub = state.publisher ?: return
            val pg = groupFor(pub, channelId)
            val waiting = pub.withChannel { ids ->
                CoPublisher.reject(ids, pg, joinerKey)
                CoPublisher.pending(ids, pg)
            }
            _pending.update { it + (channelId to waiting) }
        }
    }

    /**
     * Section 7 — the one rotation a person asks for: after removing a
     * subscriber from a private channel, so the removed key gets no seed of
     * the next epoch. Runs regardless of the policy's schedule.
     */
    public suspend fun rotateNow(channelId: String) {
        // Only an owner device rotates (ADR-0090 decision C: any of them); a
        // co-publisher asks, on the PublisherGroup.
        if (!rotatesChannelKeyHere(isOwner(channelId))) {
            mutex.withLock { sendRotateRequestLocked(channelId) }
            return
        }
        mutex.withLock { rotateDueLocked(channelId, forced = true) }
    }

    /** A co-publisher's rotate request (ADR-0087 section 15, 2026-11-08):
     *  one application message on the PublisherGroup; a failure stays owed
     *  and the next pass sends it. */
    private suspend fun sendRotateRequestLocked(channelId: String) {
        val pub = state.publisher ?: return
        reissueRecordOwed(rotateRequestOwed, channelId) {
            try {
                val pg = groupFor(pub, channelId)
                val envelope = uniffi.lakko_ffi_uniffi.composeChannelRotateRequestEnvelope(
                    ChannelsComposer.channelIdFromHex(channelId),
                )
                pub.withChannel { ids -> CoPublisher.publishApplication(ds, ids, pg, envelope) }
                true
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "the rotate request was not sent yet: ${e.javaClass.simpleName}")
                false
            }
        }
    }

    /** Forget every open handle (sign-out, forget device). */
    public fun clear() {
        groups.values.forEach { it.close() }
        groups.clear()
        claimed.clear()
        lastRotationCheckMs.clear()
        publicSeedFiled.clear()
        rotateRequested.clear()
        refusedReads.clear()
        suspensionOk.clear()
        trailingSince.clear()
        deviceRequestSentMs.clear()
        markSent.clear()
        lastEntryMs.clear()
        entryUnsettled.clear()
        _pending.value = emptyMap()
        _subscriberCounts.value = emptyMap()
    }

    public companion object {
        private const val TAG = "ChannelPublisherLoop"

        /** The rotation check cadence (the old Protection card's). */
        private const val ROTATION_HEARTBEAT_MS: Long = 5L * 60L * 1_000L

        /** Between passes of the app-wide loop (2026-11-06: 60 s, the
         *  idle request budget web keeps; chosen, not measured). */
        public const val APP_INTERVAL_MS: Long = 60_000L

        /** Reading the PublisherGroup journal: a peer's commit, an owner's
         *  admission seed (chosen, not measured). */
        private const val JOURNAL_INTERVAL_MS: Long = 30_000L

        /** Reading the co-publisher knocks, owner only (chosen). */
        private const val KNOCK_INTERVAL_MS: Long = 60_000L

        /** Refreshing the exact subscriber count (chosen). */
        private const val COUNT_INTERVAL_MS: Long = 120_000L

        /** ADR-0064 section 7 — between two tries to enter an owned channel
         *  from its head when the last try found none to take (chosen). */
        private const val ENTRY_RETRY_MS: Long = 5L * 60L * 1_000L

        private val shared = java.util.WeakHashMap<ChannelsUiState, ChannelPublisherLoop>()

        /** The one loop for the session's [state]. */
        public fun forChannels(context: Context, state: ChannelsUiState, client: ChannelsClient): ChannelPublisherLoop =
            synchronized(shared) {
                shared.getOrPut(state) { ChannelPublisherLoop(context.applicationContext, state, client) }
            }

        /** The loop for [state] when one exists. */
        public fun peek(state: ChannelsUiState): ChannelPublisherLoop? = synchronized(shared) { shared[state] }
    }
}

/**
 * ADR-0087 section 15 (2026-11-08) — whether the owner's device rotates on a
 * rotate request it read on the channel's PublisherGroup: only the owner
 * acts, only on a request naming this channel, only from a current publisher
 * other than this account (the sender resolved at receipt).
 */
internal fun honoursRotateRequest(
    isOwner: Boolean,
    requestedChannelId: ByteArray,
    channelId: ByteArray,
    sender: ByteArray,
    ownAccountKey: ByteArray,
    currentPublishers: List<ByteArray>,
): Boolean =
    isOwner &&
        requestedChannelId.contentEquals(channelId) &&
        !sender.contentEquals(ownAccountKey) &&
        currentPublishers.any { it.contentEquals(sender) }

/**
 * Section 15 — the publishers of a channel as the PublisherGroup's members
 * name them: one row per ACCOUNT (ADR-0090: an account has a leaf per device),
 * the owner first by leaf order, [ownerKey] marked the owner, [ownKey] this
 * account. Pure.
 */
internal fun publishersOf(
    members: List<uniffi.lakko_ffi_uniffi.GroupMember>,
    ownerKey: ByteArray,
    ownKey: ByteArray,
): List<ChannelPublisherLoop.Publisher> {
    val seen = ArrayList<ChannelPublisherLoop.Publisher>()
    for (m in members.sortedBy { it.leafIndex }) {
        val key = m.accountKey.takeIf { it.size == 32 } ?: continue
        if (seen.any { it.accountKey.contentEquals(key) }) continue
        seen += ChannelPublisherLoop.Publisher(
            accountKey = key,
            owner = key.contentEquals(ownerKey),
            self = key.contentEquals(ownKey),
        )
    }
    return seen.sortedByDescending { it.owner }
}
