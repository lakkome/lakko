// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage

/*
 * ADR-0101 section 7 - a channel comes back to a person who reset their keys.
 * Mirror of `apps/web/src/channels/channel-return.ts`.
 *
 * ## What travels
 *
 * A channel return (`composeChannelReturnEnvelope`) in a sealed mailbox row
 * (kind CHANNEL_RETURN) to the reset person's CONTINUITY mailbox: the channel
 * id, the role the sender says the person had, the OLD member card's account
 * binding, the newest epoch seeds the sender holds (at most 64) and,
 * optionally, the newest profile it holds for the person. It also carries an
 * INVITATION CARD: for a subscriber the channel's invitation card as "invite a
 * contact" mints it; for a publisher the one-use PUBLISHER card of "Add
 * publisher", whose request the owner's device approves without asking.
 *
 * An OWNER is never returned by anyone (rewritten 2026-11-26): the owner's
 * device takes its channels back by itself from the account's own list
 * (`OwnedChannels.kt`), with no subscriber or publisher taking part. The
 * core's decoder refuses the owner role, and no return is planned for an
 * owner.
 *
 * ## Who sends what ([channelReturnsFor])
 *
 * | this device                        | the reset account was           | sends role   |
 * |------------------------------------|---------------------------------|--------------|
 * | owner or publisher of the channel  | a subscriber with a member card | SUBSCRIBER   |
 * | an owner device                    | a publisher                     | PUBLISHER    |
 *
 * Each (channel, reset account, role) is sent once per reset generation.
 *
 * ## What the reset device does ([channelReturnAction])
 *
 * It verifies the old card binding against its own previous account key.
 * Only a verified return is acted on without a prompt. An unverified one that
 * carries an invitation is SHOWN as the ordinary channel invitation card,
 * never accepted silently and never dropped; one without an invitation is
 * dropped. Knowing the account id is not enough to subscribe someone.
 */

/** The roles. */
public enum class ChannelReturnRoleName { SUBSCRIBER, PUBLISHER }

/** What the reset device does with one decoded return. */
public enum class ChannelReturnAction {
    /** Subscribe with the carried invitation card, then add the seeds. */
    SUBSCRIBE,

    /** Accept the carried publisher invitation (the ordinary request, which
     *  the owner's device approves), then add the seeds. */
    ACCEPT_PUBLISHER,

    /** Already in the channel in that role: only the seeds are added. */
    ADD_SEEDS,

    /** Not verified, but it carries an invitation: shown as an ordinary
     *  invitation for the person to decide on. */
    SHOW_INVITATION,

    /** Nothing to do. */
    DROP,
}

/**
 * The reset device's rule over one decoded return.
 *
 * | verified | role       | invitation | already in that role | action           |
 * |----------|------------|------------|----------------------|------------------|
 * | no       | any        | yes        | -                    | SHOW_INVITATION  |
 * | no       | any        | no         | -                    | DROP             |
 * | yes      | subscriber | -          | yes                  | ADD_SEEDS        |
 * | yes      | subscriber | yes        | no                   | SUBSCRIBE        |
 * | yes      | subscriber | no         | no                   | DROP             |
 * | yes      | publisher  | yes        | no                   | ACCEPT_PUBLISHER |
 * | yes      | publisher  | otherwise  |                      | ADD_SEEDS        |
 */
public fun channelReturnAction(
    verified: Boolean,
    role: ChannelReturnRoleName,
    hasInvitation: Boolean,
    alreadyInRole: Boolean,
): ChannelReturnAction {
    if (!verified) return if (hasInvitation) ChannelReturnAction.SHOW_INVITATION else ChannelReturnAction.DROP
    return when (role) {
        ChannelReturnRoleName.SUBSCRIBER -> when {
            alreadyInRole -> ChannelReturnAction.ADD_SEEDS
            hasInvitation -> ChannelReturnAction.SUBSCRIBE
            else -> ChannelReturnAction.DROP
        }
        ChannelReturnRoleName.PUBLISHER ->
            if (hasInvitation && !alreadyInRole) ChannelReturnAction.ACCEPT_PUBLISHER else ChannelReturnAction.ADD_SEEDS
    }
}

/** One member card this device opened, as the peer side needs it. */
public data class ReturnCard(
    /** Standard base64 of the account identity key the card opens to. */
    val identityPkB64: String,
    /** Standard base64 of the 32-byte subscriber key it was uploaded under. */
    val subscriberPkB64: String,
)

/** This device's role in a channel. */
public enum class ReturnOwnRole { OWNER, PUBLISHER, SUBSCRIBER }

/** One channel this device is in, as the peer side needs it. */
public data class ReturnChannel(
    val idHex: String,
    val ownRole: ReturnOwnRole,
    val cards: List<ReturnCard>,
    /** Standard base64 of the owner's account key, when known. */
    val ownerKeyB64: String?,
    /** Standard base64 of the publishers' account keys (owner excluded). */
    val publisherKeysB64: List<String>,
)

/** One return to send. */
public data class PlannedReturn(
    val idHex: String,
    val role: ChannelReturnRoleName,
    /** The reset account's id (standard base64). */
    val accountIdB64: String,
    /** The card the return carries the binding of. */
    val card: ReturnCard,
)

/**
 * The returns this device owes, over its channels and the resets it
 * accepted ([superseded]: every retired key -> its account id). A card is
 * matched on its identity key; nothing is sent for this account's own keys.
 */
public fun channelReturnsFor(
    channels: List<ReturnChannel>,
    superseded: Map<String, String>,
    ownKeysB64: Set<String>,
): List<PlannedReturn> {
    val out = ArrayList<PlannedReturn>()
    for (ch in channels) {
        // An owner is never returned: it takes its channels back by itself
        // from its own list (ADR-0101 section 7, rewritten 2026-11-26).
        val owner = ch.ownerKeyB64
        if (ch.ownRole == ReturnOwnRole.SUBSCRIBER) continue
        val publishers = ch.publisherKeysB64.toSet()
        for (card in ch.cards) {
            if (card.identityPkB64 in ownKeysB64 || card.identityPkB64 == owner) continue
            val acct = superseded[card.identityPkB64] ?: continue
            if (card.identityPkB64 in publishers) {
                // A publisher's new devices are admitted by an owner device only.
                if (ch.ownRole == ReturnOwnRole.OWNER) {
                    out += PlannedReturn(ch.idHex, ChannelReturnRoleName.PUBLISHER, acct, card)
                }
                continue
            }
            out += PlannedReturn(ch.idHex, ChannelReturnRoleName.SUBSCRIBER, acct, card)
        }
    }
    return out
}

/** The key a sent return is recorded under. */
public fun sentReturnKey(r: PlannedReturn): String = "${r.idHex}|${r.role.name.lowercase()}|${r.accountIdB64}"

/** The planned returns not yet sent for the accepted generation of their
 *  account ([generationOf]: `null` when the account has none, then nothing
 *  is sent). */
public fun unsentReturns(
    planned: List<PlannedReturn>,
    sent: Map<String, Long>,
    generationOf: (accountIdB64: String) -> Long?,
): List<PlannedReturn> = planned.filter { r ->
    val gen = generationOf(r.accountIdB64) ?: return@filter false
    (sent[sentReturnKey(r)] ?: -1L) < gen
}

/** The newest seeds a return carries. */
public const val CHANNEL_RETURN_MAX_SEEDS: Int = 64

/** One held seed. */
public class HeldSeed(public val epoch: UInt, public val seed: ByteArray)

/** The newest [CHANNEL_RETURN_MAX_SEEDS] of the held seeds, ascending by
 *  epoch, one per epoch, each 32 bytes. */
public fun newestSeeds(seeds: List<HeldSeed>): List<HeldSeed> {
    val unique = java.util.TreeMap<UInt, ByteArray>()
    for (s in seeds) if (s.seed.size == 32) unique[s.epoch] = s.seed
    return unique.entries.toList().takeLast(CHANNEL_RETURN_MAX_SEEDS).map { HeldSeed(it.key, it.value) }
}

/**
 * Live walk 2026-11-26, D1 - the channels whose own member card this device
 * keeps current app-wide: every channel it publishes (owner and publisher
 * alike), never a removed one. A channel this device only reads uploads its card from the
 * channel view. Distinct, in the order given. Pure.
 */
public fun channelsOwingPublishingCard(
    channels: List<String>,
    publishesHere: (String) -> Boolean,
    removed: (String) -> Boolean,
): List<String> = channels.distinct().filter { publishesHere(it) && !removed(it) }

/** The owned channels still owing a recovery-key registration, sorted.
 *  [registered] is the set registered from the owned-channels list key (a
 *  channel registered under the removed scheme owes one again). */
public fun channelsOwingRecoveryKey(owned: Collection<String>, registered: Set<String>): List<String> =
    owned.filter { it !in registered }.distinct().sorted()

// ----------------------------------------------------------------------------
// Stores
// ----------------------------------------------------------------------------

private val storeJson = Json { ignoreUnknownKeys = true }
private val stringList = ListSerializer(String.serializer())

/** The returns read from the continuity mailbox and not handled yet
 *  (standard base64 of each envelope), oldest first. ENCRYPTED file: a
 *  return carries epoch seeds and an invitation card. */
public class HeldReturnsStore(private val storage: MarkerStorage) {

    public fun read(account: String): List<String> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyList()
        return runCatching { storeJson.decodeFromString(stringList, raw) }.getOrElse { emptyList() }
    }

    /** Hold one more (a duplicate is held once). Throws when storage refuses,
     *  which holds the mailbox cursor: the row is read again. */
    public fun hold(account: String, envelopeB64: String) {
        val held = read(account).filter { it != envelopeB64 }.toMutableList()
        held += envelopeB64
        while (held.size > MAX_HELD_RETURNS) held.removeAt(0)
        me.lakko.app.account.AccountStoreGate.writeOrEnd(account) {
            storage.set(KEY_PREFIX + account, storeJson.encodeToString(stringList, held))
        }
    }

    /** Drop handled returns. Never throws. */
    public fun drop(account: String, handled: Collection<String>) {
        val done = handled.toSet()
        val left = read(account).filter { it !in done }
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                if (left.isEmpty()) storage.remove(KEY_PREFIX + account)
                else storage.set(KEY_PREFIX + account, storeJson.encodeToString(stringList, left))
            }
        } catch (_: Throwable) {
            // Handled again next pass; every action is idempotent.
        }
    }

    public companion object {
        public const val KEY_PREFIX: String = "channel-returns:"

        /** How many returns are held at most (chosen). */
        public const val MAX_HELD_RETURNS: Int = 256

        public fun onDevice(context: Context): HeldReturnsStore =
            HeldReturnsStore(me.lakko.app.auth.ResetReturnStorage.secret(context))
    }
}

/** What was sent, per signed-in account: `idHex|role|acct` -> generation. */
public class SentReturnsStore(private val storage: MarkerStorage) {
    private val serializer = MapSerializer(String.serializer(), Long.serializer())

    public fun read(account: String): Map<String, Long> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptyMap()
        return runCatching { storeJson.decodeFromString(serializer, raw) }.getOrElse { emptyMap() }
    }

    public fun write(account: String, sent: Map<String, Long>) {
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, storeJson.encodeToString(serializer, sent))
            }
        } catch (_: Throwable) {
            // Sent again next pass; the receiver handles a return twice the same.
        }
    }

    public companion object {
        public const val KEY_PREFIX: String = "channel-returns-sent:"

        public fun onDevice(context: Context): SentReturnsStore =
            SentReturnsStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}

/** The owned channels whose recovery key landed, per signed-in account. */
public class RecoveryRegisteredStore(private val storage: MarkerStorage) {

    public fun read(account: String): Set<String> {
        val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull() ?: return emptySet()
        return runCatching { storeJson.decodeFromString(stringList, raw).toSet() }.getOrElse { emptySet() }
    }

    public fun mark(account: String, idHex: String) {
        val next = (read(account) + idHex).sorted()
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, storeJson.encodeToString(stringList, next))
            }
        } catch (_: Throwable) {
            // Registered again next pass: the PUT is idempotent.
        }
    }

    public companion object {
        /** `v2`: the key derived from the owned-channels list key (ADR-0101
         *  section 7 item 5). The rows of the removed scheme
         *  ([LEGACY_KEY_PREFIX]) are not read: every channel made before the
         *  change registers again at its next pass. */
        public const val KEY_PREFIX: String = "channel-recovery-registered-v2:"

        /** The removed scheme's rows, wiped with the account. */
        public const val LEGACY_KEY_PREFIX: String = "channel-recovery-registered:"

        public fun onDevice(context: Context): RecoveryRegisteredStore =
            RecoveryRegisteredStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}
