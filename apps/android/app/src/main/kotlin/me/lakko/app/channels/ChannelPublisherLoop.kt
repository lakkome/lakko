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

    /** One pass over every channel this device publishes. Never throws. */
    public suspend fun passAll() {
        if (state.boundAccount == null) return
        mutex.withLock {
            for (id in state.keysByChannel.value.keys.toList()) {
                try {
                    pass(id)
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
        }
        // Section 13a — reaction writes a failed send left owed.
        ChannelFeedbackOutbox.resendAll(context, state, client)
    }

    /** The channel ended on this device (deleted, or this account is no
     *  longer one of its publishers): its state goes from every device of
     *  the account, as an unsubscribe does (section 9). */
    private fun endHere(channelId: String) {
        val name = state.channels.value.firstOrNull { it.id == channelId }
            ?.let { it.sharedName.ifBlank { it.localName } }.orEmpty()
        dropGroup(channelId)
        _publishers.update { it - channelId }
        _pending.update { it - channelId }
        ChannelRemovalWatch.endEverywhere(state, channelId, name, onChannelEnded)
    }

    private fun readPublishers(channelId: String, pg: PublisherGroup, ownKey: ByteArray) {
        val members = try {
            pg.members()
        } catch (_: Throwable) {
            return
        }
        val ownerKey = members.firstOrNull { it.leafIndex == 0u }?.accountKey
        val seen = ArrayList<Publisher>()
        for (m in members.sortedBy { it.leafIndex }) {
            val key = m.accountKey.takeIf { it.size == 32 } ?: continue
            if (seen.any { it.accountKey.contentEquals(key) }) continue
            seen += Publisher(
                accountKey = key,
                owner = ownerKey != null && key.contentEquals(ownerKey),
                self = key.contentEquals(ownKey),
            )
        }
        _publishers.update { it + (channelId to seen) }
    }

    /** Whether this account OWNS [channelId] (a co-published one carries the
     *  group id its link conveyed). */
    public fun isOwner(channelId: String): Boolean = state.joinedGroupIdOf(channelId) == null

    private suspend fun groupFor(pub: ChannelPublisherIdentity, channelId: String): PublisherGroup {
        groups[channelId]?.let { return it }
        val opened = pub.withIdentity { id -> CoPublisher.openPublisherGroup(id, channelId, state.coPublished) }
        groups[channelId] = opened
        return opened
    }

    private fun dropGroup(channelId: String) {
        groups.remove(channelId)?.close()
        // A reopened group reads its journal again from the start (rows it
        // already merged are skipped by the core).
        journalCursor.remove(channelId)
        knockCursor.remove(channelId)
        lastJournalMs.remove(channelId)
    }

    private suspend fun pass(channelId: String) {
        val pub = state.publisher ?: return
        val account = state.boundAccount ?: return
        if (channelId in state.orphaned.value) return
        val pg = groupFor(pub, channelId)
        val owner = isOwner(channelId)
        // Section 15 (phase B) — a co-publisher the owner removed (or that
        // stepped down) is no longer in the group: the channel ends here.
        if (!owner && runCatching { pg.isActive() }.getOrDefault(true) == false) {
            endHere(channelId)
            return
        }
        // ADR-0057 — the OWNER claims the journal (an idempotent re-send on a
        // later open); a co-publisher never does.
        if (owner && claimed.add(channelId)) {
            try {
                pub.withIdentity(persist = false) { id ->
                    CoPublisher.claimJournal(ds, id, pg, state.coPublished ?: InMemoryCoPublishedJournalMarks())
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                claimed.remove(channelId)
                Log.w(TAG, "publisher journal claim did not run", e)
            }
        }
        // Co-publisher requests (knock rows, the owner only, slowly) and
        // peers' commits (from the cursor), each on its own cadence.
        val now0 = System.currentTimeMillis()
        val readJournal = now0 - (lastJournalMs[channelId] ?: 0L) >= JOURNAL_INTERVAL_MS
        val readKnocks = owner && now0 - (lastKnockMs[channelId] ?: 0L) >= KNOCK_INTERVAL_MS
        val poll = if (readJournal || readKnocks) {
            pub.withIdentity(persist = false) { id ->
                CoPublisher.pollJournal(
                    ds,
                    id,
                    pg,
                    onRequest = { joinerKey, accountId ->
                        ChannelAdmissionSeedShare.rememberRequest(context, account, channelId, joinerKey, accountId)
                    },
                    after = journalCursor[channelId] ?: 0L,
                    knockAfter = if (readKnocks) knockCursor[channelId] ?: 0L else null,
                )
            }.also { p ->
                lastJournalMs[channelId] = now0
                journalCursor[channelId] = p.journalCursor
                if (readKnocks) {
                    lastKnockMs[channelId] = now0
                    p.knockCursor?.let { knockCursor[channelId] = it }
                }
            }
        } else {
            CoPublisher.JournalPoll(applied = 0, pending = 0)
        }
        if (poll.applied > 0) pub.persist()
        val ownKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        readPublishers(channelId, pg, ownKey)
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        // Section 7 / 15 — what the owner sealed to this publisher: an
        // admission seed or the read key, kept only from the owner (the
        // account at the founding leaf, resolved at receipt).
        if (!owner && poll.applications.isNotEmpty()) {
            val ownerKey = _publishers.value[channelId]?.firstOrNull { it.owner }?.accountKey
            pub.withIdentity(persist = false) { id ->
                poll.applications.forEachIndexed { i, plaintext ->
                    val sender = poll.senders.getOrNull(i)
                    if (ownerKey == null || sender == null || !sender.contentEquals(ownerKey)) return@forEachIndexed
                    ChannelAdmissionSeedShare.receive(context, account, channelId, id, plaintext)
                    ChannelAdmissionSeedShare.receiveReadKey(context, account, channelId, id, plaintext)
                }
            }
        }
        // Phase B — a co-publisher posts its own binding once per epoch, so
        // every owner device (not only the one that approved it) can later
        // withdraw its appointment.
        val epochNow = runCatching { pg.epoch() }.getOrNull()
        if (!owner && epochNow != null && !CoPublisherBindingStore.posted(channelId, epochNow)) {
            try {
                pub.withIdentityPersistingFirst { id, persistNow -> CoPublisher.postBinding(ds, id, pg, channelBytes, persistNow) }
                CoPublisherBindingStore.markPosted(channelId, epochNow)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "the co-publisher binding was not posted yet: ${e.javaClass.simpleName}")
            }
        }
        // Section 15 (phase B) — a co-publisher stepped down: the owner
        // removes the account the sender resolves to, then reseals. A
        // co-publisher's posted binding is filed for a later removal.
        if (owner && poll.applications.isNotEmpty()) {
            val groupId = runCatching { pg.groupId() }.getOrNull()
            poll.applications.forEachIndexed { i, plaintext ->
                val sender = poll.senders.getOrNull(i) ?: return@forEachIndexed
                if (groupId != null && !sender.contentEquals(ownKey) &&
                    CoPublisher.recordJournalBinding(groupId, channelBytes, sender, plaintext)
                ) {
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
        }
        if (owner && ChannelAdmissionSeedShare.anyOwed(context, account, channelId)) {
            pub.withIdentityPersistingFirst { id, persistNow ->
                ChannelAdmissionSeedShare.sendOwed(context, account, channelId, id, pg, ds, persistNow)
            }
        }
        if (owner) {
            approveNamed(channelId, pg)
            val waiting = pub.withIdentity(persist = false) { CoPublisher.pending(groupFor(pub, channelId)) }
            _pending.update { it + (channelId to waiting) }
        }
        // G-50 — recorded commits that still owe a step.
        val store = state.commitStore
        if (store != null && store.entries(account).isNotEmpty()) {
            val finished = pub.withIdentity(persist = false) { id ->
                CoPublisher.replayPublisherCommits(ds, client, id, account, store, state.coPublished)
            }
            if (finished.isNotEmpty()) {
                dropGroup(channelId)
                for (id in finished) state.refreshPublisherKey(id)
            }
        }
        val channel = state.channels.value.firstOrNull { it.id == channelId }
        if (owner && channel != null) {
            ChannelDiscovery.sendOwedRekeyIfAny(context, state, client, account, channel)
            ChannelDiscovery.sendOwedRecordIfAny(context, pub, account, channel)
        }
        // The rotation heartbeat.
        val now = System.currentTimeMillis()
        if (now - (lastRotationCheckMs[channelId] ?: 0L) >= ROTATION_HEARTBEAT_MS) {
            lastRotationCheckMs[channelId] = now
            val stored = state.rotationState(channelId)?.let {
                ChannelRotationState(lastRotationMs = it.lastRotationMs, postsSinceRotation = it.postsSinceRotation)
            } ?: channelRotationStateAfterRotation(now.toULong()).also { seeded ->
                state.saveRotationState(channelId, StoredRotationState(seeded.lastRotationMs, seeded.postsSinceRotation))
            }
            knowEpochBeforeCommit(channelId)
            val result = rotate(pub, groupFor(pub, channelId), channelId, stored)
            if (result.rotated) afterRotation(channelId)
        }
        if (owner) filePublicSeedIfDue(pub, channelId)
        // Section 15 — the exact count, for the info screen (not every pass).
        if (System.currentTimeMillis() - (lastCountMs[channelId] ?: 0L) < COUNT_INTERVAL_MS) return
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

    private suspend fun rotate(
        pub: ChannelPublisherIdentity,
        pg: PublisherGroup,
        channelId: String,
        rotationState: ChannelRotationState,
    ): RotationCron.OnceResult {
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        val store = state.commitStore ?: throw IllegalStateException("no commit store")
        return pub.withIdentityPersistingFirst { id, persistNow ->
            RotationCron(
                publisherGroup = pg,
                identity = id,
                channelIdHex = channelId,
                channelsClient = client,
                saveState = { next ->
                    state.saveRotationState(channelId, StoredRotationState(next.lastRotationMs, next.postsSinceRotation))
                },
                accountIdB64 = account,
                store = store,
                persistNow = persistNow,
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

    /** A rotation landed: the composer's key, the epoch, the record re-issued
     *  under the new key (verbatim), the public seed of the new epoch. */
    private suspend fun afterRotation(channelId: String) {
        state.refreshPublisherKey(channelId)
        try {
            state.setEpoch(channelId, client.fetchEpoch(channelId))
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            // The next read learns it.
        }
        ChannelMetaPublisher.reissue(state, client, channelId)
        state.publisher?.let { if (isOwner(channelId)) filePublicSeedIfDue(it, channelId) }
    }

    /**
     * Section 6 — a PUBLIC channel publishes one seed envelope per epoch,
     * sealed under its read key, so a new subscriber reads at once. The owner
     * files the current epoch's; the identical envelope again is a success.
     */
    private suspend fun filePublicSeedIfDue(pub: ChannelPublisherIdentity, channelId: String) {
        val channel = state.channels.value.firstOrNull { it.id == channelId } ?: return
        if (!channel.sharedListed) return
        val keys = state.keysByChannel.value[channelId] ?: return
        val epoch = state.epochByChannel.value[channelId]
            ?: client.fetchEpoch(channelId).also { state.setEpoch(channelId, it) }
        if (publicSeedFiled[channelId] == epoch) return
        val envelope = pub.withIdentity(persist = false) { id ->
            val readKey = ChannelDiscovery.readKey(id, channelId)
            try {
                uniffi.lakko_ffi_uniffi.sealPublicSeedEnvelope(
                    readKey,
                    ChannelsComposer.channelIdFromHex(channelId),
                    epoch,
                    keys.seed,
                )
            } finally {
                readKey.fill(0)
            }
        }
        ChannelPublisherSigners.derive(pub, channelId).use { client.putPublicSeed(channelId, epoch, envelope, it) }
        publicSeedFiled[channelId] = epoch
    }

    /** Owner: approve a co-publisher request (the audited path: adds the
     *  joiner, reseals the new key to every subscriber, appoints its key). */
    public suspend fun approve(channelId: String, joinerKey: ByteArray) {
        mutex.withLock { approveLocked(channelId, joinerKey) }
    }

    private suspend fun approveLocked(channelId: String, joinerKey: ByteArray) {
        run {
            val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
            val account = state.boundAccount ?: throw IllegalStateException("no account bound")
            val store = state.commitStore ?: throw IllegalStateException("no commit store")
            val pg = groupFor(pub, channelId)
            knowEpochBeforeCommit(channelId)
            try {
                pub.withIdentityPersistingFirst { id, persistNow ->
                    CoPublisher.approve(ds, client, id, pg, joinerKey, channelId, account, store, persistNow)
                }
            } catch (e: Throwable) {
                // G-50 — the provider was rolled back: reopen the group.
                dropGroup(channelId)
                throw e
            }
            ChannelAdmissionSeedShare.notePublisher(context, account, channelId, joinerKey)
            state.refreshPublisherKey(channelId)
            afterRotation(channelId)
            val waiting = pub.withIdentity(persist = false) { CoPublisher.pending(groupFor(pub, channelId)) }
            _pending.update { it + (channelId to waiting) }
        }
    }

    /**
     * Section 15 — the owner's device approves without asking a request that
     * came by a live publisher link of this account (`Add publisher` named
     * that person), then revokes the used link through the register, which
     * deletes its ledger row. Any other request waits for the owner.
     */
    private suspend fun approveNamed(channelId: String, pg: PublisherGroup) {
        val records = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.linkRecords?.value ?: return
        if (records.none { it.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.PUBLISHER && !it.revoked }) return
        val groupId = runCatching { pg.groupId() }.getOrNull() ?: return
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val pending = runCatching { CoPublisher.pending(pg) }.getOrDefault(emptyList())
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
            // A publisher link is for one person: once used it is revoked on
            // every device of the account, and its ledger row goes.
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

    /** Owner: remove the publisher [accountKey] (section 15, phase B). */
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
            pub.withIdentityPersistingFirst { id, persistNow ->
                CoPublisher.removePublisher(ds, client, id, pg, accountKey, channelId, account, store, persistNow)
            }
        } catch (e: Throwable) {
            // G-50 — the provider was rolled back: reopen the group.
            dropGroup(channelId)
            throw e
        }
        ChannelAdmissionSeedShare.forgetPublisher(context, account, channelId, accountKey)
        // Section 7 — the removed person held the admission seed: links made
        // from now on name a new version, which it never gets.
        ChannelAdmissionKeys.current(context, account, channelId)?.let { (version, seed) ->
            seed.fill(0)
            if (version < UShort.MAX_VALUE) {
                val next = uniffi.lakko_ffi_uniffi.generateChannelAdmissionSeed()
                try {
                    ChannelAdmissionKeys.put(context, account, channelId, (version + 1u).toUShort(), next)
                } finally {
                    next.fill(0)
                }
            }
        }
        state.refreshPublisherKey(channelId)
        afterRotation(channelId)
        val ownKey = pub.withIdentity(persist = false) { it.signaturePublicKey() }
        readPublishers(channelId, groupFor(pub, channelId), ownKey)
    }

    /** A co-publisher steps down (section 15, phase B): the owner's device
     *  removes this account; the channel ends here once it has. */
    public suspend fun stepDown(channelId: String) {
        mutex.withLock {
            val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
            val pg = groupFor(pub, channelId)
            pub.withIdentityPersistingFirst { id, persistNow -> CoPublisher.stepDown(ds, id, pg, persistNow) }
        }
    }

    /** Owner: delete the channel (section 15, phase B): the service first,
     *  then the channel ends on every device of the account. */
    public suspend fun deleteChannel(channelId: String) {
        mutex.withLock {
            val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
            pub.withIdentity(persist = false) { id -> client.deleteChannel(channelId, id) }
            endHere(channelId)
        }
    }

    /** Owner: drop a co-publisher request (no commit). */
    public suspend fun reject(channelId: String, joinerKey: ByteArray) {
        mutex.withLock {
            val pub = state.publisher ?: return
            val pg = groupFor(pub, channelId)
            val waiting = pub.withIdentity { id ->
                CoPublisher.reject(id, pg, joinerKey)
                CoPublisher.pending(pg)
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
        mutex.withLock {
            val pub = state.publisher ?: return
            val result = try {
                rotate(pub, groupFor(pub, channelId), channelId, ChannelRotationState(0uL, 0u))
            } catch (e: Throwable) {
                dropGroup(channelId)
                throw e
            }
            state.saveRotationState(
                channelId,
                StoredRotationState(result.newState.lastRotationMs, result.newState.postsSinceRotation),
            )
            lastRotationCheckMs[channelId] = System.currentTimeMillis()
            if (result.rotated) afterRotation(channelId)
        }
    }

    /** Forget every open handle (sign-out, forget device). */
    public fun clear() {
        groups.values.forEach { it.close() }
        groups.clear()
        claimed.clear()
        lastRotationCheckMs.clear()
        publicSeedFiled.clear()
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
