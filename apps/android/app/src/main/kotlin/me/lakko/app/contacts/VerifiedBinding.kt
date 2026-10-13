// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0073 R.2 - the VERIFIED BINDING of an account this device never
 * pinned, and the one rule that decides what a group member's roster key is
 * worth against the directory.
 *
 * The acceptance rule (`ResetVerdict.kt`) judges a change from "the key this
 * device HOLDS for the account". For a contact that is the pin. For a group
 * member this device never pinned the account id comes from the member's
 * OWN profile envelope, and a member can write any account id there. If the
 * roster key were taken as the held key on that claim alone, a member could
 * name ANOTHER account's id; once that account ever reset, an admin's client
 * would read "member key -> that account's key" as a reset, mark the member
 * superseded, propose its removal and add the other account's devices in its
 * place.
 *
 * So a profile's claimed id establishes nothing. What does is a binding this
 * device saw for itself: `(account id -> account key)` is recorded ONLY when
 * this device itself fetched that account id's bundle through the fail-closed
 * gate and the fetched key EQUALLED the roster key. The reset path for a
 * non-pinned member runs only from such a binding, and only when it names
 * exactly the member's roster key.
 *
 * A pinned account never reaches this file: the pin is the verified binding.
 * An accepted reset and a proven rotation both write a pin, after which the
 * binding is dropped. Never carried to another device. Mirror of
 * `apps/web/src/contacts/verified-binding.ts` (R.12).
 */

/** One verified binding. */
@Serializable
public data class VerifiedBinding(
    /** The account key the directory served for the account id, standard
     *  base64, equal to the roster key at the moment it was recorded. */
    val keyB64: String,
    /** The bundle's `key_generation` at that moment: the baseline a later
     *  reset must be strictly above (R.2 rule 2). */
    val keyGeneration: Long,
    /** When this device recorded it, UNIX milliseconds. */
    val verifiedAtMs: Long,
)

/** What a group member's roster key is worth for one account id. */
public sealed interface MemberHeldVerdict {
    /** The account is pinned: the pin decides and the roster key is ignored. */
    public data object Pin : MemberHeldVerdict

    /** No binding, and the directory serves exactly the roster key: record
     *  the binding now. A first observation, never a reset. */
    public data object Record : MemberHeldVerdict

    /** The binding names the roster key and the directory still serves it. */
    public data object Confirmed : MemberHeldVerdict

    /** The binding names the roster key and the directory serves ANOTHER
     *  key: a change from a key this device verified, judged from the
     *  binding exactly as from a pin. */
    public data class Held(val heldKeyB64: String, val acceptedGeneration: Long) : MemberHeldVerdict

    /** Nothing is established for this roster key under this account id. */
    public data object None : MemberHeldVerdict
}

/**
 * | pinned | binding | binding names the roster key | directory serves the roster key | verdict     |
 * |--------|---------|------------------------------|---------------------------------|-------------|
 * | yes    | -       | -                            | -                               | `Pin`       |
 * | no     | none    | -                            | yes                             | `Record`    |
 * | no     | none    | -                            | no                              | `None`      |
 * | no     | held    | yes                          | yes                             | `Confirmed` |
 * | no     | held    | yes                          | no                              | `Held`      |
 * | no     | held    | no                           | any                             | `None`      |
 *
 * Row 3 is the attack this file exists for (a member claiming another
 * account's id) and also the honest first sight of a member whose account
 * already moved on: neither is told apart here, and neither is acted on.
 */
public fun memberHeldKey(
    pinned: Boolean,
    binding: VerifiedBinding?,
    /** The PERSON key the roster names for the member, standard base64. */
    rosterKeyB64: String,
    /** The key the directory serves for the account id right now. */
    fetchedKeyB64: String,
): MemberHeldVerdict {
    if (pinned) return MemberHeldVerdict.Pin
    if (binding == null) {
        return if (fetchedKeyB64 == rosterKeyB64) MemberHeldVerdict.Record else MemberHeldVerdict.None
    }
    if (binding.keyB64 != rosterKeyB64) return MemberHeldVerdict.None
    if (fetchedKeyB64 == rosterKeyB64) return MemberHeldVerdict.Confirmed
    return MemberHeldVerdict.Held(binding.keyB64, binding.keyGeneration)
}

private fun VerifiedBinding.wellFormed(): Boolean = keyB64.isNotEmpty() && keyGeneration >= 0

/** Where verified bindings live, per signed-in account. */
public interface VerifiedBindingStore {
    public fun read(ownerAccountIdB64: String, accountIdB64: String): VerifiedBinding?

    /** WRITE-ONCE: a held binding is never replaced (a change from it is a
     *  rotation or a reset, which ends in a pin). Returns whether written. */
    public fun record(ownerAccountIdB64: String, accountIdB64: String, binding: VerifiedBinding): Boolean

    /** Drop a binding (the account is pinned now: the pin is the binding). */
    public fun drop(ownerAccountIdB64: String, accountIdB64: String)

    public fun clearForOwner(ownerAccountIdB64: String)
}

/** In-memory store (tests, context-less configurations). */
public class InMemoryVerifiedBindingStore : VerifiedBindingStore {
    private val rows = HashMap<String, MutableMap<String, VerifiedBinding>>()

    override fun read(ownerAccountIdB64: String, accountIdB64: String): VerifiedBinding? =
        synchronized(rows) { rows[ownerAccountIdB64]?.get(accountIdB64) }

    override fun record(ownerAccountIdB64: String, accountIdB64: String, binding: VerifiedBinding): Boolean =
        synchronized(rows) {
            if (!binding.wellFormed()) return false
            val mine = rows.getOrPut(ownerAccountIdB64) { HashMap() }
            if (mine.containsKey(accountIdB64)) return false
            mine[accountIdB64] = binding
            true
        }

    override fun drop(ownerAccountIdB64: String, accountIdB64: String) {
        synchronized(rows) { rows[ownerAccountIdB64]?.remove(accountIdB64) }
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        synchronized(rows) { rows.remove(ownerAccountIdB64) }
    }
}

private val bindingJson = Json { ignoreUnknownKeys = true }
private val bindingMapSerializer = MapSerializer(String.serializer(), VerifiedBinding.serializer())

/** The stored row as JSON. Visible for the codec test. */
internal fun encodeVerifiedBindings(all: Map<String, VerifiedBinding>): String =
    bindingJson.encodeToString(bindingMapSerializer, all)

/** The inverse; a junk row reads as "no binding" (the fail-closed
 *  direction: nothing is judged from it). */
internal fun decodeVerifiedBindings(raw: String?): Map<String, VerifiedBinding> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        bindingJson.decodeFromString(bindingMapSerializer, raw).filterValues { it.wellFormed() }
    } catch (_: Exception) {
        emptyMap()
    }
}

/** Production store: plain preferences (public keys and account ids only),
 *  one row per signed-in account. */
public class SharedPrefsVerifiedBindingStore(context: Context) : VerifiedBindingStore {
    private val appContext = context.applicationContext
    private val lock = Any()

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun load(owner: String): Map<String, VerifiedBinding> =
        try {
            decodeVerifiedBindings(prefs().getString(KEY_PREFIX + owner, null))
        } catch (_: Throwable) {
            emptyMap()
        }

    override fun read(ownerAccountIdB64: String, accountIdB64: String): VerifiedBinding? =
        load(ownerAccountIdB64)[accountIdB64]

    override fun record(ownerAccountIdB64: String, accountIdB64: String, binding: VerifiedBinding): Boolean =
        synchronized(lock) {
            if (!binding.wellFormed()) return false
            val all = load(ownerAccountIdB64)
            if (all.containsKey(accountIdB64)) return false
            try {
                prefs().edit().putString(KEY_PREFIX + ownerAccountIdB64, encodeVerifiedBindings(all + (accountIdB64 to binding))).apply()
                true
            } catch (_: Throwable) {
                false
            }
        }

    override fun drop(ownerAccountIdB64: String, accountIdB64: String) {
        synchronized(lock) {
            val all = load(ownerAccountIdB64)
            if (!all.containsKey(accountIdB64)) return
            try {
                prefs().edit().putString(KEY_PREFIX + ownerAccountIdB64, encodeVerifiedBindings(all - accountIdB64)).apply()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    override fun clearForOwner(ownerAccountIdB64: String) {
        try {
            prefs().edit().remove(KEY_PREFIX + ownerAccountIdB64).apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-verified-bindings"
        private const val KEY_PREFIX = "bindings-v1:"
    }
}
