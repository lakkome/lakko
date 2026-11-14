// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.SealedBlobStorage
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.sealSubscriberSeedEnvelope

/**
 * Durable record of a channel PublisherGroup commit (G-50, the ADR-0040
 * treatment the chat groups have). Kotlin mirror of
 * `apps/web/src/channels/publisher-commit-outbox.ts` and
 * `apps/ios/Lakko/Channels/PublisherCommitOutbox.swift`.
 *
 * ADR-0090 section 3: a PublisherGroup commit (a co-publisher approve, a
 * rotation, a publisher removal, an admission or retirement of devices) is
 * STAGED on this device's leaf and recorded, with the provider snapshot
 * carrying the staged commit and the epoch-N access seed, before anything
 * leaves the device. The commit route then decides: taken (`201`, or `200`
 * for the identical re-send), the stage is merged and the other steps run;
 * `409`, the stage is abandoned, the entry dropped and nothing more happens
 * (the journal read applies the winner). The reseal follows the merge,
 * never the stage. Its effects then leave the device in steps: the N+1 epoch key is registered on
 * the Delivery Service and the Add commit published to the journal, the
 * joiner's Welcome is written to its mailbox (approve), every subscriber is
 * resealed to the N+1 seed and the channels service moves to the next epoch,
 * and the newcomer's co-publisher key is appointed (approve).
 *
 * Order: the entry FIRST, then the N+1 provider snapshot
 * ([ChannelPublisherIdentity.withIdentityPersistingFirst]'s `persistNow`),
 * both fail-closed, both before any step runs; a failure of either restores
 * the provider to the snapshot taken before the commit, so nothing records or
 * publishes an epoch the device does not hold durably. "N+1 durable with no
 * entry" is impossible by construction; an entry whose snapshot did not land
 * is a commit none of whose steps ran, and the next replay drops it
 * ([replayVerdict]).
 *
 * Each step marks itself done as it lands; the entry is dropped once none is
 * left. A replay runs the remaining steps again, each idempotent (a duplicate
 * commit is dropped by the journal's receivers, a second Welcome for a group
 * the joiner is in is skipped by it, the reseal counts as done once the
 * service is at or past the recorded target, an appointment is an upsert).
 *
 * An entry holds the commit and Welcome (public MLS artefacts), the joiner's
 * key and the epochs; the joiner's `reply` grant is not recorded (it is
 * re-read from the retained knock on every poll). Entries live in one file
 * per account under `noBackupFilesDir`, sealed under the device key.
 */
@Serializable
public data class PublisherCommitEntry(
    /** Random hex id. */
    val id: String,
    val kind: Kind,
    /** 32-char hex channel id. */
    val channelIdHex: String,
    /** 64-char hex PublisherGroup id (the DS journal). */
    val groupIdHex: String,
    /** The MLS epoch the commit moved the group to. */
    val targetEpoch: Long,
    /** The channels-service epoch the reseal binds the envelopes to. */
    val serverTargetEpoch: Long,
    /** Approve: standard base64 of the Add commit for the journal. */
    val commitB64: String? = null,
    /** Approve: standard base64 of the joiner's Welcome. */
    val welcomeB64: String? = null,
    /** Approve: standard base64 of the joiner's signature (= account) key. */
    val joinerKeyB64: String? = null,
    /** ADR-0081 — the epoch the commit LEFT, whose key proves the commit
     *  request. The commit merged eagerly, so after a restart the group holds
     *  only the N+1 key, and the DS takes a commit only under a proof of N (an
     *  identical re-send under any retained key); with the seed a replay
     *  always learns the answer: appended, the original row, or `409`. */
    val proofEpoch: Long? = null,
    /** ADR-0081 — standard base64 of that epoch's 32-byte access seed. Lives
     *  only in this device-key-sealed record and goes with it. */
    val proofSeedB64: String? = null,
    /** Kept for reading entries written before ADR-0090; never set now: a
     *  lost commit is abandoned and its entry dropped. */
    val journalLost: Boolean = false,
    /** ADR-0090 section 2.2 — the committer delegation header a
     *  co-publisher's device request carried for its mailbox (ADMIT). */
    val delegationHeader: String? = null,
    /** ADR-0090 — standard base64 of that request's 97-byte mailbox binding. */
    val delegationBindingB64: String? = null,
    /** The steps that have landed. */
    val done: List<Step> = emptyList(),
) {
    /** Which commit an entry records. */
    @Serializable
    public enum class Kind {
        APPROVE,
        ROTATION,

        /** ADR-0087 section 15 (phase B) — the owner removed a publisher
         *  ([joinerKeyB64] = its account key): every leaf of it in one
         *  commit, which also changes the content seed. */
        REMOVE,

        /** ADR-0090 sections 2.1 / 2.2 — devices of an account that already
         *  publishes admitted and / or retired in one commit; [joinerKeyB64]
         *  is the account the Welcome goes to (absent for a retirement). */
        ADMIT,

        /** ADR-0064 section 7 (amendment of 2026-11-14) — this owner device
         *  entered the PublisherGroup by an external commit from a GroupInfo
         *  saved in the channel's vault head ([proofEpoch] / [proofSeedB64]:
         *  that saved epoch and its access seed). */
        ENTER,
    }

    /** The steps of an entry, in the order they run. */
    @Serializable
    public enum class Step {
        JOURNAL,
        WELCOME,
        RESEAL,
        APPOINT,

        /** The removed publisher's appointment is withdrawn at the service. */
        UNAPPOINT,

        /** ADR-0090 section 6 — the account of the admitted devices is owed
         *  the read key, the admission seeds and the epoch seeds again. */
        HANDOVER,

        /** ADR-0064 section 7 — the channel's vault head gets the epoch the
         *  commit landed at. Never holds the entry. */
        HEAD,
    }

    /** The steps still to run, in order. */
    public val remainingSteps: List<Step> get() = stepsOf(kind).filterNot { it in done }

    /** The entry with [step] marked done (once). */
    public fun withStepDone(step: Step): PublisherCommitEntry =
        if (step in done) this else copy(done = done + step)

    /** Whether the reseal of this entry has already landed: the channels
     *  service is at or past the epoch it resealed to. */
    public fun resealLanded(serverEpoch: Long): Boolean = serverEpoch >= serverTargetEpoch

    /** Whether the fields are well formed (a malformed entry read back is
     *  dropped alone). */
    public val isWellFormed: Boolean
        get() {
            fun isHex(s: String, len: Int) = s.length == len && s.all { it in '0'..'9' || it in 'a'..'f' }
            if (!isHex(id, 32) || !isHex(channelIdHex, 32) || !isHex(groupIdHex, 64)) return false
            if (targetEpoch < 0 || serverTargetEpoch < 0) return false
            if (kind == Kind.APPROVE && (commitB64 == null || welcomeB64 == null || joinerKeyB64 == null)) return false
            if (kind == Kind.REMOVE && (commitB64 == null || joinerKeyB64 == null)) return false
            if (kind == Kind.ADMIT && commitB64 == null) return false
            if (kind == Kind.ENTER && (commitB64 == null || proofEpoch == null || proofSeedB64 == null)) return false
            return true
        }

    public companion object {
        /** The steps an entry of [kind] runs, in order. */
        public fun stepsOf(kind: Kind): List<Step> = when (kind) {
            // ADR-0090 — the Welcome after the reseal: the joining device
            // learns the channel epoch its group's epoch serves from the
            // service when it lands (ChannelEpochOffsets).
            Kind.APPROVE -> listOf(Step.JOURNAL, Step.RESEAL, Step.WELCOME, Step.APPOINT)
            Kind.ROTATION -> listOf(Step.JOURNAL, Step.RESEAL)
            Kind.REMOVE -> listOf(Step.JOURNAL, Step.RESEAL, Step.UNAPPOINT)
            Kind.ADMIT -> listOf(Step.JOURNAL, Step.RESEAL, Step.WELCOME, Step.HANDOVER)
            // ADR-0064 section 7 — merge on the DS's answer, reseal the new
            // seed as after any admission, then save the head.
            Kind.ENTER -> listOf(Step.JOURNAL, Step.RESEAL, Step.HEAD)
        }

        /** A fresh random 16-byte hex id. */
        public fun newId(): String {
            val bytes = ByteArray(16).also { java.security.SecureRandom().nextBytes(it) }
            return bytes.joinToString("") { "%02x".format(it) }
        }
    }
}

/** What a restart does with an entry. */
public enum class PublisherCommitReplay {
    /** The commit's N+1 never became durable (a torn write, or the group is
     *  gone): none of its steps ran, drop it. */
    DISCARD,

    /** Run the remaining steps. */
    REPLAY,
}

/** The verdict for [entry], given the epoch the restored group is at
 *  (`null`: the group did not load at all) and whether it holds a staged
 *  commit (ADR-0090: the entry's commit, staged at `targetEpoch - 1`, whose
 *  verdict the DS has not given yet). */
public fun replayVerdict(entry: PublisherCommitEntry, restoredEpoch: Long?, staged: Boolean = false): PublisherCommitReplay =
    when {
        restoredEpoch == null -> PublisherCommitReplay.DISCARD
        restoredEpoch >= entry.targetEpoch -> PublisherCommitReplay.REPLAY
        staged && restoredEpoch == entry.targetEpoch - 1 -> PublisherCommitReplay.REPLAY
        else -> PublisherCommitReplay.DISCARD
    }

/** How running an entry ended (ADR-0090 section 3). */
public enum class PublisherCommitOutcome {
    /** The DS took the commit, the stage merged, every step landed. */
    LANDED,

    /** The DS took another device's commit at that epoch (`409`): the stage
     *  was abandoned and the entry dropped; nothing of it left the device. */
    LOST,
}

/** What running the steps of one entry needs; injected so the order is
 *  testable without a network. */
public interface PublisherCommitPorts {
    /** ADR-0064 section 7 — before the commit is sent: the channel's vault
     *  head gets the GroupInfo of the epoch the staged commit moves to. One
     *  attempt; a failure (thrown or not) never stops the commit. */
    public suspend fun beforeSend() {}

    /** ADR-0064 section 7 — the [PublisherCommitEntry.Step.HEAD] step: the
     *  head gets the current epoch. Never holds the entry. */
    public suspend fun writeHead() {}

    /** ADR-0081 decision 1 — register the group's CURRENT epoch key (the N+1
     *  one) with the channel's root AND append [commit] to the group journal,
     *  in one request (`POST .../commit`), proven under the recorded key of
     *  the epoch the commit left ([proofEpoch], [proofSeed]; `null` on an
     *  entry that kept none). A re-send of the same commit is idempotent on
     *  the DS. */
    public suspend fun commitToJournal(commit: ByteArray, proofEpoch: Long?, proofSeed: ByteArray?)

    /** ADR-0081 — whether a failed journal step is lost (`409`, a definitive
     *  refusal): nothing of the commit is or will be on the DS. */
    public fun isLost(cause: Throwable): Boolean

    /** ADR-0090 — the DS took the commit: merge the staged commit (a no-op
     *  when it already merged) and persist the provider. */
    public suspend fun merge() {}

    /** ADR-0090 — the DS took another commit: abandon the staged one and
     *  persist the provider. */
    public suspend fun abandon() {}

    /** Write the Welcome to [accountKey]'s mailbox (the account of the
     *  joiner, or of the admitted devices). */
    public suspend fun deliverWelcome(entry: PublisherCommitEntry, welcome: ByteArray, accountKey: ByteArray)

    /** ADR-0090 section 6 — [accountKey] is owed the hand-over again. */
    public suspend fun handOver(accountKey: ByteArray) {}

    /** The channels service's current epoch. */
    public suspend fun serverEpoch(): Long

    /** Reseal the group's CURRENT seed to every subscriber and move the
     *  service to [targetEpoch] (stage + commit). */
    public suspend fun reseal(targetEpoch: Long)

    /** Appoint the joiner's co-publisher key. Never fails the entry. */
    public suspend fun appoint(joinerKey: ByteArray)

    /** Withdraw the appointment of a removed publisher (its account key).
     *  Never fails the entry. */
    public suspend fun unappoint(accountKey: ByteArray) {}

    /** Durably record [entry] (a step landed). */
    public fun save(entry: PublisherCommitEntry)

    /** The entry is finished: drop it. */
    public fun finish(entry: PublisherCommitEntry)
}

private fun b64(s: String): ByteArray = java.util.Base64.getDecoder().decode(s)

/**
 * Run the remaining steps of [entry] in order, recording each as it lands.
 * ADR-0090 section 3: the journal step comes first; a `409` abandons the
 * stage, drops the entry and returns [PublisherCommitOutcome.LOST]; any other
 * failure there leaves the stage and the entry for the next run (the owed
 * verdict). Only once the DS took the commit is the stage merged and is
 * anything else (a Welcome, the reseal) done. A later step's failure throws,
 * leaving the entry (with what did land) for the next run.
 */
public suspend fun runPublisherCommit(
    entry: PublisherCommitEntry,
    ports: PublisherCommitPorts,
    /** Where a failure that does not hold the entry is logged (a host test
     *  passes its own: the host JVM has no `android.util.Log`). */
    warn: (message: String, cause: Throwable) -> Unit = { message, cause ->
        android.util.Log.w("PublisherCommit", message, cause)
    },
): PublisherCommitOutcome {
    var current = entry
    for (step in current.remainingSteps) {
        when (step) {
            PublisherCommitEntry.Step.JOURNAL -> {
                val commit = current.commitB64
                if (commit != null) {
                    // ADR-0064 section 7 — the head first, best effort.
                    try {
                        ports.beforeSend()
                    } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        warn("ADR-0064: the channel head was not written before the commit", e)
                    }
                    try {
                        ports.commitToJournal(b64(commit), current.proofEpoch, current.proofSeedB64?.let(::b64))
                    } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        if (!ports.isLost(e)) throw e
                        // ADR-0090 — another owner device committed at this
                        // epoch: nothing of ours is on the journal.
                        warn("ADR-0090: PublisherGroup commit not taken by the journal", e)
                        ports.abandon()
                        ports.finish(current)
                        return PublisherCommitOutcome.LOST
                    }
                }
                ports.merge()
            }
            PublisherCommitEntry.Step.WELCOME -> {
                val welcome = current.welcomeB64
                val joiner = current.joinerKeyB64
                if (welcome != null && joiner != null) ports.deliverWelcome(current, b64(welcome), b64(joiner))
            }
            PublisherCommitEntry.Step.RESEAL ->
                if (!current.resealLanded(ports.serverEpoch())) ports.reseal(current.serverTargetEpoch)
            PublisherCommitEntry.Step.APPOINT ->
                current.joinerKeyB64?.let { joiner ->
                    try {
                        ports.appoint(b64(joiner))
                    } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        // An appointment is retried by the next co-publisher
                        // open; it never holds the entry.
                        warn("ADR-0059: appointment after an approve failed", e)
                    }
                }
            PublisherCommitEntry.Step.UNAPPOINT ->
                current.joinerKeyB64?.let { removed ->
                    try {
                        ports.unappoint(b64(removed))
                    } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        warn("ADR-0087: withdrawing a removed publisher's appointment failed", e)
                    }
                }
            PublisherCommitEntry.Step.HANDOVER ->
                current.joinerKeyB64?.let { account -> ports.handOver(b64(account)) }
            PublisherCommitEntry.Step.HEAD ->
                try {
                    ports.writeHead()
                } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    warn("ADR-0064: the channel head was not written after the commit", e)
                }
        }
        current = current.withStepDone(step)
        ports.save(current)
    }
    ports.finish(current)
    return PublisherCommitOutcome.LANDED
}

/**
 * The per-account store of recorded PublisherGroup commits: one file per
 * account, the JSON array of entries, sealed under the device key. Every
 * call runs under the channel provider's lock (the callers are inside
 * [ChannelPublisherIdentity.withIdentity]), so a read-modify-write is never
 * interleaved.
 */
public class PublisherCommitStore(private val storage: SealedBlobStorage) {
    private val json = Json { ignoreUnknownKeys = false }
    private val serializer = ListSerializer(PublisherCommitEntry.serializer())

    /** Every well-formed entry of the account. An unreadable file reads as
     *  empty; a malformed entry is dropped alone. */
    public fun entries(accountIdB64: String): List<PublisherCommitEntry> {
        val name = nameOf(accountIdB64) ?: return emptyList()
        val plain = storage.read(name) ?: return emptyList()
        return try {
            json.decodeFromString(serializer, plain.decodeToString()).filter { it.isWellFormed }
        } catch (_: Throwable) {
            emptyList()
        } finally {
            plain.fill(0)
        }
    }

    /** Insert or replace [entry], durably. Throws when it was not written
     *  (fail-closed: a commit whose entry is not on disk is never published). */
    public fun put(accountIdB64: String, entry: PublisherCommitEntry) {
        val name = nameOf(accountIdB64) ?: throw IllegalStateException("no account to record the commit for")
        val all = entries(accountIdB64).filterNot { it.id == entry.id } + entry
        // Refused (a signed-out or wiped account) or failed: throws.
        me.lakko.app.account.AccountStoreGate.writeOrEnd(accountIdB64) {
            storage.write(name, json.encodeToString(serializer, all).encodeToByteArray())
        }
    }

    /** Drop the entry [id]. Best effort: a lost removal replays the entry,
     *  whose steps are all done or idempotent. */
    public fun remove(accountIdB64: String, id: String) {
        val name = nameOf(accountIdB64) ?: return
        val rest = entries(accountIdB64).filterNot { it.id == id }
        try {
            if (rest.isEmpty()) {
                storage.delete(name)
            } else {
                me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                    storage.write(name, json.encodeToString(serializer, rest).encodeToByteArray())
                }
            }
        } catch (_: Throwable) {
            // best effort
        }
    }

    private fun nameOf(accountIdB64: String): String? =
        me.lakko.app.auth.KeyRotationFiles.hexOf(accountIdB64)

    public companion object {
        /** The folder under `noBackupFilesDir`. */
        public const val DIR: String = "lakko-publisher-commits"

        public fun onDevice(context: Context): PublisherCommitStore =
            PublisherCommitStore(me.lakko.app.auth.DeviceSealedBlobStorage(context.applicationContext, DIR))

        /** Forget device: delete [accountIdB64]'s file without opening it. */
        public fun deleteAll(context: Context, accountIdB64: String) {
            val hex = me.lakko.app.auth.KeyRotationFiles.hexOf(accountIdB64) ?: return
            java.io.File(java.io.File(context.noBackupFilesDir, DIR), hex).delete()
        }
    }
}

/**
 * Reseal [publisherGroup]'s CURRENT seed to every subscriber bound to
 * [targetEpoch], stage the envelopes in bounded chunks and commit (ADR-0030;
 * C-42: the commit is the one atomic point). A single seal failure aborts
 * the whole batch. Requests are signed with this account's publisher role
 * (ADR-0059 section 9).
 */
public suspend fun resealToSubscribers(
    channelsClient: ChannelsClient,
    ids: ChannelIdentity,
    publisherGroup: PublisherGroup,
    channelIdHex: String,
    targetEpoch: Long,
    /** [ChannelReseal.resealThrough]: `null` reseals to every row; a value
     *  only to the rows at or below it (a private channel's rows let in). */
    throughSeq: Long? = null,
    /** docs/ux/second-device.md section 4a — the account's own reader key
     *  of the channel (hex): resealed to whatever the mark says. */
    alwaysKeys: Set<String> = emptySet(),
) {
    // ADR-0090 C-90: only the seed of a MERGED epoch (never a staged one).
    val seed = ids.mls { device -> publisherGroup.channelSeed(device) }
    val channelIdBytes = publisherGroup.channelId()
    val target = targetEpoch.toUInt()
    ChannelPublisherSigners.derive(ids.account, channelIdHex).use { proofs ->
        val subscribers = if (throughSeq == null) {
            channelsClient.listSubscribersAsPublisher(channelIdHex, proofs)
        } else {
            val rows = ArrayList<SubscriberRow>()
            var after = 0L
            while (true) {
                val page = channelsClient.subscriberPage(channelIdHex, proofs, after)
                rows += page.rows
                val last = page.rows.lastOrNull() ?: break
                if (page.nextAfter <= after) break
                // Past the mark, read on only while the account's own key
                // has not shown up.
                if (last.seq > throughSeq && ChannelReseal.allSeen(rows, alwaysKeys)) break
                after = page.nextAfter
            }
            ChannelReseal.resealKeys(rows, throughSeq, alwaysKeys)
        }
        val batch = ArrayList<RotateBatchEntry>(subscribers.size)
        for (subscriberHex in subscribers) {
            val subscriberBytes = hexToBytesOrNull(subscriberHex)
                ?: throw CoPublisher.CoPublisherException.MalformedSubscriberHex(subscriberHex)
            val envelope = sealSubscriberSeedEnvelope(subscriberBytes, channelIdBytes, target, seed)
            batch.add(
                RotateBatchEntry(
                    subscriber_id = subscriberHex,
                    envelope_b64 = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(envelope),
                ),
            )
        }
        var start = 0
        while (start < batch.size) {
            val end = minOf(start + ChannelsClient.MAX_STAGE_CHUNK_ENTRIES, batch.size)
            channelsClient.stageRotation(channelIdHex, target, batch.subList(start, end), proofs)
            start = end
        }
        channelsClient.commitRotation(channelIdHex, target, proofs)
    }
}

/**
 * ADR-0090 decision A — reseal [publisherGroup]'s current seed to exactly
 * [keys] (lower-case hex subscriber keys: the rows a re-founding device let
 * in by their tickets, and the account's own reader key), then move the
 * service to [targetEpoch]. The same chunked stage + atomic commit as
 * [resealToSubscribers].
 */
public suspend fun resealToKeys(
    channelsClient: ChannelsClient,
    ids: ChannelIdentity,
    publisherGroup: PublisherGroup,
    channelIdHex: String,
    targetEpoch: Long,
    keys: Collection<String>,
) {
    val seed = ids.mls { device -> publisherGroup.channelSeed(device) }
    val channelIdBytes = publisherGroup.channelId()
    val target = targetEpoch.toUInt()
    ChannelPublisherSigners.derive(ids.account, channelIdHex).use { proofs ->
        val batch = keys.map { it.lowercase() }.distinct().map { subscriberHex ->
            val subscriberBytes = hexToBytesOrNull(subscriberHex)
                ?: throw CoPublisher.CoPublisherException.MalformedSubscriberHex(subscriberHex)
            RotateBatchEntry(
                subscriber_id = subscriberHex,
                envelope_b64 = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(
                    sealSubscriberSeedEnvelope(subscriberBytes, channelIdBytes, target, seed),
                ),
            )
        }
        var start = 0
        while (start < batch.size) {
            val end = minOf(start + ChannelsClient.MAX_STAGE_CHUNK_ENTRIES, batch.size)
            channelsClient.stageRotation(channelIdHex, target, batch.subList(start, end), proofs)
            start = end
        }
        channelsClient.commitRotation(channelIdHex, target, proofs)
    }
}

private fun hexToBytesOrNull(hex: String): ByteArray? {
    if (hex.isEmpty() || hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        out[i] = (hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null).toByte()
    }
    return out
}

/**
 * ADR-0087 section 7 — whom a rotation reseals to per key, the web
 * `resealPlan`: only the rows at or below this device's mark — the readers
 * from before the channel went private and the rows a let-in pass let in —
 * never a row with no valid ticket a server could have added. This device
 * keeps no type for the channel where the rotation runs, so it does the same
 * in a public channel, as web does: its readers read the public seed the
 * publisher loop files. Pure.
 */
public object ChannelReseal {
    public fun resealThrough(mark: Long): Long = mark

    /** The rows a per-key reseal seals to: those at or below [throughSeq],
     *  and the account's own reader key ([alwaysKeys], hex) wherever its row
     *  is (docs/ux/second-device.md section 4a: the account's other devices
     *  read under it, like "this device's own key is never removed"). */
    public fun resealKeys(rows: List<SubscriberRow>, throughSeq: Long, alwaysKeys: Set<String> = emptySet()): List<String> {
        val always = alwaysKeys.mapTo(HashSet()) { it.lowercase() }
        return rows.filter { it.seq <= throughSeq || it.pubkeyHex.lowercase() in always }.map { it.pubkeyHex }
    }

    /** Whether every key of [alwaysKeys] has a row in [rows]. */
    public fun allSeen(rows: List<SubscriberRow>, alwaysKeys: Set<String>): Boolean {
        if (alwaysKeys.isEmpty()) return true
        val seen = rows.mapTo(HashSet()) { it.pubkeyHex.lowercase() }
        return alwaysKeys.all { it.lowercase() in seen }
    }

    /** The account's reader key of a channel (hex) as the bound channels
     *  state holds it, or `null`; set by the session shell. */
    @Volatile
    public var accountReaderKeyOf: ((accountIdB64: String, channelIdHex: String) -> String?)? = null

    /** [accountReaderKeyOf] as the set a reseal takes. */
    public fun accountKeysFor(accountIdB64: String, channelIdHex: String): Set<String> =
        try {
            accountReaderKeyOf?.invoke(accountIdB64, channelIdHex)?.let { setOf(it) } ?: emptySet()
        } catch (_: Throwable) {
            emptySet()
        }

    /** [resealThrough] for [channelIdHex] as this device keeps it. Throws
     *  when the channel state is not reachable yet (the reseal stays owed
     *  and is retried), rather than resealing to every row for want of it. */
    public fun throughFor(accountIdB64: String, channelIdHex: String): Long {
        val context = SubscriberAdmission.appContext
            ?: throw IllegalStateException("the channel's admission state is not loaded yet")
        return resealThrough(ChannelAdmissionMark.get(context, accountIdB64, channelIdHex))
    }
}
