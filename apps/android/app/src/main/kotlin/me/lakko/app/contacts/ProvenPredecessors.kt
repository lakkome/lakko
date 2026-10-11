// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/*
 * ADR-0066 section 3.5 ("Peers") - the proven-predecessor rule.
 *
 * An account whose published key changed WITH a supersession proof that
 * verified under the pinned key rotated its keys and kept its devices. The
 * fail-closed gate ([ContactsClient.verifyBundleAccountKey]) re-pins silently,
 * as ADR-0016 always did, and from then on the old key is that account's
 * PROVEN PREDECESSOR: the conversation leaves that exist stay bound to it
 * (Stage A moves none of them) and are the same person. Nothing is evicted,
 * no chat closes, nothing is flagged and nothing is shown.
 *
 * A key change that nothing proved (a reset, or anything the gate refused)
 * NEVER gets this treatment: a link is created by [provenLinkFor] alone, and
 * that takes the gate's verdict. A later reset of the same account ends every
 * predecessor it had ([withoutLinksOf]).
 *
 * The same rule serves this account's OWN previous key on its own devices
 * (the links are then recorded by the rotation, which signed the proof
 * itself).
 *
 * Everything here is pure; the store holds public keys only.
 */

/** One proven step of an account's key history. Standard base64 keys. */
@Serializable
public data class ProvenLink(
    /** The account whose key changed (standard base64 of its 16-byte id). */
    public val accountIdB64: String,
    /** The key the proof was signed by. */
    public val predecessorKeyB64: String,
    /** The key the proof authorises. */
    public val successorKeyB64: String,
    /** When this device accepted the proof (unix ms). */
    public val provenAtMs: Long,
)

/** How the gate judged a changed key. */
public enum class KeyChangeEvidence {
    /** A supersession proof by the pinned key over exactly the served key
     *  verified. */
    ProofVerified,

    /** The key changed and nothing proved it (a reset, a stripped proof). */
    Unproven,

    /** A proof was served and did not verify, or named another key. */
    ProofRefused,
}

/**
 * The link a key change earns, or `null`: ONLY a verified proof makes the
 * old key a predecessor. An unproven or refused change never does, whatever
 * else is known about the account.
 */
public fun provenLinkFor(
    evidence: KeyChangeEvidence,
    accountIdB64: String,
    oldKeyB64: String,
    newKeyB64: String,
    nowMs: Long,
): ProvenLink? {
    if (evidence != KeyChangeEvidence.ProofVerified) return null
    if (oldKeyB64.isEmpty() || newKeyB64.isEmpty() || oldKeyB64 == newKeyB64) return null
    return ProvenLink(accountIdB64, oldKeyB64, newKeyB64, nowMs)
}

/**
 * [links] with [link] added. Refused (the list is returned unchanged) when it
 * would make a key its own ancestor, give a key a second successor, or link
 * keys that already belong to ANOTHER account: a key history is a chain per
 * account, never a graph.
 */
public fun withProvenLink(links: List<ProvenLink>, link: ProvenLink): List<ProvenLink> {
    if (link.predecessorKeyB64 == link.successorKeyB64) return links
    if (links.any { it == link.copy(provenAtMs = it.provenAtMs) }) return links
    val foreign = links.any {
        it.accountIdB64 != link.accountIdB64 && (
            it.predecessorKeyB64 in setOf(link.predecessorKeyB64, link.successorKeyB64) ||
                it.successorKeyB64 in setOf(link.predecessorKeyB64, link.successorKeyB64)
            )
    }
    if (foreign) return links
    if (links.any { it.predecessorKeyB64 == link.predecessorKeyB64 }) return links
    // A cycle: the new successor already leads (forward) to the predecessor.
    if (currentKeyOf(link.successorKeyB64, links) == link.predecessorKeyB64 ||
        earliestKeyOf(link.predecessorKeyB64, links) == link.successorKeyB64
    ) {
        return links
    }
    return links + link
}

/** [links] without any step of [accountIdB64]: the account's keys were
 *  RESET, so no earlier key of it is a predecessor any more (R.7). */
public fun withoutLinksOf(links: List<ProvenLink>, accountIdB64: String): List<ProvenLink> =
    links.filterNot { it.accountIdB64 == accountIdB64 }

/** The key [keyB64] leads to through proven steps (itself when it has no
 *  successor). Bounded, so a damaged store cannot spin. */
public fun currentKeyOf(keyB64: String, links: List<ProvenLink>): String {
    var key = keyB64
    repeat(MAX_CHAIN) {
        key = links.firstOrNull { it.predecessorKeyB64 == key }?.successorKeyB64 ?: return key
    }
    return key
}

/** The first key of the chain [keyB64] is on (itself when nothing precedes
 *  it): the key existing conversation leaves and a contact row are under. */
public fun earliestKeyOf(keyB64: String, links: List<ProvenLink>): String {
    var key = keyB64
    repeat(MAX_CHAIN) {
        key = links.firstOrNull { it.successorKeyB64 == key }?.predecessorKeyB64 ?: return key
    }
    return key
}

/** Every key of the chain [keyB64] is on, earliest first. */
public fun keysOfPerson(keyB64: String, links: List<ProvenLink>): List<String> {
    val out = ArrayList<String>()
    var key: String? = earliestKeyOf(keyB64, links)
    while (key != null && out.size <= MAX_CHAIN && key !in out) {
        out += key
        key = links.firstOrNull { it.predecessorKeyB64 == key }?.successorKeyB64
    }
    return out
}

/**
 * Whether two account keys are the same person: equal, or joined by PROVEN
 * steps only. Two keys that nothing proved related are two people, even when
 * they were once published under one account id.
 */
public fun samePerson(aKeyB64: String, bKeyB64: String, links: List<ProvenLink>): Boolean =
    aKeyB64 == bKeyB64 || currentKeyOf(aKeyB64, links) == currentKeyOf(bKeyB64, links)

/**
 * Whether [earlierKeyB64] is a proven PREDECESSOR of [laterKeyB64]: a
 * different key whose proven steps lead to it. Never true across an
 * unproven change, and never for the key itself.
 */
public fun keyPrecedes(earlierKeyB64: String, laterKeyB64: String, links: List<ProvenLink>): Boolean {
    if (earlierKeyB64 == laterKeyB64) return false
    val chain = keysOfPerson(earlierKeyB64, links)
    val earlier = chain.indexOf(earlierKeyB64)
    val later = chain.indexOf(laterKeyB64)
    return earlier >= 0 && later > earlier
}

/**
 * Whether a device binding under [boundKeyB64] is one of the account whose
 * key is [accountKeyB64] now (section 3.5 "Kept devices' directory rows"):
 * the pinned key, or a proven predecessor of it.
 */
public fun bindingIsOfAccount(boundKeyB64: String, accountKeyB64: String, links: List<ProvenLink>): Boolean =
    boundKeyB64 == accountKeyB64 || currentKeyOf(boundKeyB64, links) == accountKeyB64

/**
 * The name-resolution aliases a set of proven steps yields (later key ->
 * the key the person was first known by), so a profile, a rename or a row
 * stored under the earlier key still names the person under the later one.
 */
public fun provenAliases(links: List<ProvenLink>): Map<String, String> =
    links.associate { it.successorKeyB64 to earliestKeyOf(it.predecessorKeyB64, links) }

private const val MAX_CHAIN: Int = 64

/** Where the proven steps live, per signed-in account. */
public interface ProvenLinkStore {
    public fun load(ownerAccountIdB64: String): List<ProvenLink>

    public fun save(ownerAccountIdB64: String, links: List<ProvenLink>)

    public fun clearForOwner(ownerAccountIdB64: String)
}

/** Record one proven step; a no-op for a step [withProvenLink] refuses. */
public fun ProvenLinkStore.record(ownerAccountIdB64: String, link: ProvenLink): Boolean {
    val held = load(ownerAccountIdB64)
    val next = withProvenLink(held, link)
    if (next === held || next == held) return false
    save(ownerAccountIdB64, next)
    return true
}

/** In-memory store (tests). */
public class InMemoryProvenLinkStore : ProvenLinkStore {
    private val rows = HashMap<String, List<ProvenLink>>()

    override fun load(ownerAccountIdB64: String): List<ProvenLink> = rows[ownerAccountIdB64].orEmpty()

    override fun save(ownerAccountIdB64: String, links: List<ProvenLink>) {
        rows[ownerAccountIdB64] = links
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        rows.remove(ownerAccountIdB64)
    }
}

/** Production store: plain `SharedPreferences` (public keys and ids only). */
public class SharedPrefsProvenLinkStore(context: Context) : ProvenLinkStore {
    private val appContext = context.applicationContext

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(ownerAccountIdB64: String): List<ProvenLink> =
        try {
            prefs().getString(KEY_PREFIX + ownerAccountIdB64, null)?.let { json.decodeFromString(LIST, it) }.orEmpty()
        } catch (_: Throwable) {
            emptyList()
        }

    override fun save(ownerAccountIdB64: String, links: List<ProvenLink>) {
        try {
            prefs().edit().putString(KEY_PREFIX + ownerAccountIdB64, json.encodeToString(LIST, links)).apply()
        } catch (_: Throwable) {
            // The step then lives until the next fetch re-proves it.
        }
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        try {
            prefs().edit().remove(KEY_PREFIX + ownerAccountIdB64).apply()
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-proven-predecessors"
        private const val KEY_PREFIX: String = "links-v1:"
        private val json = Json { ignoreUnknownKeys = true }
        private val LIST = ListSerializer(ProvenLink.serializer())
    }
}

/**
 * The process's view of the proven steps: the gate records into it, and the
 * surfaces that resolve a person by key ([ContactsRepository]) are told.
 * Holds nothing but the listener.
 */
public object ProvenPredecessors {
    /** Run after a step was recorded or dropped (any thread). */
    @Volatile
    public var onChanged: (() -> Unit)? = null

    /** The gate accepted a proof: [oldKeyB64] is now a proven predecessor
     *  of [newKeyB64] for [accountIdB64]. */
    public fun recordProven(
        store: ProvenLinkStore,
        ownerAccountIdB64: String,
        accountIdB64: String,
        oldKeyB64: String,
        newKeyB64: String,
        nowMs: Long = System.currentTimeMillis(),
    ) {
        val link = provenLinkFor(KeyChangeEvidence.ProofVerified, accountIdB64, oldKeyB64, newKeyB64, nowMs) ?: return
        if (store.record(ownerAccountIdB64, link)) notifyChanged()
    }

    /** The account's keys were RESET: none of its earlier keys is a
     *  predecessor any more. */
    public fun dropAccount(store: ProvenLinkStore, ownerAccountIdB64: String, accountIdB64: String) {
        val held = store.load(ownerAccountIdB64)
        val next = withoutLinksOf(held, accountIdB64)
        if (next.size == held.size) return
        store.save(ownerAccountIdB64, next)
        notifyChanged()
    }

    private fun notifyChanged() {
        try {
            onChanged?.invoke()
        } catch (_: Throwable) {
            // A listener never breaks the gate.
        }
    }
}
