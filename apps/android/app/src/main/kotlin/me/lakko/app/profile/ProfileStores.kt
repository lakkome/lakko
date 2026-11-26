// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

/**
 * ADR-0048 §3 — the per-signature-key profile store and the per
 * `(key, username)` verification cache.
 *
 * ## Keyed on the signature key, never a leaf
 *
 * A profile received in a group is stored under the sender's Ed25519
 * signature key resolved from the MLS sender leaf AT RECEIPT (C-68) — the
 * caller hands in that key, never a leaf index and never a key the envelope
 * might name (the envelope names none). A profile from a contact request is
 * stored under `ContactRequest.from_pubkey`, which the request signature
 * authenticates. The key string is standard base64, the one spelling
 * [me.lakko.app.chat.MemberIdentity] and the accepted-contact store use.
 *
 * ## Strictly newer only
 *
 * A claim replaces the stored one only when the core's `profileIsNewer` says
 * so — strictly greater `updated_at_ms`; equal is not newer, so a re-broadcast
 * (every group republishes on growth) is a no-op rather than a write. The
 * order is injected so a host test runs without the `.so`.
 *
 * Both stores are pure over a [ProfilePersistence] port, so the ordering and
 * keying rules are testable in a plain JVM.
 */

/**
 * Durable backing for the two stores, ONE set per account.
 *
 * What a person learned about other people's names belongs to the signed-in
 * account, not to the device: until 2026-09-30 the profiles and verification
 * results lived under one device-wide key, so a second account signed in on
 * the same phone resolved names the first account had learned. The stores ask
 * [currentAccount] on every operation and reload when it changed; with no
 * account a read is empty and a write is dropped.
 *
 * Reads return an empty map on any decode failure; writes are best-effort.
 */
public interface ProfilePersistence {
    /** The account the stores act on now, or `null` when none is signed in. */
    public fun currentAccount(): String?

    public fun loadProfiles(account: String): Map<String, PeerProfile>
    public fun saveProfiles(account: String, profiles: Map<String, PeerProfile>)
    public fun loadVerifications(account: String): Map<String, UsernameVerification>
    public fun saveVerifications(account: String, verifications: Map<String, UsernameVerification>)
}

/**
 * A [ProfilePersistence] that keeps its maps in memory, per account (tests,
 * and a missing Context). [account] is the account the stores act on; tests
 * switch it to model a second account signing in on the same device.
 */
public class InMemoryProfilePersistence(
    @Volatile public var account: String? = DEFAULT_ACCOUNT,
) : ProfilePersistence {
    private val profiles = HashMap<String, Map<String, PeerProfile>>()
    private val verifications = HashMap<String, Map<String, UsernameVerification>>()

    override fun currentAccount(): String? = account

    override fun loadProfiles(account: String): Map<String, PeerProfile> =
        synchronized(this) { profiles[account] ?: emptyMap() }

    override fun saveProfiles(account: String, profiles: Map<String, PeerProfile>) {
        synchronized(this) { this.profiles[account] = profiles }
    }

    override fun loadVerifications(account: String): Map<String, UsernameVerification> =
        synchronized(this) { verifications[account] ?: emptyMap() }

    override fun saveVerifications(account: String, verifications: Map<String, UsernameVerification>) {
        synchronized(this) { this.verifications[account] = verifications }
    }

    public companion object {
        /** The one account a single-account test runs as. */
        public const val DEFAULT_ACCOUNT: String = "test-account"
    }
}

/** Outcome of offering a received profile to [PeerProfileStore.offer]. */
public enum class ProfileOffer {
    /** Stored: no profile was held for the key, or this one is strictly newer. */
    STORED,

    /** Dropped: the held profile is as new or newer. */
    NOT_NEWER,

    /** Dropped: no signature key to bind it to (C-68 — never stored unbound). */
    UNATTRIBUTED,

    /** Dropped: no account is signed in, so there is no one to file it for. */
    NO_ACCOUNT,
}

/** The per-signature-key profile store. Thread-safe. */
public class PeerProfileStore(
    private val persistence: ProfilePersistence,
    private val isNewer: (incoming: ULong, stored: ULong) -> Boolean =
        { incoming, stored -> uniffi.lakko_ffi_uniffi.profileIsNewer(incoming, stored) },
) {
    private val lock = Any()
    private var profiles: Map<String, PeerProfile> = emptyMap()

    /** The account [profiles] was loaded for; [UNLOADED] before the first read. */
    private var loadedFor: String? = UNLOADED

    /** The current account's map, reloaded when the account changed. Call
     *  under [lock]. */
    private fun current(): Map<String, PeerProfile> {
        val account = persistence.currentAccount()
        if (account != loadedFor) {
            profiles = account?.let { persistence.loadProfiles(it) } ?: emptyMap()
            loadedFor = account
        }
        return profiles
    }

    /** The profile held for [signatureKeyB64], or `null`. */
    public fun get(signatureKeyB64: String): PeerProfile? =
        synchronized(lock) { current()[signatureKeyB64] }

    /** Every held profile (a snapshot). */
    public fun all(): Map<String, PeerProfile> = synchronized(lock) { current() }

    /** Signature key -> account id hex, for every held profile that has one
     *  (the ADR-0021 call-roster sources). */
    public fun accountIdsByKey(): Map<String, String> = synchronized(lock) {
        current().mapNotNull { (key, p) -> p.accountIdHex?.let { key to it } }.toMap()
    }

    /**
     * Offer a received [profile] from [signatureKeyB64]. Stores it only when
     * nothing is held or it is strictly newer than what is.
     *
     * A newer profile that carries no account id keeps the id already held for
     * the key: the id names the account, which a rename does not change, and
     * dropping it would take the person's name off their call tile.
     */
    public fun offer(signatureKeyB64: String?, profile: PeerProfile): ProfileOffer {
        if (signatureKeyB64.isNullOrEmpty()) return ProfileOffer.UNATTRIBUTED
        synchronized(lock) {
            current()
            // No account signed in: nothing to file it under.
            val account = loadedFor ?: return ProfileOffer.NO_ACCOUNT
            val held = profiles[signatureKeyB64]
            // An account that never set a name publishes at stamp 0, level
            // with a name found for it: theirs still replaces that. A profile
            // saying nothing does not, or the row would read `Member` again.
            val replacesFoundName = held?.isFoundName == true && !profile.isFoundName &&
                (profile.displayName != null || profile.username != null)
            if (held != null && !replacesFoundName && !isNewer(profile.updatedAtMs, held.updatedAtMs)) {
                // The same profile sent again (a growth or request answer
                // carries the stamp it was set at) is not newer, but it can
                // carry an account id the held row never had: a row filed
                // before the envelope carried one. Take the id and nothing
                // else, or the call roster can never name a person whose
                // profile predates the field.
                val learned = profile.accountIdHex
                if (held.accountIdHex != null || learned == null ||
                    held.updatedAtMs != profile.updatedAtMs
                ) {
                    return ProfileOffer.NOT_NEWER
                }
                profiles = profiles + (
                    signatureKeyB64 to held.copy(
                        accountIdHex = learned,
                        envelopeB64 = held.envelopeB64 ?: profile.envelopeB64,
                    )
                    )
                persistence.saveProfiles(account, profiles)
                return ProfileOffer.STORED
            }
            val merged = if (profile.accountIdHex == null && held?.accountIdHex != null) {
                profile.copy(accountIdHex = held.accountIdHex)
            } else {
                profile
            }
            profiles = profiles + (signatureKeyB64 to merged)
            persistence.saveProfiles(account, profiles)
            return ProfileOffer.STORED
        }
    }

    /** Drop everything held for [account] (forget device). Other accounts'
     *  profiles stay. */
    public fun clear(account: String) {
        synchronized(lock) {
            persistence.saveProfiles(account, emptyMap())
            if (loadedFor == account) profiles = emptyMap()
        }
    }
}

/** Marks a store that has not read its persistence yet (never an account). */
private const val UNLOADED: String = "\u0000unloaded"

/** The per `(signature key, username)` verification cache. Thread-safe. */
public class UsernameVerificationStore(private val persistence: ProfilePersistence) {
    private val lock = Any()
    private var results: Map<String, UsernameVerification> = emptyMap()
    private var loadedFor: String? = UNLOADED

    /** The current account's results, reloaded when the account changed.
     *  Call under [lock]. */
    private fun current(): Map<String, UsernameVerification> {
        val account = persistence.currentAccount()
        if (account != loadedFor) {
            results = account?.let { persistence.loadVerifications(it) } ?: emptyMap()
            loadedFor = account
        }
        return results
    }

    /** The cached result for the pair, or `null` when never verified. */
    public fun get(signatureKeyB64: String, username: String): UsernameVerification? =
        synchronized(lock) { current()[pairKey(signatureKeyB64, username)] }

    /** Record a definitive result for the pair. Dropped with no account
     *  signed in. */
    public fun put(signatureKeyB64: String, username: String, result: UsernameVerification) {
        synchronized(lock) {
            current()
            val account = loadedFor ?: return
            results = results + (pairKey(signatureKeyB64, username) to result)
            persistence.saveVerifications(account, results)
        }
    }

    /** Drop everything held for [account] (forget device). Other accounts'
     *  results stay. */
    public fun clear(account: String) {
        synchronized(lock) {
            persistence.saveVerifications(account, emptyMap())
            if (loadedFor == account) results = emptyMap()
        }
    }

    public companion object {
        /** The cache key. `|` is outside both the base64 alphabet and the
         *  username character set, so the pair cannot be ambiguous. */
        public fun pairKey(signatureKeyB64: String, username: String): String =
            "$signatureKeyB64|$username"
    }
}
